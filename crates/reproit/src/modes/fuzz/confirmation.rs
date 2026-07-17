use super::*;

#[allow(clippy::too_many_arguments)]
async fn capsule_candidate_reproduces(
    cfg: &Config,
    root: &Path,
    journey: &str,
    cfg_path: &PathBuf,
    defines: &[(String, String)],
    actions: &[String],
    sim: bool,
    want: &std::collections::BTreeSet<String>,
    candidate: &crate::capsule::Capsule,
) -> Result<bool> {
    let guard = candidate.materialize_candidate(root)?;
    let mut candidate_defines = defines.to_vec();
    candidate_defines.push((
        "REPROIT_CAPSULE".into(),
        guard.path().to_string_lossy().into_owned(),
    ));
    confirm_trace(
        cfg,
        root,
        journey,
        cfg_path,
        &candidate_defines,
        actions,
        sim,
        want,
    )
    .await
}

/// Joint network/payload half of minimization. Action ddmin has already run;
/// every candidate below starts from that minimal action trace and is accepted
/// only after an independent clean replay of the exact original finding.
#[allow(clippy::too_many_arguments)]
pub(super) async fn shrink_causal_capsule(
    cfg: &Config,
    root: &Path,
    journey: &str,
    cfg_path: &PathBuf,
    defines: &[(String, String)],
    actions: &[String],
    sim: bool,
    want: &std::collections::BTreeSet<String>,
    mut best: crate::capsule::Capsule,
    json_output: bool,
) -> Result<crate::capsule::Capsule> {
    let original_id = best.id.clone();
    let mut replays = 0usize;
    let mut i = 0;
    while i < best.exchanges.len() && replays < MAX_SHRINK_REPLAYS {
        let mut candidate = best.clone();
        candidate.exchanges.remove(i);
        replays += 1;
        if capsule_candidate_reproduces(
            cfg, root, journey, cfg_path, defines, actions, sim, want, &candidate,
        )
        .await?
        {
            best = candidate;
        } else {
            i += 1;
        }
    }
    for exchange_index in 0..best.exchanges.len() {
        if replays >= MAX_SHRINK_REPLAYS {
            break;
        }
        let Some(mut current) = best.exchanges[exchange_index].response_body.clone() else {
            continue;
        };
        loop {
            let mut accepted = None;
            for reduced in crate::capsule::json_reductions(&current) {
                if replays >= MAX_SHRINK_REPLAYS {
                    break;
                }
                let mut candidate = best.clone();
                candidate.exchanges[exchange_index].response_body = Some(reduced.clone());
                replays += 1;
                if capsule_candidate_reproduces(
                    cfg, root, journey, cfg_path, defines, actions, sim, want, &candidate,
                )
                .await?
                {
                    accepted = Some((candidate, reduced));
                    break;
                }
            }
            let Some((candidate, reduced)) = accepted else {
                break;
            };
            best = candidate;
            current = reduced;
        }
    }
    if !capsule_candidate_reproduces(
        cfg, root, journey, cfg_path, defines, actions, sim, want, &best,
    )
    .await?
    {
        anyhow::bail!("jointly minimized causal capsule failed final clean confirmation");
    }
    best.persist(root)?;
    if best.id != original_id {
        let _ = std::fs::remove_dir_all(crate::layout::capsule_dir(root, &original_id));
    }
    say(
        json_output,
        format!(
            "  capsule shrink: {} exchange(s), {replays} clean replay(s)",
            best.exchanges.len()
        ),
    );
    Ok(best)
}

/// Trust gate between an observation and a public finding. Replays the complete
/// observed trace in a fresh explorer session and accepts it only when the same
/// oracle/signature set fires. A failed confirmation is silently discarded by
/// the caller: it never receives a finding id, notification, or saved guard.
#[allow(clippy::too_many_arguments)]
pub(super) async fn confirm_trace(
    cfg: &Config,
    root: &Path,
    journey: &str,
    cfg_path: &PathBuf,
    defines: &[(String, String)],
    trace: &[String],
    sim: bool,
    want: &std::collections::BTreeSet<String>,
) -> Result<bool> {
    std::fs::write(cfg_path, json!({ "replay": trace }).to_string())?;
    Ok(
        match run_explorer(cfg, root, journey, true, defines, false, sim, false).await {
            Ok(outcome) => {
                reproduces_original(&findings_for_tier(cfg, &outcome.run_dir, sim), want)
            }
            Err(_) => false,
        },
    )
}

/// ddmin (Zeller & Hildebrand 2002): minimize a failing trace by removing
/// CHUNKS at decreasing granularity rather than one action at a time. Each
/// replay is an expensive device run, so we want the 1-minimal trace in
/// O(log n) replays, not O(n). Granularity starts at 2 (remove halves) and
/// doubles only when no chunk at the current granularity can be dropped.
#[allow(clippy::too_many_arguments)]
pub(super) async fn shrink(
    cfg: &Config,
    root: &Path,
    journey: &str,
    cfg_path: &PathBuf,
    defines: &[(String, String)],
    trace: Vec<String>,
    sim: bool,
    want: &std::collections::BTreeSet<String>,
    json: bool,
) -> Result<Vec<String>> {
    say(
        json,
        format!(
            "  ddmin shrinking from {} actions (cap {MAX_SHRINK_REPLAYS} replays), oracle: \
             reproduce [{}]",
            trace.len(),
            want.iter().cloned().collect::<Vec<_>>().join(", ")
        ),
    );
    // ZERO-ACTION test: a "broken on arrival" finding (an overflow / content bug
    // already present at load) needs NO action to reproduce. ddmin
    // floors at one action and never tries the empty replay, so without this it
    // keeps a meaningless leftover tap - often one that MISSES on replay - which
    // makes the repro and its recorded clip nonsensical (the HUD shows a phantom
    // action while the box sits on a load-state element). Test load-only FIRST: if
    // the SAME finding category fires with zero actions, that IS the minimal repro.
    // The reproduces_original category gate rejects an empty replay that trips a
    // different finding category.
    std::fs::write(
        cfg_path,
        json!({ "replay": Vec::<String>::new() }).to_string(),
    )?;
    let load_only_reproduces =
        match run_explorer(cfg, root, journey, true, defines, false, sim, false).await {
            Ok(o) => reproduces_original(&findings_for_tier(cfg, &o.run_dir, sim), want),
            Err(_) => false,
        };
    if load_only_reproduces {
        say(
            json,
            "    -[0..0): reproduces on load, repro is empty (0 actions)",
        );
        return Ok(Vec::new());
    }

    let mut current = trace;
    let mut granularity = 2usize;
    let mut replays = 1usize; // the zero-action probe above counts as one replay
    while current.len() >= 2 && replays < MAX_SHRINK_REPLAYS {
        let chunk = current.len().div_ceil(granularity);
        let mut removed_any = false;
        // Try removing each chunk (the "complement" subsets of ddmin).
        let mut start = 0;
        while start < current.len() && replays < MAX_SHRINK_REPLAYS {
            let end = (start + chunk).min(current.len());
            let candidate: Vec<String> = current[..start]
                .iter()
                .chain(current[end..].iter())
                .cloned()
                .collect();
            replays += 1;
            let reproduces = if candidate.is_empty() {
                false
            } else {
                std::fs::write(cfg_path, json!({ "replay": candidate }).to_string())?;
                // Shrink replays run on the SAME tier as the discovering run
                // (headless replays are deterministic with the sim path). A
                // candidate reproduces ONLY if it trips the SAME finding
                // category as the original.
                match run_explorer(cfg, root, journey, true, defines, false, sim, false).await {
                    Ok(o) => reproduces_original(&findings_for_tier(cfg, &o.run_dir, sim), want),
                    Err(_) => false,
                }
            };
            if reproduces {
                say(
                    json,
                    format!(
                        "    -[{start}..{end}): still reproduces ({} actions)",
                        candidate.len()
                    ),
                );
                current = candidate;
                removed_any = true;
                granularity = granularity.max(2); // reset toward fine
                break;
            }
            start += chunk;
        }
        if !removed_any {
            if granularity >= current.len() {
                break; // 1-minimal at this point
            }
            granularity = (granularity * 2).min(current.len());
        }
    }
    say(
        json,
        format!("  shrunk to {} actions in {replays} replays", current.len()),
    );
    // Truncate a CRASH repro at the action that fires the exception. Everything
    // after the crash is unnecessary to reproduce it, and a repro that ENDS at
    // its trigger keeps trigger_index == len == the crash point, so a guard-style
    // fix (one that stops the crash) replays cleanly UP TO that point and is
    // judged Green/PASS. Without this, the trailing post-crash actions, which the
    // fix often makes unreachable, look like a pre-trigger miss and the fixed
    // repro is misclassified STALE. One replay of the minimized trace locates the
    // crash; the truncated trace still reproduces (the crash fires at its end).
    if want.iter().any(|c| is_crash_category(c)) && current.len() >= 2 {
        std::fs::write(cfg_path, json!({ "replay": current }).to_string())?;
        if let Ok(o) = run_explorer(cfg, root, journey, true, defines, false, sim, false).await {
            let log = std::fs::read_to_string(o.run_dir.join("drive-a.log")).unwrap_or_default();
            if let Some(n0) = crash_trigger_index(&log) {
                // Back the cut off any TRAILING fragile actions to the last KEYED
                // tap at/before the crash. A `pageerror` is async, so the logged
                // crash position can land a step past the action that caused it
                // (often an unkeyed error-overlay button); ending a repro on a
                // positional `role:...#idx` (or `back`) makes it misclassify STALE
                // after a fix, because that index shifts. A keyed action survives.
                let mut n = n0.min(current.len());
                while n >= 1 && !is_keyed_action(&current[n - 1]) {
                    n -= 1;
                }
                if (1..current.len()).contains(&n) {
                    // Re-verify the keyed-truncated trace still reproduces from
                    // cold before adopting it; keep the longer trace otherwise.
                    let candidate: Vec<String> = current[..n].to_vec();
                    std::fs::write(cfg_path, json!({ "replay": candidate }).to_string())?;
                    let still =
                        match run_explorer(cfg, root, journey, true, defines, false, sim, false)
                            .await
                        {
                            Ok(o2) => {
                                reproduces_original(&findings_for_tier(cfg, &o2.run_dir, sim), want)
                            }
                            Err(_) => false,
                        };
                    if still {
                        current = candidate;
                        say(
                            json,
                            format!("  truncated to {n} actions at the crash (keyed)"),
                        );
                    }
                }
            }
        }
    }
    Ok(current)
}

/// True for the crash/exception finding category (the invariant id or kind a
/// thrown app exception is recorded under).
fn is_crash_category(cat: &str) -> bool {
    cat == "no-exception" || cat == "exception"
}

/// Whether an action targets a stable DEVELOPER KEY (`tap:key:...` /
/// `type:key:...`) rather than a positional `role:...#idx` selector or a `back`
/// navigation. Keyed actions survive layout changes; positional ones shift.
pub(super) fn is_keyed_action(action: &str) -> bool {
    let sel = action
        .strip_prefix("tap:")
        .or_else(|| action.strip_prefix("type:"))
        .unwrap_or(action);
    sel.starts_with("key:")
}

/// The 1-based action count at which a replay log first fired an app exception:
/// the number of `FUZZ:ACT` lines up to and including the one that produced the
/// `EXCEPTION CAUGHT BY` block. None if the log has no exception (e.g. a graph
/// finding, which is not truncated).
pub(super) fn crash_trigger_index(log: &str) -> Option<usize> {
    let mut acts = 0usize;
    for line in log.lines() {
        if line.contains("FUZZ:ACT ") {
            acts += 1;
        }
        if line.contains("EXCEPTION CAUGHT BY") {
            return Some(acts.max(1));
        }
    }
    None
}
