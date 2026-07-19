use super::*;

pub(super) fn equivalent_findings_key(findings: &[Value]) -> String {
    let mut signatures: Vec<String> = findings.iter().map(finding_signature).collect();
    signatures.sort();
    signatures.dedup();
    signatures.join("\n")
}

pub(super) fn reserve_shrink_representative(
    seen: &mut std::collections::BTreeSet<String>,
    findings: &[Value],
) -> bool {
    seen.insert(equivalent_findings_key(findings))
}

pub(super) fn batch_completed(log: &str, plans: &[SeedPlan]) -> bool {
    if !log.lines().any(|line| line.trim() == "JOURNEY DONE") {
        return false;
    }
    let ended: std::collections::BTreeSet<u64> = log
        .lines()
        .filter_map(|line| marker_seed(line, "SEED:END "))
        .collect();
    ended.is_empty() || plans.iter().all(|plan| ended.contains(&plan.seed))
}

/// A stable cross-locale signature for a finding: `<oracle>:<kind>:<message>`.
/// Used to tell "the same finding showed up in another locale" from "only here"
/// so the locale loop can flag locale-specific i18n findings.
pub(crate) fn finding_signature(f: &Value) -> String {
    let oracle = crate::crosscut::classify(f).as_str();
    let invariant = f
        .get("invariant")
        .and_then(Value::as_str)
        .unwrap_or("exception");
    let kind = f.get("kind").and_then(Value::as_str).unwrap_or("?");
    let message = f.get("message").and_then(Value::as_str).unwrap_or("");
    // The top stack frame (the crash LOCATION) makes this a robust bug-bucket
    // key: two walks that reach the same crash by different paths share it, while
    // same-message crashes at different code locations stay distinct.
    let frame = f
        .get("frames")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(Value::as_str)
        .unwrap_or("");
    // Crashes bucket on the exact message + top frame (the crash LOCATION): two
    // walks that reach the same crash by different paths share it, same-message
    // crashes at different code locations stay distinct. For NON-crash oracles the
    // message carries run/locale-varying detail ("3 overflowing elements", "jank
    // 54.5%", a localized label) that must NOT split one defect into many buckets,
    // so we key on a normalized message: digit runs -> `#`, quoted labels -> `<q>`.
    let trigger = ["root_trigger", "trigger", "element", "selector", "sig"]
        .iter()
        .find_map(|key| f.get(*key).and_then(Value::as_str))
        .unwrap_or("");
    if oracle == "crash" {
        format!("{oracle}:{invariant}:{kind}:{message}:{frame}:{trigger}")
    } else {
        format!(
            "{oracle}:{invariant}:{kind}:{}:{frame}:{trigger}",
            normalize_message(message)
        )
    }
}

/// Apply the normal invariant/exception pipeline to an aggregate runner log.
/// Multi-actor exploration concatenates every actor log and uses this adapter
/// so it has exactly the same oracle identity as ordinary fuzz and shrink.
pub(crate) fn finding_signatures_for_log(cfg: &Config, log: &str) -> BTreeSet<String> {
    let findings = findings_from_log(cfg, log, true, Default::default());
    let (confirmed, _) = crate::crosscut::OracleFilter::stable().apply(findings);
    confirmed
        .iter()
        .filter(|finding| {
            !finding
                .get("advisory")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .map(finding_signature)
        .collect()
}

/// Stable application identity used to keep findings from different targets
/// distinct without incorporating machine-local paths or run timestamps.
pub(super) fn target_identity(cfg: &Config) -> String {
    let app = &cfg.app;
    let target = app
        .url
        .as_deref()
        .or((!app.bundle_id.is_empty()).then_some(app.bundle_id.as_str()))
        .or(app.executable.as_deref())
        .or((!app.project_dir.is_empty()).then_some(app.project_dir.as_str()))
        .unwrap_or("default");
    format!("{}:{}", app.platform.trim(), target.trim())
}

/// Collapse run/locale-varying detail in a finding message so the same defect
/// buckets to one signature: every digit run (counts, percentages, px,
/// decimals) becomes `#`, and every quoted run (a localized label) becomes
/// `<q>`.
pub(crate) fn normalize_message(message: &str) -> String {
    let mut out = String::with_capacity(message.len());
    let mut chars = message.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' | '\'' => {
                let q = c;
                for n in chars.by_ref() {
                    if n == q {
                        break;
                    }
                }
                out.push_str("<q>");
            }
            d if d.is_ascii_digit() => {
                out.push('#');
                while matches!(
                    chars.peek(),
                    Some(n) if n.is_ascii_digit() || *n == '.' || *n == ','
                ) {
                    chars.next();
                }
            }
            _ => out.push(c),
        }
    }
    out
}

/// A short human label for a bug bucket (oracle + kind + first line of the
/// message), for the `--all` unique-bugs summary.
pub(super) fn finding_label(f: &Value) -> String {
    let oracle = crate::crosscut::classify(f).as_str();
    let kind = f.get("kind").and_then(Value::as_str).unwrap_or("?");
    let message = f
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("")
        .lines()
        .next()
        .unwrap_or("");
    let message = if message.len() > 80 {
        format!("{}…", &message[..80])
    } else {
        message.to_string()
    };
    if message.is_empty() {
        format!("{oracle}:{kind}")
    } else {
        format!("{oracle}: {message}")
    }
}

/// Exception records not produced by the test framework itself.
pub(crate) fn app_exceptions(run_dir: &Path) -> Vec<Value> {
    std::fs::read_to_string(run_dir.join("exceptions.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| {
            !v.get("kind")
                .and_then(Value::as_str)
                .unwrap_or("")
                .contains("TEST FRAMEWORK")
        })
        .collect()
}

/// Perf oracle: the run's frame summary (manifest) exceeding the jank
/// threshold is a finding too. Discovered the hard way: the bug zoo's
/// jank-loop fired at 54.5% jank and the exception-only oracle shrugged.
pub(super) fn perf_findings(run_dir: &Path) -> Vec<Value> {
    let Ok(manifest) = std::fs::read_to_string(run_dir.join("manifest.json")) else {
        return vec![];
    };
    let Ok(m) = serde_json::from_str::<Value>(&manifest) else {
        return vec![];
    };
    let mut out = Vec::new();
    for d in m
        .get("devices")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(f) = d.get("frames") else { continue };
        let jank = f.get("jank_pct").and_then(Value::as_f64).unwrap_or(0.0);
        if jank > JANK_PCT_MAX {
            out.push(serde_json::json!({
                "kind": "PERF",
                "message": format!(
                    "jank {jank:.1}% (threshold {JANK_PCT_MAX}%), p90 build {:.1}ms, worst {:.0}ms",
                    f.get("p90_build_ms").and_then(Value::as_f64).unwrap_or(0.0),
                    f.get("worst_ms").and_then(Value::as_f64).unwrap_or(0.0),
                ),
                "frames": [],
            }));
        }
    }
    out
}

pub(super) fn all_findings(run_dir: &Path) -> Vec<Value> {
    let mut f = app_exceptions(run_dir);
    f.extend(perf_findings(run_dir));
    f
}

/// Build the observation bundle the INVARIANTS oracle evaluates: this seed's
/// parsed state graph (EXPLORE:STATE/EDGE), the already-parsed exception
/// findings, and the tier. Per-state jank and a non-exception leak signal are
/// sim-tier inputs we do not have per-seed in the headless log, so they are
/// left empty here (no-jank then reports nothing headless, as documented).
/// The session-wide sim jank is still surfaced by `perf_findings`.
fn invariant_observations(
    mut obs: crate::model::map::RunObs,
    exceptions: Vec<Value>,
    sim: bool,
    escapable_route_labels: crate::model::map::EscapableRoutes,
) -> crate::model::invariants::Observations {
    obs.escapable_route_labels = escapable_route_labels;
    crate::model::invariants::Observations {
        obs,
        exceptions,
        jank_by_sig: std::collections::BTreeMap::new(),
        leak_signal: None,
        sim,
    }
}

/// route -> the label sets of states the AGGREGATE map can leave via a forward
/// (non-back) action. Folded into each per-seed permission-trap evaluation so a
/// state on an escapable page is not flagged as a sink just because one sparse
/// seed recorded no exit from it (the animated single-page-app false positive).
/// The label set (recovered from the state description, which is its first
/// labels) lets the oracle suppress only a same-or-reduced render of the
/// escapable page, not a distinct screen that merely shares the URL.
pub(super) fn map_escapable_routes(
    map: &crate::model::appmap::AppMap,
) -> crate::model::map::EscapableRoutes {
    let mut out = std::collections::BTreeMap::<
        String,
        std::collections::BTreeSet<std::collections::BTreeSet<String>>,
    >::new();
    for t in &map.transitions {
        if matches!(t.action, crate::model::appmap::Action::Back) || t.from == t.to {
            continue;
        }
        if let Some(state) = map.states.get(&t.from) {
            if let Some(route) = state.signature.route.as_ref() {
                let labels: std::collections::BTreeSet<String> = state
                    .description
                    .split(", ")
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .collect();
                out.entry(route.clone()).or_default().insert(labels);
            }
        }
    }
    std::sync::Arc::new(out)
}

/// The category of a finding: its named invariant id when present, else its
/// `kind`, else "exception". Shrink minimizes toward the SAME category that was
/// originally discovered.
pub(super) fn finding_category(f: &Value) -> String {
    f.get("invariant")
        .and_then(Value::as_str)
        .or_else(|| f.get("kind").and_then(Value::as_str))
        .unwrap_or("exception")
        .to_string()
}

/// Shrink-targeting severity of a finding category.
fn category_severity(_cat: &str) -> u8 {
    1
}

/// Exact finding identities shrink must preserve: only the MOST-SEVERE among
/// the originals. Identity includes oracle, invariant/kind, normalized symptom,
/// first-party frame, structural trigger, and state signature via
/// `finding_signature`. A different bug from the same oracle never satisfies
/// the confirmation/shrink gate.
pub(super) fn shrink_target(findings: &[Value]) -> std::collections::BTreeSet<String> {
    let Some(top) = findings
        .iter()
        .map(|f| category_severity(&finding_category(f)))
        .max()
    else {
        return std::collections::BTreeSet::new();
    };
    findings
        .iter()
        .filter(|f| category_severity(&finding_category(f)) == top)
        .map(finding_signature)
        .collect()
}

/// The PRIMARY finding to headline. Stable: keeps the first finding among
/// equal-severity ties, preserving discovery order.
pub(super) fn primary_finding(findings: &[Value]) -> Option<&Value> {
    findings.iter().reduce(|best, f| {
        if category_severity(&finding_category(f)) > category_severity(&finding_category(best)) {
            f
        } else {
            best
        }
    })
}

/// Does this candidate reproduce the exact original failure identity? A second
/// finding from the same oracle/category is deliberately insufficient.
pub(super) fn reproduces_original(
    candidate: &[Value],
    want: &std::collections::BTreeSet<String>,
) -> bool {
    if want.is_empty() {
        return !candidate.is_empty();
    }
    candidate
        .iter()
        .any(|f| want.contains(&finding_signature(f)))
}

/// The shared crawl -> per-state-findings core, given an already-read drive log
/// plus the exceptions parsed for it. Runs the log's state graph + exceptions
/// through the INVARIANTS oracle (built-in + custom) and folds the app
/// exceptions back in when `no-exception` is disabled. This is the one place
/// the invariant evaluation lives; `findings_for_tier` (a whole run dir), the
/// per-seed fuzz loop (a session segment), and `scan` all funnel through it,
/// differing only in where the log/exceptions/escapable set come from and how
/// perf is attributed. `escapable` is the pool of routes any walk could leave
/// via a forward action, so a permission trap is only flagged when NO evidence
/// escapes it (the per-seed loop pools across batches; single-finding re-verify
/// passes an empty set).
pub(super) fn findings_from_log(
    cfg: &Config,
    log: &str,
    sim: bool,
    escapable: crate::model::map::EscapableRoutes,
) -> Vec<Value> {
    let parsed = crate::model::runner::ParsedRun::new(
        log,
        &[],
        !cfg.contracts.is_empty(),
        cfg.backend.enabled,
    );
    findings_from_parsed(
        cfg,
        parsed.map,
        parsed.exceptions,
        sim,
        escapable,
        &parsed.observations,
        &parsed.backend,
    )
}

pub(super) fn findings_from_parsed(
    cfg: &Config,
    obs: crate::model::map::RunObs,
    exceptions: Vec<Value>,
    sim: bool,
    escapable: crate::model::map::EscapableRoutes,
    observations: &[crate::model::observation::Observation],
    backend_events: &[crate::model::backend::BackendEvent],
) -> Vec<Value> {
    let inv_obs = invariant_observations(obs, exceptions.clone(), sim, escapable);
    let mut f = crate::model::invariants::evaluate(&inv_obs, &cfg.invariants);
    if !cfg.invariants.no_exception {
        f.extend(exceptions);
    }
    if !cfg.contracts.is_empty() {
        f.extend(
            crate::model::contracts::evaluate_all(&cfg.contracts, observations)
                .iter()
                .map(crate::model::contracts::finding),
        );
    }
    if cfg.backend.enabled {
        f.extend(
            crate::model::backend::evaluate(&cfg.backend, backend_events)
                .iter()
                .map(crate::model::backend::finding),
        );
    }
    f
}

/// Findings for a run, by tier, run through the INVARIANTS oracle so a shrink
/// replay is judged by the SAME named invariants that discovered the finding
/// (a graph/label/exception invariant must reproduce, not just exceptions).
/// The simulator tier writes a structured exceptions.jsonl + a frames manifest
/// (perf), so `all_findings` supplies the exception+perf inputs; the HEADLESS
/// tier (flutter test) parses exceptions from the drive log. Per-state jank is
/// sim-only, surfaced separately via perf_findings.
pub(super) fn findings_for_tier(cfg: &Config, run_dir: &Path, sim: bool) -> Vec<Value> {
    let log = std::fs::read_to_string(run_dir.join("drive-a.log")).unwrap_or_default();
    let parsed = crate::model::runner::ParsedRun::new(
        &log,
        &[],
        !cfg.contracts.is_empty(),
        cfg.backend.enabled,
    );
    let contract_violations =
        crate::model::contracts::evaluate_all(&cfg.contracts, &parsed.observations);
    let _ = crate::model::contracts::write_evidence(
        &run_dir.join("contract-evidence.json"),
        &cfg.contracts,
        &parsed.observations,
        &contract_violations,
    );
    let backend_violations = crate::model::backend::evaluate(&cfg.backend, &parsed.backend);
    let _ = crate::model::backend::write_evidence(
        &run_dir.join("backend-evidence.json"),
        &cfg.backend,
        &parsed.backend,
        &backend_violations,
    );
    let exceptions = if sim {
        app_exceptions(run_dir)
    } else {
        parsed.exceptions
    };
    // The check path re-verifies a specific recorded finding without the
    // aggregate map in scope; an empty set keeps its permission-trap check
    // unchanged.
    let mut f = findings_from_parsed(
        cfg,
        parsed.map,
        exceptions,
        sim,
        Default::default(),
        &parsed.observations,
        &parsed.backend,
    );
    if sim {
        f.extend(perf_findings(run_dir));
    }
    f
}
