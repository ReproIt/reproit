use super::*;

const MAX_ENVIRONMENT_REPLAYS: usize = 8;
const MAX_ENVIRONMENT_DIMENSIONS: usize = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReplayTrial {
    Reproduced,
    NotReproduced,
    Abstain,
}

#[allow(clippy::too_many_arguments)]
async fn capsule_candidate_trial(
    cfg: &Config,
    root: &Path,
    journey: &str,
    cfg_path: &PathBuf,
    defines: &[(String, String)],
    excluded_defines: &[String],
    warm: bool,
    sim: bool,
    want: &std::collections::BTreeSet<String>,
    candidate: &crate::capsule::Capsule,
) -> Result<ReplayTrial> {
    let guard = candidate.materialize_candidate(root)?;
    let mut candidate_defines = defines.to_vec();
    candidate_defines.push((
        "REPROIT_CAPSULE".into(),
        guard.path().to_string_lossy().into_owned(),
    ));
    confirm_trace_trial(
        cfg,
        root,
        journey,
        cfg_path,
        &candidate_defines,
        excluded_defines,
        &candidate.replay_actions(),
        warm,
        sim,
        want,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn capsule_candidate_reproduces(
    cfg: &Config,
    root: &Path,
    journey: &str,
    cfg_path: &PathBuf,
    defines: &[(String, String)],
    sim: bool,
    want: &std::collections::BTreeSet<String>,
    candidate: &crate::capsule::Capsule,
) -> Result<bool> {
    Ok(capsule_candidate_trial(
        cfg,
        root,
        journey,
        cfg_path,
        defines,
        &[],
        true,
        sim,
        want,
        candidate,
    )
    .await?
        == ReplayTrial::Reproduced)
}

/// Dependency-aware reduction operates on the capsule's causal DAG. Removing
/// an action atomically removes hard-dependent responses and backend events;
/// every accepted graph and payload reduction independently replays the exact
/// original finding.
#[allow(clippy::too_many_arguments)]
pub(super) async fn shrink_causal_capsule(
    cfg: &Config,
    root: &Path,
    journey: &str,
    cfg_path: &PathBuf,
    defines: &[(String, String)],
    sim: bool,
    want: &std::collections::BTreeSet<String>,
    mut best: crate::capsule::Capsule,
    json_output: bool,
) -> Result<crate::capsule::Capsule> {
    let original_id = best.id.clone();
    let mut replays = 0usize;
    best = shrink_causal_nodes(
        cfg,
        root,
        journey,
        cfg_path,
        defines,
        sim,
        want,
        best,
        &mut replays,
    )
    .await?;
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
                candidate.refresh_causal_graph()?;
                replays += 1;
                if capsule_candidate_reproduces(
                    cfg, root, journey, cfg_path, defines, sim, want, &candidate,
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
    if !capsule_candidate_reproduces(cfg, root, journey, cfg_path, defines, sim, want, &best)
        .await?
    {
        anyhow::bail!("jointly minimized causal capsule failed final clean confirmation");
    }
    best = minimize_environment(
        cfg,
        root,
        journey,
        cfg_path,
        defines,
        sim,
        want,
        best,
        json_output,
    )
    .await?;
    best.persist(root)?;
    if best.id != original_id {
        let _ = std::fs::remove_dir_all(crate::layout::capsule_dir(root, &original_id));
    }
    say(
        json_output,
        format!(
            "  causal shrink: {} node(s), {} action(s), {} exchange(s), {replays} clean replay(s)",
            best.causal_graph.nodes.len(),
            best.actions.len(),
            best.exchanges.len(),
        ),
    );
    Ok(best)
}

#[allow(clippy::too_many_arguments)]
async fn shrink_causal_nodes(
    cfg: &Config,
    root: &Path,
    journey: &str,
    cfg_path: &PathBuf,
    defines: &[(String, String)],
    sim: bool,
    want: &std::collections::BTreeSet<String>,
    mut best: crate::capsule::Capsule,
    replays: &mut usize,
) -> Result<crate::capsule::Capsule> {
    let mut granularity = 2usize;
    loop {
        let units = best.causal_graph.reduction_nodes();
        if units.is_empty() || *replays >= MAX_SHRINK_REPLAYS {
            break;
        }
        let chunk_size = units.len().div_ceil(granularity);
        let mut accepted = false;
        for chunk in units.chunks(chunk_size) {
            if *replays >= MAX_SHRINK_REPLAYS {
                break;
            }
            let requested = chunk.iter().cloned().collect();
            let candidate = best.reduced_without_nodes(&requested)?;
            if candidate.actions.len() == best.actions.len()
                && candidate.exchanges.len() == best.exchanges.len()
                && candidate.backend_events.len() == best.backend_events.len()
            {
                continue;
            }
            *replays += 1;
            if capsule_candidate_reproduces(
                cfg, root, journey, cfg_path, defines, sim, want, &candidate,
            )
            .await?
            {
                best = candidate;
                accepted = true;
                granularity = 2;
                break;
            }
        }
        if accepted {
            continue;
        }
        if granularity >= units.len() {
            break;
        }
        granularity = (granularity * 2).min(units.len());
    }
    Ok(best)
}

#[allow(clippy::too_many_arguments)]
async fn minimize_environment(
    cfg: &Config,
    root: &Path,
    journey: &str,
    cfg_path: &PathBuf,
    defines: &[(String, String)],
    sim: bool,
    want: &std::collections::BTreeSet<String>,
    mut capsule: crate::capsule::Capsule,
    json_output: bool,
) -> Result<crate::capsule::Capsule> {
    use crate::capsule::{EnvironmentEnvelope, EnvironmentOutcome, EnvironmentTrial};

    let mut envelope = EnvironmentEnvelope::default();
    let dimensions = capsule
        .environment
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect::<Vec<_>>();
    let mut excluded = Vec::<String>::new();
    for (dimension_index, (dimension, baseline)) in dimensions.into_iter().enumerate() {
        if dimension_index >= MAX_ENVIRONMENT_DIMENSIONS {
            envelope.trials.push(EnvironmentTrial {
                dimension,
                baseline,
                candidate: None,
                outcome: EnvironmentOutcome::Abstain,
                reason: "dimension-budget-exhausted".into(),
                replay_attempts: 0,
            });
            continue;
        }
        let Some(name) = dimension.strip_prefix("define:").map(str::to_string) else {
            envelope.trials.push(EnvironmentTrial {
                dimension,
                baseline,
                candidate: None,
                outcome: EnvironmentOutcome::Abstain,
                reason: "no-bounded-mutation-adapter".into(),
                replay_attempts: 0,
            });
            continue;
        };
        if envelope.replay_attempts as usize >= MAX_ENVIRONMENT_REPLAYS {
            envelope.trials.push(EnvironmentTrial {
                dimension,
                baseline,
                candidate: None,
                outcome: EnvironmentOutcome::Abstain,
                reason: "replay-budget-exhausted".into(),
                replay_attempts: 0,
            });
            continue;
        }
        let mut candidate_excluded = excluded.clone();
        candidate_excluded.push(name);
        let trial = capsule_candidate_trial(
            cfg,
            root,
            journey,
            cfg_path,
            defines,
            &candidate_excluded,
            false,
            sim,
            want,
            &capsule,
        )
        .await?;
        envelope.replay_attempts += 1;
        let (outcome, reason, attempts) = match trial {
            ReplayTrial::Reproduced => {
                excluded = candidate_excluded;
                envelope.relaxed_dimensions.insert(dimension.clone());
                (
                    EnvironmentOutcome::Reproduces,
                    "exact-identity-reproduced".to_string(),
                    1,
                )
            }
            ReplayTrial::Abstain => (
                EnvironmentOutcome::Abstain,
                "candidate-replay-incomplete".to_string(),
                1,
            ),
            ReplayTrial::NotReproduced => {
                let (baseline_trial, baseline_attempts) =
                    if envelope.replay_attempts as usize >= MAX_ENVIRONMENT_REPLAYS {
                        (ReplayTrial::Abstain, 0)
                    } else {
                        envelope.replay_attempts += 1;
                        (
                            capsule_candidate_trial(
                                cfg, root, journey, cfg_path, defines, &excluded, false, sim, want,
                                &capsule,
                            )
                            .await?,
                            1,
                        )
                    };
                if baseline_trial == ReplayTrial::Reproduced {
                    (
                        EnvironmentOutcome::DoesNotReproduce,
                        "exact-identity-disappeared-and-baseline-reconfirmed".to_string(),
                        1 + baseline_attempts,
                    )
                } else {
                    (
                        EnvironmentOutcome::Abstain,
                        "baseline-could-not-be-reconfirmed".to_string(),
                        1 + baseline_attempts,
                    )
                }
            }
        };
        envelope.trials.push(EnvironmentTrial {
            dimension,
            baseline,
            candidate: None,
            outcome,
            reason,
            replay_attempts: attempts,
        });
    }
    if (envelope.replay_attempts as usize) < MAX_ENVIRONMENT_REPLAYS {
        envelope.replay_attempts += 1;
        envelope.complete = capsule_candidate_trial(
            cfg, root, journey, cfg_path, defines, &excluded, false, sim, want, &capsule,
        )
        .await?
            == ReplayTrial::Reproduced;
    }
    if !envelope.complete {
        envelope.relaxed_dimensions.clear();
    }
    say(
        json_output,
        format!(
            "  environment: {} relaxed dimension(s), {} replay(s), final {}",
            envelope.relaxed_dimensions.len(),
            envelope.replay_attempts,
            if envelope.complete {
                "confirmed"
            } else {
                "ABSTAIN"
            }
        ),
    );
    capsule.environment_envelope = envelope;
    capsule.refresh_causal_graph()?;
    Ok(capsule)
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
    Ok(confirm_trace_trial(
        cfg,
        root,
        journey,
        cfg_path,
        defines,
        &[],
        trace,
        true,
        sim,
        want,
    )
    .await?
        == ReplayTrial::Reproduced)
}

#[allow(clippy::too_many_arguments)]
async fn confirm_trace_trial(
    cfg: &Config,
    root: &Path,
    journey: &str,
    cfg_path: &PathBuf,
    defines: &[(String, String)],
    excluded_defines: &[String],
    trace: &[String],
    warm: bool,
    sim: bool,
    want: &std::collections::BTreeSet<String>,
) -> Result<ReplayTrial> {
    std::fs::write(cfg_path, json!({ "replay": trace }).to_string())?;
    let outcome = match run_explorer_with_exclusions(
        cfg,
        root,
        journey,
        warm,
        defines,
        excluded_defines,
        false,
        sim,
        false,
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(_) => return Ok(ReplayTrial::Abstain),
    };
    let log = std::fs::read_to_string(outcome.run_dir.join("drive-a.log")).unwrap_or_default();
    if !replay_is_hermetic(&log) {
        return Ok(ReplayTrial::Abstain);
    }
    if reproduces_original(&findings_for_tier(cfg, &outcome.run_dir, sim), want) {
        Ok(ReplayTrial::Reproduced)
    } else {
        Ok(ReplayTrial::NotReproduced)
    }
}

/// A causal-capsule confirmation is valid only if every external request was
/// fulfilled from the capsule. An exception after a fail-closed miss may be a
/// secondary artifact of the incomplete environment, not the original bug.
pub(super) fn replay_is_hermetic(log: &str) -> bool {
    !log.lines().any(|line| line.contains("CAPSULE:MISS "))
}

/// Capture one final live replay after action minimization so the causal
/// exchanges and backend events share the minimized action clock. Earlier
/// discovery evidence may refer to actions that ddmin has already removed.
#[allow(clippy::too_many_arguments)]
pub(super) async fn capture_confirmed_trace(
    cfg: &Config,
    root: &Path,
    journey: &str,
    cfg_path: &PathBuf,
    defines: &[(String, String)],
    trace: &[String],
    sim: bool,
    want: &std::collections::BTreeSet<String>,
) -> Result<Option<RunOutcome>> {
    std::fs::write(cfg_path, json!({ "replay": trace }).to_string())?;
    let outcome = match run_explorer(cfg, root, journey, true, defines, false, sim, false).await {
        Ok(outcome) => outcome,
        Err(_) => return Ok(None),
    };
    let log = std::fs::read_to_string(outcome.run_dir.join("drive-a.log")).unwrap_or_default();
    if replay_is_hermetic(&log)
        && reproduces_original(&findings_for_tier(cfg, &outcome.run_dir, sim), want)
    {
        Ok(Some(outcome))
    } else {
        Ok(None)
    }
}

/// Bounded delta reduction removes chunks at decreasing granularity rather
/// than one action at a time. Each replay is an expensive device run, so the
/// reducer tries large complements first and accepts only exact, hermetic
/// reproductions.
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
    let load_only_reproduces =
        confirm_trace(cfg, root, journey, cfg_path, defines, &[], sim, want).await?;
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
    for action_index in 0..current.len() {
        if replays >= MAX_SHRINK_REPLAYS {
            break;
        }
        let original = current[action_index].clone();
        for reduced in action_value_reductions(&original) {
            if replays >= MAX_SHRINK_REPLAYS {
                break;
            }
            let mut candidate = current.clone();
            candidate[action_index] = reduced;
            replays += 1;
            if confirm_trace(cfg, root, journey, cfg_path, defines, &candidate, sim, want).await? {
                current = candidate;
                say(
                    json,
                    format!("    value[{action_index}]: smaller value still reproduces"),
                );
                break;
            }
        }
    }
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
            let reproduces = !candidate.is_empty()
                && confirm_trace(cfg, root, journey, cfg_path, defines, &candidate, sim, want)
                    .await?;
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
                    let still =
                        confirm_trace(cfg, root, journey, cfg_path, defines, &candidate, sim, want)
                            .await?;
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

fn action_value_reductions(action: &str) -> Vec<String> {
    const MAX_REDUCTIONS: usize = 8;
    let Some((prefix, value)) = action.rsplit_once('=') else {
        return Vec::new();
    };
    if let Some(text_prefix) = prefix.strip_prefix("type:") {
        let characters = value.chars().collect::<Vec<_>>();
        let mut values = vec![String::new()];
        if let Some(first) = characters.first() {
            values.push(first.to_string());
        }
        if characters.len() > 1 {
            values.push(characters[..characters.len().div_ceil(2)].iter().collect());
        }
        values.push(
            if value.chars().all(|character| character.is_ascii_digit()) {
                "0".to_string()
            } else {
                "a".to_string()
            },
        );
        return values
            .into_iter()
            .map(|reduced| format!("type:{text_prefix}={reduced}"))
            .filter(|reduced| reduced != action)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .take(MAX_REDUCTIONS)
            .collect();
    }
    if prefix.starts_with("scroll:") {
        let Ok(amount) = value.parse::<i64>() else {
            return Vec::new();
        };
        let sign = amount.signum();
        return [sign, amount / 2]
            .into_iter()
            .filter(|reduced| *reduced != 0 && *reduced != amount)
            .map(|reduced| format!("{prefix}={reduced}"))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .take(MAX_REDUCTIONS)
            .collect();
    }
    Vec::new()
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

#[cfg(test)]
mod tests {
    use super::action_value_reductions;

    #[test]
    fn typed_action_reductions_preserve_selector_structure() {
        let reductions = action_value_reductions("type:key:email=example@example.test");
        assert!(reductions.contains(&"type:key:email=".to_string()));
        assert!(reductions.contains(&"type:key:email=e".to_string()));
        assert!(reductions
            .iter()
            .all(|reduction| reduction.starts_with("type:key:email=")));
    }

    #[test]
    fn scroll_reductions_preserve_direction_and_target() {
        assert_eq!(
            action_value_reductions("scroll:key:list=-300"),
            vec!["scroll:key:list=-1", "scroll:key:list=-150"]
        );
        assert!(action_value_reductions("tap:key:submit").is_empty());
    }
}
