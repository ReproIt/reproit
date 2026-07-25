use super::*;

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

    history.runs = run;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_json::to_vec_pretty(&history)?)?;

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
        )
        .unwrap();
        assert_eq!(r1["counts"]["new"], 2);
        assert_eq!(r1["counts"]["fixed"], 0);

        // Run 2: fp1 persists, fp2 is fixed (absent, opB covered).
        let r2 = classify_and_record(&dir, &[finding("fp1", "opA")], &covered).unwrap();
        assert_eq!(r2["counts"]["new"], 0);
        assert_eq!(r2["counts"]["persisting"], 1);
        assert_eq!(r2["counts"]["fixed"], 1);

        // Run 3: fp2 comes back -> regressed; fp1 still persisting.
        let r3 = classify_and_record(
            &dir,
            &[finding("fp1", "opA"), finding("fp2", "opB")],
            &covered,
        )
        .unwrap();
        assert_eq!(r3["counts"]["regressed"], 1);
        assert_eq!(r3["counts"]["persisting"], 1);

        // A finding whose operation was NOT covered is not falsely marked fixed.
        let narrow: BTreeSet<String> = ["opA"].iter().map(|s| s.to_string()).collect();
        let r4 = classify_and_record(&dir, &[finding("fp1", "opA")], &narrow).unwrap();
        assert_eq!(
            r4["counts"]["fixed"], 0,
            "opB uncovered, so fp2 is not fixed"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
