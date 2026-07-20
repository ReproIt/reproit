use super::*;

/// A single walk that traverses every reachable edge once, with the transition
/// each action exercises, plus edges with no path from the entry state.
pub(super) struct VerifyPlan {
    pub(super) actions: Vec<String>,
    pub(super) edge_at: Vec<usize>,
    pub(super) unreachable: Vec<usize>,
}

/// Greedily build an edge-covering walk from the entry state: repeatedly take
/// the untaken edge reachable by the shortest path, pathfinding to its `from`
/// and then crossing it. Navigation also covers any edges it traverses, so the
/// whole reachable graph is checked in one device run.
pub(super) fn cover_walk(map: &AppMap) -> Result<VerifyPlan> {
    let entry = entry_state(map).ok_or_else(|| anyhow::anyhow!("map has no entry state"))?;
    let mut untaken: BTreeSet<usize> = (0..map.transitions.len()).collect();
    let mut actions = Vec::new();
    let mut edge_at = Vec::new();
    let mut current = entry;
    while !untaken.is_empty() {
        let mut best: Option<(usize, Vec<usize>)> = None;
        for &ti in &untaken {
            if let Some(path) = edge_path_to(map, &current, &map.transitions[ti].from) {
                if best.as_ref().is_none_or(|(_, p)| path.len() < p.len()) {
                    best = Some((ti, path));
                }
            }
        }
        let Some((ti, mut path)) = best else { break };
        path.push(ti); // navigation edges, then the target edge
        for pti in path {
            let t = &map.transitions[pti];
            actions.push(action_str(&t.action));
            edge_at.push(pti);
            current = t.to.clone();
            untaken.remove(&pti);
        }
    }
    Ok(VerifyPlan {
        actions,
        edge_at,
        unreachable: untaken.into_iter().collect(),
    })
}

/// A drifted edge: the app no longer lands where the map says it should.
pub struct Drift {
    pub from: String,
    pub action: String,
    pub expected: String,
    pub observed: String,
}

/// The result of `map --verify`.
pub struct VerifyReport {
    pub edges: usize,
    pub ok: usize,
    pub entry_drift: Option<(String, String)>, // (expected, observed)
    pub drift: Vec<Drift>,
    pub missed: Vec<(String, String)>, // (from, action) the app could not perform
    pub unreachable: Vec<(String, String)>, // (from, action) no path from entry
    pub crashed: bool,
}

impl VerifyReport {
    /// Clean iff nothing drifted, missed, was unreachable, and no crash.
    pub fn is_clean(&self) -> bool {
        self.entry_drift.is_none()
            && self.drift.is_empty()
            && self.missed.is_empty()
            && self.unreachable.is_empty()
            && !self.crashed
    }

    pub fn print(&self) {
        if let Some((exp, obs)) = &self.entry_drift {
            println!("  DRIFT entry: map says {exp}, app boots {obs}");
        }
        for d in &self.drift {
            println!(
                "  DRIFT {} --{}--> map says {}, app reached {}",
                short(&d.from),
                d.action,
                d.expected,
                d.observed
            );
        }
        for (from, action) in &self.missed {
            println!(
                "  MISS  {} --{}--> action no longer available",
                short(from),
                action
            );
        }
        for (from, action) in &self.unreachable {
            println!(
                "  UNREACHABLE {} --{}--> no path from entry",
                short(from),
                action
            );
        }
        if self.crashed {
            println!("  CRASH the app threw while walking the map");
        }
        if self.is_clean() {
            println!("map verified: {}/{} edges still hold", self.ok, self.edges);
        } else {
            println!(
                "map drifted: {}/{} edges hold ({} drift, {} miss, {} unreachable)",
                self.ok,
                self.edges,
                self.drift.len() + self.entry_drift.iter().count(),
                self.missed.len(),
                self.unreachable.len(),
            );
        }
    }
}

/// Strip the `s_` state-key prefix for display.
pub(super) fn short(key: &str) -> &str {
    key.strip_prefix("s_").unwrap_or(key)
}

/// Re-walk the committed map and report where the app has drifted from it. One
/// device run covers every reachable edge; each edge's landing state is
/// compared to what the map recorded. This is the "is the map still valid?"
/// check.
pub async fn verify_map(loaded: &config::Loaded, quiet: bool) -> Result<VerifyReport> {
    let map = load_map(&loaded.root)?.ok_or_else(|| {
        anyhow::anyhow!("no internal app model; run `reproit scan` once to learn the app")
    })?;
    let edges = map.transitions.len();
    if edges == 0 {
        return Ok(VerifyReport {
            edges: 0,
            ok: 0,
            entry_drift: None,
            drift: Vec::new(),
            missed: Vec::new(),
            unreachable: Vec::new(),
            crashed: false,
        });
    }
    let plan = cover_walk(&map)?;
    // map --verify re-walks the real app the same way build_map did (sim tier);
    // the headless tier has no backend and dies on a multi-sim host.
    let (log, passed) = run_replay(loaded, &plan.actions, false, true).await?;
    let (initial, steps) = replay_trace(&log);

    let entry = entry_state(&map).ok_or_else(|| {
        anyhow::anyhow!(
            "the internal app model has no states; run `reproit scan` to relearn the app"
        )
    })?;
    let entry_sig = short(&entry).to_string();
    let entry_drift = match &initial {
        Some(obs) if *obs != entry_sig => Some((entry_sig.clone(), obs.clone())),
        _ => None,
    };

    let mut drift = Vec::new();
    let mut missed = Vec::new();
    let mut ok = 0usize;
    for (k, &ti) in plan.edge_at.iter().enumerate() {
        let t = &map.transitions[ti];
        let action = action_str(&t.action);
        let expected = short(&t.to).to_string();
        match steps.get(k) {
            Some(s) if s.missed || s.state_after.is_none() => {
                missed.push((t.from.clone(), action));
            }
            Some(s) => {
                let obs = s.state_after.as_ref().unwrap();
                if *obs == expected {
                    ok += 1;
                } else {
                    drift.push(Drift {
                        from: t.from.clone(),
                        action,
                        expected,
                        observed: obs.clone(),
                    });
                }
            }
            None => missed.push((t.from.clone(), action)), // log truncated
        }
    }
    let unreachable = plan
        .unreachable
        .iter()
        .map(|&ti| {
            let t = &map.transitions[ti];
            (t.from.clone(), action_str(&t.action))
        })
        .collect();

    let report = VerifyReport {
        edges,
        ok,
        entry_drift,
        drift,
        missed,
        unreachable,
        crashed: !passed,
    };
    if !quiet {
        report.print();
    }
    Ok(report)
}

// ---- authoring (MCP / agent) --------------------------------------------
