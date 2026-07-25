use super::*;
use crate::interface::junit;

/// Run-over-run finding lifecycle. Findings are identified by their stable
/// content-hash fingerprint, so the same defect reappearing across runs is the
/// same finding. Each run's findings are classified new / persisting / regressed
/// / fixed against a per-project history, so a CI gate can block on *new or
/// regressed* findings and a run can say "N new, N regressed since last run".
#[derive(Default, Serialize, Deserialize)]
struct History {
    #[serde(default)]
    runs: u64,
    #[serde(default)]
    findings: BTreeMap<String, HistoryEntry>,
}

#[derive(Serialize, Deserialize)]
struct HistoryEntry {
    operation: String,
    /// `active` (currently reproducing) or `fixed` (previously seen, gone).
    status: String,
    first_seen_run: u64,
    last_seen_run: u64,
}

fn history_path(root: &Path) -> PathBuf {
    layout::reproit_dir(root).join("backend-history.json")
}

fn item(fingerprint: &str, operation: &str) -> Value {
    json!({ "fingerprint": fingerprint, "operation": operation })
}

/// Classify this run's findings against the stored history, update the history,
/// and return the lifecycle summary. `covered_ops` are the operation ids this run
/// exercised: a previously-active finding becomes `fixed` only when its operation
/// was actually re-exercised, so reduced coverage (a run that reached fewer
/// endpoints) can never fake a fix. Absence within a covered run is a fresh
/// reproduction attempt that did not reproduce.
pub(super) fn classify_and_record(
    root: &Path,
    findings: &[Value],
    covered_ops: &BTreeSet<String>,
    record: bool,
) -> Result<Value> {
    let path = history_path(root);
    let mut history: History = std::fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    let run = history.runs + 1;

    let mut current = BTreeMap::new();
    for finding in findings {
        if let Some(fingerprint) = finding.get("fingerprint").and_then(Value::as_str) {
            let operation = finding
                .get("operation")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            current.insert(fingerprint.to_string(), operation);
        }
    }

    let mut new = Vec::new();
    let mut persisting = Vec::new();
    let mut regressed = Vec::new();
    let mut fixed = Vec::new();

    for (fingerprint, operation) in &current {
        match history.findings.get(fingerprint) {
            None => new.push(item(fingerprint, operation)),
            Some(entry) if entry.status == "fixed" => regressed.push(item(fingerprint, operation)),
            Some(_) => persisting.push(item(fingerprint, operation)),
        }
        let entry = history
            .findings
            .entry(fingerprint.clone())
            .or_insert_with(|| HistoryEntry {
                operation: operation.clone(),
                status: "active".to_string(),
                first_seen_run: run,
                last_seen_run: run,
            });
        entry.status = "active".to_string();
        entry.operation = operation.clone();
        entry.last_seen_run = run;
    }

    for (fingerprint, entry) in history.findings.iter_mut() {
        if entry.status == "active"
            && !current.contains_key(fingerprint)
            && covered_ops.contains(&entry.operation)
        {
            entry.status = "fixed".to_string();
            fixed.push(item(fingerprint, &entry.operation));
        }
    }

    // Read-only classification (the CI gate) leaves the stored baseline
    // untouched, so re-running a failing PR is stable; only a recording run (a
    // normal scan/fuzz, or `check --update-baseline`) advances the baseline.
    if record {
        history.runs = run;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, serde_json::to_vec_pretty(&history)?)?;
    }

    let counts = json!({
        "new": new.len(),
        "persisting": persisting.len(),
        "regressed": regressed.len(),
        "fixed": fixed.len(),
    });
    Ok(json!({
        "run": run,
        "new": new,
        "persisting": persisting,
        "regressed": regressed,
        "fixed": fixed,
        "counts": counts,
    }))
}

/// The CI gate's exit decision, given this run's lifecycle, whether the run was
/// complete, and how many operations were inconclusive (rate-limited). Blocks on
/// new-or-regressed findings and fails closed on inconclusive operations, so a
/// run that could not evaluate part of the surface never renders as a pass.
pub(super) fn gate_outcome(
    ctx: &Ctx,
    lifecycle: &Value,
    complete: bool,
    inconclusive: usize,
    root: &Path,
) -> ExitCode {
    if let Some(path) = std::env::var_os("REPROIT_GATE_JUNIT") {
        write_gate_junit(Path::new(&path), lifecycle);
    }
    let counts = lifecycle.get("counts");
    let new = counts.and_then(|c| c["new"].as_u64()).unwrap_or(0);
    let regressed = counts.and_then(|c| c["regressed"].as_u64()).unwrap_or(0);
    let (accepted, expired) = apply_accepts(ctx, lifecycle, root);
    let new = new.saturating_sub(accepted);
    if std::env::var_os("REPROIT_GATE_BASELINE").is_some() {
        if inconclusive > 0 {
            ctx.say(format!(
                "refusing to record a baseline: {inconclusive} operation(s) inconclusive \
                 (rate-limited); re-run with backoff or fewer identities"
            ));
            return Exit::Regression.code();
        }
        ctx.say("baseline recorded; the gate now blocks on new or regressed findings".to_string());
        return ExitCode::SUCCESS;
    }
    // An expired accept is not a pass: it blocks again, loudly, which is what
    // makes the expiry meaningful rather than decorative.
    let blocking = new + regressed + expired;
    // The breakdown must add up to the blocking count, or the line reads as a
    // bug in the gate rather than as a lapsed exception.
    let mut breakdown = format!("{new} new, {regressed} regressed");
    if expired > 0 {
        breakdown.push_str(&format!(", {expired} expired acceptance"));
    }
    if inconclusive > 0 {
        ctx.say(format!(
            "gate: {blocking} blocking ({breakdown}), \
             {inconclusive} inconclusive (rate-limited): failing closed"
        ));
    } else {
        ctx.say(format!("gate: {blocking} blocking ({breakdown})"));
    }
    if complete && blocking == 0 {
        ExitCode::SUCCESS
    } else {
        Exit::Regression.code()
    }
}

/// Emit a JUnit report for a gated run: each new/regressed finding is a failing
/// testcase, each persisting one a passing testcase, so CI surfaces exactly what
/// a merge would newly introduce.
pub(super) fn write_gate_junit(path: &Path, lifecycle: &Value) {
    let mut cases = Vec::new();
    let mut push = |category: &str, passed: bool| {
        if let Some(items) = lifecycle.get(category).and_then(Value::as_array) {
            for item in items {
                let operation = item["operation"].as_str().unwrap_or("");
                let fingerprint = item["fingerprint"].as_str().unwrap_or("");
                cases.push(junit::Case {
                    name: format!("{operation} [{category}]"),
                    passed,
                    time_s: 0.0,
                    message: format!("{category} finding {fingerprint} on {operation}"),
                });
            }
        }
    };
    push("new", false);
    push("regressed", false);
    push("persisting", true);
    if let Err(error) = junit::write(path, "reproit-gate", &cases) {
        eprintln!(
            "warn: could not write gate junit {}: {error}",
            path.display()
        );
    }
}

/// Subtract per-finding accepts from this run's blocking findings.
///
/// Returns (accepted, expired). Accepted findings are reported by name and
/// reason so a passing gate still says what it is carrying: an exception nobody
/// can see is indistinguishable from a bug nobody found.
fn apply_accepts(ctx: &Ctx, lifecycle: &Value, root: &Path) -> (u64, u64) {
    let accepted_store = accept::load(root);
    let today = accept::today();
    let mut seen = BTreeSet::new();
    let mut accepted = 0u64;
    let mut expired = 0u64;
    for category in ["new", "regressed"] {
        for item in lifecycle[category].as_array().into_iter().flatten() {
            let Some(fingerprint) = item["fingerprint"].as_str() else {
                continue;
            };
            let operation = item["operation"].as_str().unwrap_or("");
            seen.insert(fingerprint.to_string());
            match accepted_store.verdict(fingerprint, &today) {
                accept::Verdict::None => {}
                accept::Verdict::Accepted(reason) => {
                    // Regressed findings are deliberately NOT auto-accepted: a
                    // finding that was fixed and came back is new information
                    // about the code, not the known issue that was accepted.
                    if category == "new" {
                        accepted += 1;
                        ctx.say(format!("  accepted {operation}: {reason}"));
                    }
                }
                accept::Verdict::Expired(on) => {
                    expired += 1;
                    ctx.say(format!(
                        "  acceptance EXPIRED on {on} for {operation}: blocking again"
                    ));
                }
            }
        }
    }
    for (fingerprint, entry) in accepted_store.stale(&seen) {
        ctx.say(format!(
            "  stale acceptance for {} ({}): the finding no longer reproduces, \
             remove it with `reproit accept --remove`",
            entry.operation,
            &fingerprint[..fingerprint.len().min(12)]
        ));
    }
    (accepted, expired)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(fingerprint: &str, operation: &str) -> Value {
        json!({ "fingerprint": fingerprint, "operation": operation })
    }

    #[test]
    fn tracks_new_persisting_regressed_and_fixed_across_runs() {
        let dir = std::env::temp_dir().join(format!("reproit-history-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let covered: BTreeSet<String> = ["opA", "opB"].iter().map(|s| s.to_string()).collect();

        // Run 1: two findings, both new.
        let r1 = classify_and_record(
            &dir,
            &[finding("fp1", "opA"), finding("fp2", "opB")],
            &covered,
            true,
        )
        .unwrap();
        assert_eq!(r1["counts"]["new"], 2);
        assert_eq!(r1["counts"]["fixed"], 0);

        // Run 2: fp1 persists, fp2 is fixed (absent, opB covered).
        let r2 = classify_and_record(&dir, &[finding("fp1", "opA")], &covered, true).unwrap();
        assert_eq!(r2["counts"]["new"], 0);
        assert_eq!(r2["counts"]["persisting"], 1);
        assert_eq!(r2["counts"]["fixed"], 1);

        // Run 3: fp2 comes back -> regressed; fp1 still persisting.
        let r3 = classify_and_record(
            &dir,
            &[finding("fp1", "opA"), finding("fp2", "opB")],
            &covered,
            true,
        )
        .unwrap();
        assert_eq!(r3["counts"]["regressed"], 1);
        assert_eq!(r3["counts"]["persisting"], 1);

        // A finding whose operation was NOT covered is not falsely marked fixed.
        let narrow: BTreeSet<String> = ["opA"].iter().map(|s| s.to_string()).collect();
        let r4 = classify_and_record(&dir, &[finding("fp1", "opA")], &narrow, true).unwrap();
        assert_eq!(
            r4["counts"]["fixed"], 0,
            "opB uncovered, so fp2 is not fixed"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
