use super::*;

/// Replay an action sequence once through the same tier `check` uses, returning
/// the drive log and whether the harness reported a clean run. `warm` reuses
/// the previous build.
pub(super) async fn run_replay(
    loaded: &config::Loaded,
    actions: &[String],
    warm: bool,
    sim: bool,
) -> Result<(String, bool)> {
    run_replay_cfg(
        loaded,
        serde_json::json!({ "seed": 0, "replay": actions }),
        warm,
        sim,
    )
    .await
}

/// Replay an arbitrary fuzz-config value (single-actor or multi-actor) once.
/// `sim` picks the tier: true = real simulator + backend (what E2E journeys
/// need), false = the in-process headless tier (pure-widget fuzzing only).
pub(super) async fn run_replay_cfg(
    loaded: &config::Loaded,
    cfg: serde_json::Value,
    warm: bool,
    sim: bool,
) -> Result<(String, bool)> {
    let cfg_path = crate::runtime::project_layout::fuzz_config_path(&loaded.root);
    std::fs::create_dir_all(cfg_path.parent().unwrap())?;
    std::fs::write(&cfg_path, cfg.to_string())?;
    let defines = vec![(
        "REPROIT_FUZZ_CONFIG".to_string(),
        cfg_path.to_string_lossy().into_owned(),
    )];
    let outcome = orchestrator::run_journey_tier(
        &loaded.config,
        &loaded.root,
        "explore",
        &orchestrator::RunOpts {
            devices: 1,
            warm,
            extra_defines: &defines,
            ..Default::default()
        },
        sim,
    )
    .await?;
    let log = std::fs::read_to_string(outcome.run_dir.join("drive-a.log")).unwrap_or_default();
    Ok((log, outcome.passed))
}

// ---- map --verify --------------------------------------------------------

/// One replayed action's outcome, reconstructed from the drive log so
/// positional alignment survives misses: a missed action emits no `FUZZ:STATE`,
/// so we track the per-action state by walking `FUZZ:ACT` / `FUZZ:MISS` /
/// `FUZZ:STATE` in order rather than by counting `FUZZ:STATE` lines.
pub(super) struct ReplayStep {
    pub(super) missed: bool,
    pub(super) state_after: Option<String>,
}

/// Parse a replay drive log into the initial state and a per-action outcome.
pub(super) fn replay_trace(log: &str) -> (Option<String>, Vec<ReplayStep>) {
    let mut initial = None;
    let mut steps: Vec<ReplayStep> = Vec::new();
    for line in log.lines() {
        if line.contains("FUZZ:ACT ") {
            steps.push(ReplayStep {
                missed: false,
                state_after: None,
            });
        } else if line.contains("FUZZ:MISS ") {
            if let Some(s) = steps.last_mut() {
                s.missed = true;
            }
        } else if let Some(i) = line.find("FUZZ:STATE ") {
            let sig = line[i + "FUZZ:STATE ".len()..]
                .split_whitespace()
                .next()
                .map(str::to_string);
            match steps.last_mut() {
                Some(s) => s.state_after = sig,
                None => initial = sig, // emitted before the first action
            }
        }
    }
    (initial, steps)
}

/// BFS shortest path (as transition indices) from state key `from` to key `to`.
pub(super) fn edge_path_to(map: &AppMap, from: &str, to: &str) -> Option<Vec<usize>> {
    if from == to {
        return Some(Vec::new());
    }
    let mut q = VecDeque::new();
    let mut prev: BTreeMap<&str, (usize, &str)> = BTreeMap::new();
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    q.push_back(from);
    seen.insert(from);
    while let Some(cur) = q.pop_front() {
        for (ti, t) in map.transitions.iter().enumerate() {
            if t.from == cur && seen.insert(t.to.as_str()) {
                prev.insert(t.to.as_str(), (ti, cur));
                if t.to == to {
                    let mut path = vec![ti];
                    let mut node = cur;
                    while node != from {
                        let (pti, pfrom) = prev[node];
                        path.push(pti);
                        node = pfrom;
                    }
                    path.reverse();
                    return Some(path);
                }
                q.push_back(t.to.as_str());
            }
        }
    }
    None
}
