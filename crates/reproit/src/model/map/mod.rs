//! The app map as LIVE state: every exploration/fuzz run's EXPLORE records
//! merge into .reproit/map/appmap.json (states/transitions union by semantics
//! signature) and .reproit/map/visits.json (per-sig visit counts + the start
//! state). Frontier fuzzing and authoring path over this; normal commands keep
//! the model fresh, while `reproit debug map` exposes diagnostics.

use crate::backends::orchestrator;
use crate::config::Config;
use crate::layout;
use crate::model::appmap::AppMap;
#[cfg(test)]
use crate::model::appmap::{
    Action, OperabilityGaps, Reversibility, State, StateSignature, Transition,
};
use anyhow::{Context, Result};
#[cfg(test)]
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

mod frontier;
mod merge;
mod parse;
mod persistence;
mod provenance;

#[cfg(test)]
use frontier::VISIT_WEIGHT_CAP;
#[allow(unused_imports)] // Compatibility façade; several helpers are agent-facing APIs.
pub(crate) use frontier::{edges_summary, entry_state, frontier_path, path_to_label, Visits};
#[cfg(test)]
use merge::parse_action;
use merge::sig_index;
pub(crate) use merge::{action_str, merge};
#[allow(unused_imports)]
// Types remain reachable at their pre-split `crate::model::map` paths.
pub(crate) use parse::{parse_run, LeakMetric, RelationCheck, RelationViolation, RunObs};
#[allow(unused_imports)] // MapSnapshot is normally inferred by rebuild callers.
pub(crate) use persistence::{
    appmap_path, begin_full_rebuild, load_map, load_visits, restore_map, MapSnapshot,
};
use persistence::{save_map, save_visits};
#[allow(unused_imports)] // MapProvenance is part of the existing façade contract.
pub(crate) use provenance::{map_freshness, stamp_map, MapFreshness, MapProvenance};

/// Merge one run's observations into an IN-MEMORY map + visits, returning the
/// parsed observations. Does no I/O, so callers that must stay pure (notably
/// `fuzz`, which reports discoveries but never mutates the committed graph) can
/// accrue cross-seed/cross-batch coverage guidance within a single invocation
/// without touching `.reproit/map/appmap.json` / `.reproit/map/visits.json`.
pub(crate) fn absorb_run_inmem(map: &mut AppMap, visits: &mut Visits, log: &str) -> RunObs {
    let obs = parse_run(log);
    if obs.states.is_empty() {
        return obs;
    }
    merge(map, &obs);
    if visits.start.is_none() {
        visits.start = obs.start.clone();
    }
    for sig in obs.states.keys() {
        *visits.counts.entry(sig.clone()).or_insert(0) += 1;
    }
    for (from, action, _to) in &obs.edges {
        *visits
            .edge_counts
            .entry(format!("{from}|{action}"))
            .or_insert(0) += 1;
    }
    obs
}

/// Merge one run's observations into both live files and persist them. This is
/// `map`'s commit path: `map` is what folds discovered coverage into the
/// committed graph. `fuzz` must NOT call this (it would make a fixed seed drift
/// across invocations as visit counts accumulate); it uses
/// [`absorb_run_inmem`].
pub(crate) fn absorb_run(root: &Path, cfg: &Config, log: &str) -> Result<RunObs> {
    let mut map = load_map(root, cfg);
    let mut visits = load_visits(root);
    let obs = absorb_run_inmem(&mut map, &mut visits, log);
    if !obs.states.is_empty() {
        save_map(root, &map)?;
        save_visits(root, &visits)?;
    }
    Ok(obs)
}

/// Concatenate every device's drive log in a run dir (`drive-a.log`,
/// `drive-b.log`, ...), sorted by name, so a multi-actor run's full traversal
/// feeds the map and not just device a's. A single-device run just yields
/// `drive-a.log`.
fn read_all_device_logs(run_dir: &Path) -> Result<String> {
    let mut logs: Vec<(String, String)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(run_dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with("drive-") && name.ends_with(".log") {
                if let Ok(s) = std::fs::read_to_string(e.path()) {
                    logs.push((name, s));
                }
            }
        }
    }
    if logs.is_empty() {
        anyhow::bail!("no drive-*.log files in {}", run_dir.display());
    }
    logs.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(logs
        .into_iter()
        .map(|(_, s)| s)
        .collect::<Vec<_>>()
        .join("\n"))
}

pub async fn build_map(
    cfg: &Config,
    root: &Path,
    journey: &str,
    budget: Option<u32>,
    label: bool,
    from_run: Option<&Path>,
) -> Result<()> {
    let run_dir = match from_run {
        Some(p) if p.is_absolute() => p.to_path_buf(),
        Some(p) => root.join(p),
        None => {
            let mut extra_defines: Vec<(String, String)> = Vec::new();
            if let Some(budget) = budget {
                let cfg_path = layout::fuzz_config_path(root);
                std::fs::create_dir_all(cfg_path.parent().unwrap())?;
                std::fs::write(
                    &cfg_path,
                    serde_json::json!({ "seed": 0, "budget": budget }).to_string(),
                )?;
                extra_defines.push((
                    "REPROIT_FUZZ_CONFIG".to_string(),
                    cfg_path.to_string_lossy().into_owned(),
                ));
            }
            let outcome = orchestrator::run_journey(
                cfg,
                root,
                journey,
                &orchestrator::RunOpts {
                    devices: 1,
                    extra_defines: &extra_defines,
                    ..Default::default()
                },
            )
            .await?;
            if !outcome.passed {
                eprintln!(
                    "  note: exploration run did not pass cleanly; mapping what was observed"
                );
            }
            outcome.run_dir
        }
    };
    // Fold in EVERY device's log, not just device a: a multi-actor scenario run
    // has each actor traverse different (often deeper) screens, and a scenario
    // now emits the same EXPLORE records the crawl does, so the dual-user
    // journeys double as the mapper for screens a single actor can't reach.
    let log = read_all_device_logs(&run_dir)?;
    let obs = absorb_run(root, cfg, &log)?;
    if let Some(line) = log.lines().find(|line| line.contains("EXPLORE:TRUNCATED ")) {
        let detail = line
            .split_once("EXPLORE:TRUNCATED ")
            .map(|(_, detail)| detail)
            .unwrap_or("{}");
        eprintln!(
            "  note: map reached its deterministic work limit; saved bounded partial coverage \
             ({detail})"
        );
    }
    if obs.states.is_empty() {
        // UNSCANNABLE (a WAF bot-challenge interstitial): the runner never reached
        // the app, so there are legitimately no states to map. Do NOT treat this as
        // a "missing explorer journey" error; return with an empty map so the caller
        // (scan) can surface the runner's blocked diagnostic instead.
        if log.contains("EXPLORE:UNSCANNABLE") {
            return Ok(());
        }
        anyhow::bail!(
            "no EXPLORE:STATE records in {} (is the generated explorer journey installed?)",
            run_dir.display()
        );
    }

    if label {
        let mut map = load_map(root, cfg);
        let state_labels: BTreeMap<String, Vec<String>> = map
            .states
            .values()
            .filter_map(|s| {
                let sig = s.signature.semantics_hash.clone()?;
                Some((sig, s.description.split(", ").map(String::from).collect()))
            })
            .collect();
        match label_states(cfg, &state_labels).await {
            Ok(names) => {
                let index = sig_index(&map);
                let mut renames: Vec<(String, String)> = Vec::new();
                for (sig, name) in names {
                    if let Some(old_id) = index.get(&sig) {
                        if old_id != &name && !map.states.contains_key(&name) {
                            renames.push((old_id.clone(), name));
                        }
                    }
                }
                for (old, new) in renames {
                    if let Some(state) = map.states.remove(&old) {
                        map.states.insert(new.clone(), state);
                        for t in &mut map.transitions {
                            if t.from == old {
                                t.from = new.clone();
                            }
                            if t.to == old {
                                t.to = new.clone();
                            }
                        }
                    }
                }
                save_map(root, &map)?;
            }
            Err(e) => eprintln!("  warn: labeling pass failed ({e}); keeping current names"),
        }
    }

    // The graph and its provenance are committed as one logical snapshot. The
    // next graph-consuming command compares actual project inputs to this stamp
    // and refreshes automatically when they differ.
    stamp_map(root)?;
    let map = load_map(root, cfg);
    // Progress lines go to STDERR: stdout is reserved for machine output (e.g. a
    // `--json` scan/fuzz that auto-builds the map on first run), and these landing
    // on stdout corrupted the JSON object a piped consumer parses.
    eprintln!(
        "  map: {} states, {} transitions -> {}",
        map.states.len(),
        map.transitions.len(),
        appmap_path(root).display()
    );
    Ok(())
}

/// Ask the LLM to name states from their visible labels. Resilient: any
/// parse failure keeps the current names.
async fn label_states(
    cfg: &Config,
    state_labels: &BTreeMap<String, Vec<String>>,
) -> Result<BTreeMap<String, String>> {
    let provider = llm::from_spec(&cfg.llm.to_spec())?;
    let mut listing = String::new();
    for (sig, labels) in state_labels {
        listing.push_str(&format!("{sig}: {}\n", labels.join(" | ")));
    }
    let prompt = format!(
        "These are screens of a mobile app, identified by signature, with the visible semantic \
         labels observed on each. Give each a short snake_case name (login, meet_feed, profile, \
         settings, ...). Reply with ONLY a JSON object mapping signature to name, no commentary, \
         no code fences.\n\n{listing}"
    );
    let response = provider.complete(&llm::Task::new(prompt)).await?;
    let json_str = response
        .find('{')
        // Guard the slice: an LLM reply could place `}` before its first `{`, and
        // `&response[s..=e]` would panic when e < s. Require e >= s.
        .and_then(|s| {
            response
                .rfind('}')
                .filter(|&e| e >= s)
                .map(|e| &response[s..=e])
        })
        .context("no JSON object in labeling response")?;
    let parsed: BTreeMap<String, String> = serde_json::from_str(json_str)?;
    let mut used = std::collections::HashSet::new();
    let mut out = BTreeMap::new();
    for (sig, name) in parsed {
        let mut clean: String = name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_lowercase()
                } else {
                    '_'
                }
            })
            .collect::<String>()
            .trim_matches('_')
            .to_string();
        if clean.is_empty() || clean.chars().next().unwrap().is_ascii_digit() {
            clean = format!("s_{sig}");
        }
        while !used.insert(clean.clone()) {
            clean.push('_');
        }
        out.insert(sig, clean);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The VERBATIM `EXPLORE:GROUNDTRUTH` JSON each in-process operability
    /// agent emits, kept in ONE shared place:
    /// `tests/golden/operability/<platform>.json` (byte-for-byte the marker
    /// the real agent prints). The engine contract tests below read these
    /// goldens instead of inlining the literal, and a per-platform
    /// capture-diff CI job (.github/workflows/ci.yml) re-runs the real agent,
    /// drops the volatile `sig`, and DIFFs the live marker against the same
    /// golden. So the golden is the single source of truth: if the real
    /// marker drifts, the test keeps asserting the old contract here while
    /// the CI diff catches the drift against production, instead of an
    /// inline literal silently going stale.
    fn golden_groundtruth(platform: &str) -> &'static str {
        match platform {
            "web" => include_str!("../../../tests/golden/operability/web.json"),
            "appkit" => include_str!("../../../tests/golden/operability/appkit.json"),
            "wpf" => include_str!("../../../tests/golden/operability/wpf.json"),
            "qt" => include_str!("../../../tests/golden/operability/qt.json"),
            "gtk" => include_str!("../../../tests/golden/operability/gtk.json"),
            "flutter" => include_str!("../../../tests/golden/operability/flutter.json"),
            other => panic!("no operability golden for platform {other:?}"),
        }
        .trim()
    }

    /// Parse a platform's golden marker through the real engine, returning the
    /// state's operability gaps. The golden carries the marker's own `sig`, so
    /// we read it back out of the JSON rather than hard-coding it at each
    /// call site.
    fn gaps_from_golden(platform: &str) -> OperabilityGaps {
        let payload = golden_groundtruth(platform);
        let sig = serde_json::from_str::<Value>(payload)
            .expect("golden is valid JSON")
            .get("sig")
            .and_then(Value::as_str)
            .expect("golden carries a sig")
            .to_string();
        let log = format!("EXPLORE:GROUNDTRUTH {payload}");
        parse_run(&log)
            .gaps
            .get(&sig)
            .unwrap_or_else(|| panic!("gaps for the {platform} agent state ({sig})"))
            .clone()
    }

    fn st(desc: &str) -> State {
        State {
            description: desc.to_string(),
            signature: StateSignature {
                screenshot_phash: None,
                semantics_hash: None,
                route: None,
            },
            elements: vec![],
            texts: vec![],
            parameters: vec![],
            operability_gaps: Default::default(),
        }
    }
    fn tap(from: &str, label: &str, to: &str) -> Transition {
        Transition {
            from: from.to_string(),
            to: to.to_string(),
            action: Action::Tap {
                finder: label.to_string(),
            },
            guards: vec![],
            reversibility: Reversibility::ProposedReversible,
            expected: None,
        }
    }
    fn sample() -> AppMap {
        let mut states = BTreeMap::new();
        states.insert("Home".to_string(), st("home screen"));
        states.insert("Settings".to_string(), st("settings screen"));
        states.insert("About".to_string(), st("about / version info"));
        AppMap {
            app: "demo".to_string(),
            version: 1,
            states,
            transitions: vec![
                tap("Home", "Settings", "Settings"),
                tap("Settings", "About", "About"),
            ],
            invariants: vec![],
            interrupts: vec![],
        }
    }

    #[test]
    fn entry_is_the_state_without_incoming_edges() {
        assert_eq!(entry_state(&sample()).as_deref(), Some("Home"));
    }

    #[test]
    fn path_to_label_finds_shortest_action_sequence() {
        let m = sample();
        let (target, path) = path_to_label(&m, "about").expect("About is reachable");
        assert_eq!(target, "About");
        assert_eq!(
            path,
            vec!["tap:Settings".to_string(), "tap:About".to_string()]
        );
        // the entry state itself matching yields an empty path.
        let (t0, p0) = path_to_label(&m, "home").unwrap();
        assert_eq!(t0, "Home");
        assert!(p0.is_empty());
        // an unreachable/unknown label yields None.
        assert!(path_to_label(&m, "nonexistent-screen").is_none());
    }

    #[test]
    fn frontier_path_is_deterministic_on_ties() {
        // Two unvisited frontier states, each one tap from Home: equal visit count
        // AND equal path length, so the pick comes down to the tie-break. Before
        // the fix it resolved on `HashMap` iteration order (a fresh random seed per
        // call), so `fuzz --frontier` could target a different state run-to-run.
        let sig_state = |sig: &str| {
            let mut s = st("x");
            s.signature.semantics_hash = Some(sig.to_string());
            s
        };
        let mut states = BTreeMap::new();
        states.insert("Home".to_string(), sig_state("sig-home"));
        states.insert("Alpha".to_string(), sig_state("sig-alpha"));
        states.insert("Bravo".to_string(), sig_state("sig-bravo"));
        let map = AppMap {
            app: "demo".to_string(),
            version: 1,
            states,
            transitions: vec![tap("Home", "a", "Alpha"), tap("Home", "b", "Bravo")],
            invariants: vec![],
            interrupts: vec![],
        };
        let visits = Visits {
            start: Some("sig-home".to_string()),
            counts: BTreeMap::new(),
            edge_counts: BTreeMap::new(),
        };
        // Stable across many calls (each rebuilds the internal HashMaps with a new
        // seed, so a non-deterministic tie-break would diverge over the loop)...
        let first = frontier_path(&map, &visits).expect("a frontier exists");
        for _ in 0..64 {
            assert_eq!(frontier_path(&map, &visits), Some(first.clone()));
        }
        // ...and it is the smallest-signature tied state (sig-alpha < sig-bravo),
        // not whichever happened to hash first.
        assert_eq!(first.0, "Alpha");
    }

    #[test]
    fn parse_action_recovers_typed_scroll_system_edges() {
        // type:/scroll:/system: must round-trip into their real variants, not
        // collapse to Back (which lost the finder/value of form-driven edges).
        assert!(matches!(parse_action("tap:Go"), Action::Tap { .. }));
        match parse_action("type:role:textfield#0=hello") {
            Action::Type { finder, text } => {
                assert_eq!(finder, "role:textfield#0");
                assert_eq!(text, "hello");
            }
            a => panic!("expected Type, got {a:?}"),
        }
        match parse_action("scroll:key:list=-300") {
            Action::Scroll { finder, dy } => {
                assert_eq!(finder, "key:list");
                assert_eq!(dy, -300);
            }
            a => panic!("expected Scroll, got {a:?}"),
        }
        match parse_action("system:back") {
            Action::System { event } => assert_eq!(event, "back"),
            a => panic!("expected System, got {a:?}"),
        }
        assert!(matches!(parse_action("back"), Action::Back));
        // A typed edge with no `=value` still parses as Type (empty text), not Back.
        assert!(matches!(parse_action("type:key:x"), Action::Type { .. }));
    }

    #[test]
    fn edges_summary_lists_real_transitions() {
        assert!(edges_summary(&sample())
            .iter()
            .any(|e| e == "Home --tap:Settings--> Settings"));
    }

    #[test]
    fn edge_weights_caps_the_visit_count_so_hub_actions_keep_a_floor() {
        // A hub destination visited far more than the cap must not decay the
        // edge weight toward zero: the count feeding 1/(1+count) is clamped to
        // VISIT_WEIGHT_CAP, so the walk can still reach it.
        let sig_state = |sig: &str| {
            let mut s = st("x");
            s.signature.semantics_hash = Some(sig.to_string());
            s
        };
        let mut states = BTreeMap::new();
        states.insert("A".to_string(), sig_state("sigA"));
        states.insert("B".to_string(), sig_state("sigB"));
        let map = AppMap {
            app: "demo".to_string(),
            version: 1,
            states,
            transitions: vec![tap("A", "go", "B")],
            invariants: vec![],
            interrupts: vec![],
        };
        let mut visits = Visits::default();
        visits.counts.insert("sigB".to_string(), 1000); // wildly over-visited hub
        let ew = visits.edge_weights(&map);
        let count = *ew
            .get("sigA")
            .and_then(|m| m.values().next())
            .expect("an edge from sigA");
        assert_eq!(
            count, VISIT_WEIGHT_CAP,
            "the weighting count must be capped, not the raw 1000"
        );
    }

    #[test]
    fn merge_captures_route_from_explore_state() {
        // A runner that reports a route (Flutter anchor, web URL path, ...) lands
        // it on the verified state, so the candidate map can reconcile by route.
        let log = r#"EXPLORE:STATE {"sig":"abc","route":"/home","labels":["Home"]}"#;
        let obs = parse_run(log);
        assert_eq!(obs.routes.get("abc").map(String::as_str), Some("/home"));
        let mut map = AppMap {
            app: "t".into(),
            version: 1,
            states: BTreeMap::new(),
            transitions: vec![],
            invariants: vec![],
            interrupts: vec![],
        };
        merge(&mut map, &obs);
        let state = map.states.values().next().expect("a merged state");
        assert_eq!(state.signature.route.as_deref(), Some("/home"));
    }

    #[test]
    fn groundtruth_marker_yields_operability_gaps() {
        // The motivating case: a control operable by pointer but not keyboard-
        // reachable and exposing no role (the finding-div in the dashboard). This
        // is the web in-process agent's marker, kept in
        // tests/golden/operability/web.json (sig "abc"); CI re-captures + diffs it.
        let log = format!(
            "{}\nEXPLORE:GROUNDTRUTH {}",
            r#"EXPLORE:STATE {"sig":"abc","labels":[]}"#,
            golden_groundtruth("web"),
        );
        let obs = parse_run(&log);
        let g = obs.gaps.get("abc").expect("gaps for abc");
        assert_eq!(
            g.pointer_only, 1,
            "one operable element not keyboard-activatable"
        );
        assert_eq!(
            g.keyboard_unreachable, 1,
            "one operable element not in tab order"
        );
        assert_eq!(g.no_role, 1, "one operable element with no role");
        assert!(!g.focus_trap);
        // The grounded per-element detail: exactly the one failing element, by
        // selector, tagged with every dimension it fails. This is what the
        // accessibility view/MCP tool serves, so it must be present, not a count.
        assert_eq!(g.items.len(), 1, "only the one failing element is recorded");
        assert_eq!(g.items[0].selector, "role:option#0");
        assert_eq!(
            g.items[0].kinds,
            vec!["pointer_only", "keyboard_unreachable", "no_role"],
            "the failing element is tagged with all three dimensions it fails"
        );
        // The non-operable decoration is never a gap; the healthy nav is not either.
        let mut map = AppMap {
            app: "t".into(),
            version: 1,
            states: BTreeMap::new(),
            transitions: vec![],
            invariants: vec![],
            interrupts: vec![],
        };
        merge(&mut map, &obs);
        let state = map.states.values().next().expect("a merged state");
        assert_eq!(state.operability_gaps.pointer_only, 1);
        assert_eq!(state.operability_gaps.keyboard_unreachable, 1);
    }

    #[test]
    fn rerender_marker_yields_keyed_churn() {
        // A transition that rebuilt persistent chrome which did not change: the
        // runner emits EXPLORE:RERENDER with the from sig, the action, and the
        // churned anchor selectors. parse_run keys it by (from, action). A marker
        // with an empty churned list (no flicker) is dropped, not recorded.
        let log = concat!(
            r#"EXPLORE:STATE {"sig":"s1","labels":[]}"#,
            "\n",
            r#"EXPLORE:RERENDER {"from":"s1","action":"tap:key:id:bad","#,
            r#""churned":["id:hdr","id:nav"]}"#,
            "\n",
            r#"EXPLORE:RERENDER {"from":"s1","action":"tap:key:id:good","churned":[]}"#,
        );
        let obs = parse_run(log);
        assert_eq!(
            obs.rerenders.len(),
            1,
            "only the non-empty churn is recorded"
        );
        let churned = obs
            .rerenders
            .get(&("s1".to_string(), "tap:key:id:bad".to_string()))
            .expect("churn for the bad transition");
        assert_eq!(churned, &vec!["id:hdr".to_string(), "id:nav".to_string()]);
        assert!(
            !obs.rerenders
                .contains_key(&("s1".to_string(), "tap:key:id:good".to_string())),
            "the reconciled (empty-churn) transition is not a flicker"
        );
    }

    #[test]
    fn dupsubmit_marker_yields_keyed_method_url_count() {
        // The opt-in double-dispatch probe: EXPLORE:DUPSUBMIT carries the
        // duplicated (method, url) and how many times it fired, keyed by
        // (from, action). A record missing any field (here: no url) is dropped,
        // never half-recorded.
        let log = concat!(
            r#"EXPLORE:STATE {"sig":"s1","labels":[]}"#,
            "\n",
            r#"EXPLORE:DUPSUBMIT {"from":"s1","action":"tap:key:id:pay","#,
            r#""method":"POST","url":"https://app.example/api/orders","count":2}"#,
            "\n",
            r#"EXPLORE:DUPSUBMIT {"from":"s1","action":"tap:key:id:bad","#,
            r#""method":"POST","count":2}"#,
        );
        let obs = parse_run(log);
        assert_eq!(obs.duplicate_submits.len(), 1, "only the valid payload");
        let rec = obs
            .duplicate_submits
            .get(&("s1".to_string(), "tap:key:id:pay".to_string()))
            .expect("duplicate submit for the pay button");
        assert_eq!(
            rec,
            &(
                "POST".to_string(),
                "https://app.example/api/orders".to_string(),
                2
            )
        );
    }

    #[test]
    fn focusloss_marker_yields_keyed_pairs() {
        // The focus-loss oracle: EXPLORE:FOCUSLOSS is keyed by (from, action);
        // a repeat of the same pair dedupes (set semantics) and a record
        // missing the action is dropped.
        let log = concat!(
            r#"EXPLORE:STATE {"sig":"s1","labels":[]}"#,
            "\n",
            r#"EXPLORE:FOCUSLOSS {"from":"s1","action":"tap:key:id:add"}"#,
            "\n",
            r#"EXPLORE:FOCUSLOSS {"from":"s1","action":"tap:key:id:add"}"#,
            "\n",
            r#"EXPLORE:FOCUSLOSS {"from":"s1"}"#,
        );
        let obs = parse_run(log);
        assert_eq!(obs.focus_losses.len(), 1, "deduped, invalid dropped");
        assert!(obs
            .focus_losses
            .contains(&("s1".to_string(), "tap:key:id:add".to_string())));
    }

    #[test]
    fn flicker_marker_records_peak_divergence() {
        // The gated Tier-2 pixel oracle: EXPLORE:FLICKER carries the peak
        // transient-divergence magnitude, keyed by (from, action).
        let log = concat!(
            r#"EXPLORE:STATE {"sig":"s1","labels":[]}"#,
            "\n",
            r#"EXPLORE:FLICKER {"from":"s1","action":"tap:key:id:bad","peak":0.82,"frames":7}"#,
        );
        let obs = parse_run(log);
        let peak = obs
            .paint_flickers
            .get(&("s1".to_string(), "tap:key:id:bad".to_string()))
            .expect("paint flicker for the bad transition");
        assert!((peak - 0.82).abs() < 1e-9);
    }

    #[test]
    fn stuck_keyboard_marker_records_sig() {
        // The stuck-keyboard oracle: EXPLORE:STUCKKEYBOARD is emitted only on a
        // violation (IME visible, no editable focused), so presence of the sig
        // is the whole record.
        let log = concat!(
            r#"EXPLORE:STATE {"sig":"s1","labels":[]}"#,
            "\n",
            r#"EXPLORE:STUCKKEYBOARD {"sig":"s1","route":"/detail"}"#,
        );
        let obs = parse_run(log);
        assert!(obs.stuck_keyboards.contains("s1"));
        // A marker without a sig is dropped, never recorded as an empty key.
        let obs2 = parse_run(r#"EXPLORE:STUCKKEYBOARD {"route":"/detail"}"#);
        assert!(obs2.stuck_keyboards.is_empty());
    }

    #[test]
    fn rotation_and_bgrestore_markers_key_by_sig() {
        // The lifecycle-metamorphic oracles: EXPLORE:ROTATION / EXPLORE:BGRESTORE
        // carry the pre-transform structural sig (`expected`) and what survived
        // the transform (`got`), keyed by the state signature. A marker missing
        // any of sig/expected/got is dropped.
        let log = concat!(
            r#"EXPLORE:STATE {"sig":"s1","labels":[]}"#,
            "\n",
            r#"EXPLORE:ROTATION {"sig":"s1","route":"/detail","expected":"abc","got":"def"}"#,
            "\n",
            r#"EXPLORE:BGRESTORE {"sig":"s1","route":"/detail","expected":"abc","got":"xyz"}"#,
            "\n",
            r#"EXPLORE:ROTATION {"sig":"s2","expected":"only"}"#,
        );
        let obs = parse_run(log);
        assert_eq!(
            obs.rotation_losses.get("s1"),
            Some(&("abc".to_string(), "def".to_string()))
        );
        assert_eq!(
            obs.background_losses.get("s1"),
            Some(&("abc".to_string(), "xyz".to_string()))
        );
        // A marker missing `got` is dropped (never a half-recorded entry).
        assert!(!obs.rotation_losses.contains_key("s2"));
    }

    #[test]
    fn listenerleak_marker_keys_by_route() {
        // The listener-leak oracle: EXPLORE:LISTENERLEAK carries the per-metric
        // climb (kind, first, last) plus the revisit count, keyed by route. A
        // marker with an empty items list is dropped (silent when the route is
        // stable), and a marker without a route is ignored.
        let log = concat!(
            r#"EXPLORE:LISTENERLEAK {"route":"/detail","visits":5,"items":["#,
            r#"{"kind":"listeners","first":8,"last":40},"#,
            r#"{"kind":"nodes","first":120,"last":180}]}"#,
            "\n",
            r#"EXPLORE:LISTENERLEAK {"route":"/home","visits":5,"items":[]}"#,
            "\n",
            r#"EXPLORE:LISTENERLEAK {"visits":5,"items":["#,
            r#"{"kind":"listeners","first":1,"last":9}]}"#,
        );
        let obs = parse_run(log);
        let (visits, items) = obs.listener_leaks.get("/detail").expect("leak for /detail");
        assert_eq!(*visits, 5);
        assert_eq!(
            items,
            &vec![
                ("listeners".to_string(), 8, 40),
                ("nodes".to_string(), 120, 180),
            ]
        );
        assert!(
            !obs.listener_leaks.contains_key("/home"),
            "an empty listener-leak list is not recorded"
        );
        assert_eq!(
            obs.listener_leaks.len(),
            1,
            "a marker without a route is dropped"
        );
    }

    #[test]
    fn blankscreen_marker_keys_by_sig() {
        // The blank-screen (white-screen-of-death) oracle: EXPLORE:BLANKSCREEN
        // carries the scanned root + viewport size, keyed by state signature. A
        // marker with an empty items list is dropped (silent when clean).
        let log = concat!(
            r#"EXPLORE:STATE {"sig":"s1","labels":[]}"#,
            "\n",
            r#"EXPLORE:BLANKSCREEN {"sig":"s1","items":[{"key":"tag:body","w":1280,"h":720}]}"#,
            "\n",
            r#"EXPLORE:BLANKSCREEN {"sig":"s2","items":[]}"#,
        );
        let obs = parse_run(log);
        let items = obs.blank_screens.get("s1").expect("blank screen for s1");
        assert_eq!(items, &vec![("tag:body".to_string(), 1280, 720)]);
        assert!(
            !obs.blank_screens.contains_key("s2"),
            "an empty blank-screen list is not recorded"
        );
    }

    #[test]
    fn invariant_marker_keys_app_predicates_by_sig() {
        // The app-invariant oracle: EXPLORE:INVARIANT carries the app's own
        // predicate violations (id, message), keyed by state signature. A
        // marker with an empty items list is dropped (silent when all held),
        // and a missing message defaults to empty.
        let log = concat!(
            r#"EXPLORE:STATE {"sig":"s1","labels":[]}"#,
            "\n",
            r#"EXPLORE:INVARIANT {"sig":"s1","items":["#,
            r#"{"id":"cart total never negative","message":"total was -5"},"#,
            r#"{"id":"tab highlighted"}]}"#,
            "\n",
            r#"EXPLORE:INVARIANT {"sig":"s2","items":[]}"#,
        );
        let obs = parse_run(log);
        let items = obs.app_invariants.get("s1").expect("invariants for s1");
        assert_eq!(
            items,
            &vec![
                (
                    "cart total never negative".to_string(),
                    "total was -5".to_string()
                ),
                ("tab highlighted".to_string(), String::new()),
            ]
        );
        assert!(
            !obs.app_invariants.contains_key("s2"),
            "an empty invariant list is not recorded"
        );
    }

    #[test]
    fn safearea_marker_keys_collisions_by_sig() {
        // The safe-area oracle: EXPLORE:SAFEAREA carries the controls whose hit
        // rect intersects a device inset (key, edge, overlap px), keyed by state
        // signature. A marker with an empty items list is dropped (silent when no
        // control sits in an inset).
        let log = concat!(
            r#"EXPLORE:STATE {"sig":"s1","labels":[]}"#,
            "\n",
            r#"EXPLORE:SAFEAREA {"sig":"s1","items":["#,
            r#"{"key":"key:done","edge":"top","by":18},"#,
            r#"{"key":"key:next","edge":"bottom","by":6}]}"#,
            "\n",
            r#"EXPLORE:SAFEAREA {"sig":"s2","items":[]}"#,
        );
        let obs = parse_run(log);
        let items = obs.safe_areas.get("s1").expect("safe-area for s1");
        assert_eq!(
            items,
            &vec![
                ("key:done".to_string(), "top".to_string(), 18),
                ("key:next".to_string(), "bottom".to_string(), 6),
            ]
        );
        assert!(
            !obs.safe_areas.contains_key("s2"),
            "an empty safe-area list is not recorded"
        );
    }

    #[test]
    fn wakelock_marker_keys_leaks_by_sig() {
        // The wakelock-leak oracle: EXPLORE:WAKELOCK carries the wakelocks still
        // held after leaving a screen (tag, kind), keyed by the origin state
        // signature. A marker with an empty items list is dropped (silent when a
        // screen releases its locks on leaving).
        let log = concat!(
            r#"EXPLORE:STATE {"sig":"video","labels":[]}"#,
            "\n",
            r#"EXPLORE:WAKELOCK {"sig":"video","items":["#,
            r#"{"tag":"com.app:VideoPlayback","kind":"wakelock"},"#,
            r#"{"tag":"KEEP_SCREEN_ON","kind":"keep-screen-on"}]}"#,
            "\n",
            r#"EXPLORE:WAKELOCK {"sig":"home","items":[]}"#,
        );
        let obs = parse_run(log);
        let items = obs.wakelock_leaks.get("video").expect("leak for video");
        assert_eq!(
            items,
            &vec![
                ("com.app:VideoPlayback".to_string(), "wakelock".to_string()),
                ("KEEP_SCREEN_ON".to_string(), "keep-screen-on".to_string()),
            ]
        );
        assert!(
            !obs.wakelock_leaks.contains_key("home"),
            "an empty wakelock list is not recorded"
        );
    }

    #[test]
    fn permissionwalk_marker_records_permission_by_sig() {
        // The permission-walk oracle: EXPLORE:PERMISSIONWALK marks a screen
        // reached after a permission denial, keyed by state signature; the value
        // is the denied permission. A marker without both a sig and a permission
        // is dropped.
        let log = concat!(
            r#"EXPLORE:STATE {"sig":"s1","labels":[]}"#,
            "\n",
            r#"EXPLORE:PERMISSIONWALK {"sig":"s1","permission":"camera","route":"/scan"}"#,
        );
        let obs = parse_run(log);
        assert_eq!(
            obs.permission_screens.get("s1").map(String::as_str),
            Some("camera")
        );
        let obs2 = parse_run(r#"EXPLORE:PERMISSIONWALK {"sig":"s1"}"#);
        assert!(obs2.permission_screens.is_empty());
    }

    #[test]
    fn brokenasset_marker_keys_dead_assets_by_sig() {
        // The broken-asset oracle: EXPLORE:BROKENASSET carries the dead
        // subresources (key, reason, detail), keyed by state signature. A marker
        // with an empty items list is dropped (silent when every asset loads).
        let log = concat!(
            r#"EXPLORE:STATE {"sig":"s1","labels":[]}"#,
            "\n",
            r#"EXPLORE:BROKENASSET {"sig":"s1","items":["#,
            r#"{"key":"key:id:hero","reason":"img","detail":"missing.png"},"#,
            r#"{"key":"font:BrokeFont","reason":"font","detail":"BrokeFont"},"#,
            r#"{"key":"key:id:desc","reason":"tofu","detail":"glitch � here"}]}"#,
            "\n",
            r#"EXPLORE:BROKENASSET {"sig":"s2","items":[]}"#,
        );
        let obs = parse_run(log);
        let items = obs.broken_assets.get("s1").expect("broken assets for s1");
        assert_eq!(
            items,
            &vec![
                (
                    "key:id:hero".to_string(),
                    "img".to_string(),
                    "missing.png".to_string()
                ),
                (
                    "font:BrokeFont".to_string(),
                    "font".to_string(),
                    "BrokeFont".to_string()
                ),
                (
                    "key:id:desc".to_string(),
                    "tofu".to_string(),
                    "glitch \u{FFFD} here".to_string()
                ),
            ]
        );
        assert!(
            !obs.broken_assets.contains_key("s2"),
            "an empty broken-asset list is not recorded"
        );
    }

    #[test]
    fn auth_input_purpose_marker_contract_is_locale_and_backend_independent() {
        let log = concat!(
            "EXPLORE:STATE {\"sig\":\"web\",\"labels\":[\"Correo \
             electrónico\"],\"elements\":[{\"sel\":\"key:email\",\"role\":\"textfield\",\"label\":\
             \"Correo electrónico\",\"inputPurpose\":\"email-address\"}]}\n",
            "EXPLORE:STATE {\"sig\":\"native\",\"labels\":[\"Код \
             подтверждения\"],\"elements\":[{\"sel\":\"key:otp\",\
             \"role\":\"textfield\",\"label\":\
             \"Код подтверждения\",\"inputPurpose\":\"one-time-code\"}]}\n",
            "EXPLORE:STATE \
             {\"sig\":\"instrumented\",\"labels\":[],\"elements\":[{\"sel\":\"key:\
             reproit-purpose-phone--login\",\"role\":\"textfield\",\"label\":\"\"}]}\n"
        );
        let obs = parse_run(log);
        assert_eq!(
            obs.elements["web"][0].input_purpose.as_deref(),
            Some("email")
        );
        assert_eq!(
            obs.elements["native"][0].input_purpose.as_deref(),
            Some("otp")
        );
        assert_eq!(
            obs.elements["instrumented"][0].input_purpose.as_deref(),
            Some("phone")
        );
    }

    #[test]
    fn zoomreflow_marker_keys_breaks_by_sig() {
        // The zoom-reflow (WCAG 1.4.10) oracle: EXPLORE:ZOOMREFLOW carries the
        // reflow breaks (key, kind, by) measured at the zoomed viewport, keyed
        // by state signature. A marker with an empty items list is dropped
        // (silent when the route reflows cleanly).
        let log = concat!(
            r#"EXPLORE:STATE {"sig":"s1","labels":[]}"#,
            "\n",
            r#"EXPLORE:ZOOMREFLOW {"sig":"s1","items":["#,
            r#"{"key":"tag:html","kind":"hscroll","by":560},"#,
            r#"{"key":"key:id:save","kind":"collapsed","by":0}]}"#,
            "\n",
            r#"EXPLORE:ZOOMREFLOW {"sig":"s2","items":[]}"#,
        );
        let obs = parse_run(log);
        let items = obs.zoom_reflows.get("s1").expect("zoom reflow for s1");
        assert_eq!(
            items,
            &vec![
                ("tag:html".to_string(), "hscroll".to_string(), 560),
                ("key:id:save".to_string(), "collapsed".to_string(), 0),
            ]
        );
        assert!(
            !obs.zoom_reflows.contains_key("s2"),
            "an empty zoom-reflow list is not recorded"
        );
    }

    #[test]
    fn scrollroundtrip_marker_keys_diffs_by_sig() {
        // The scroll-round-trip oracle: EXPLORE:SCROLLROUNDTRIP carries the
        // per-offset (pos, before, after) content mismatches observed after
        // scrolling a list away and back, keyed by state signature. A marker
        // with an empty items list is dropped (silent when the list is stable).
        let log = concat!(
            r#"EXPLORE:STATE {"sig":"s1","labels":[]}"#,
            "\n",
            r#"EXPLORE:SCROLLROUNDTRIP {"sig":"s1","items":["#,
            r#"{"pos":"y=0","before":"Alpha|Bravo","after":"Charlie|Delta"}]}"#,
            "\n",
            r#"EXPLORE:SCROLLROUNDTRIP {"sig":"s2","items":[]}"#,
        );
        let obs = parse_run(log);
        let items = obs
            .scroll_round_trips
            .get("s1")
            .expect("scroll round trip for s1");
        assert_eq!(
            items,
            &vec![(
                "y=0".to_string(),
                "Alpha|Bravo".to_string(),
                "Charlie|Delta".to_string()
            )]
        );
        assert!(
            !obs.scroll_round_trips.contains_key("s2"),
            "an empty scroll-round-trip list is not recorded"
        );
    }

    #[test]
    fn appkit_in_process_agent_groundtruth_detects_fake_button_gap() {
        // End-to-end contract proof for the in-process AppKit operability agent
        // (runners/native/appkit-agent/main.swift). This is the VERBATIM
        // EXPLORE:GROUNDTRUTH line that the built+run Swift agent emits for a
        // window holding a real NSButton, a "fake button" (custom NSView with a
        // click gesture + handler and no a11y role), and a correctly-built
        // accessible custom control. The engine must score exactly one gap row
        // (the fake button), failing all three a11y dimensions. The marker lives
        // in tests/golden/operability/appkit.json; CI re-captures + diffs it.
        let g = gaps_from_golden("appkit");
        // The fake button alone is an operable-but-inaccessible element.
        assert_eq!(g.no_role, 1, "fake button has no a11y role");
        assert_eq!(
            g.keyboard_unreachable, 1,
            "fake button is not in the key-view loop"
        );
        assert_eq!(g.pointer_only, 1, "fake button is pointer-only (gesture)");
        assert!(!g.focus_trap);
    }

    #[test]
    fn wpf_in_process_agent_groundtruth_detects_fake_button_gap() {
        // End-to-end contract proof for the in-process WPF operability agent
        // (runners/native/wpf-agent/Program.cs). This is the VERBATIM
        // EXPLORE:GROUNDTRUTH line that the built+run agent emits on the Windows
        // VM for a window holding a real <Button> and a "fake button" (a
        // clickable <Border>/<TextBlock> with a MouseLeftButtonUp handler and no
        // Button role / no AutomationProperties). Graph 1 (visual tree + handler
        // reflection) and graph 2 (UIElementAutomationPeer) are joined by object
        // identity. The engine must score exactly one gap row (the fake button),
        // failing all three a11y dimensions; the real Button is clean. The marker
        // lives in tests/golden/operability/wpf.json; CI re-captures + diffs it.
        let g = gaps_from_golden("wpf");
        assert_eq!(g.no_role, 1, "fake button has no Button role");
        assert_eq!(
            g.keyboard_unreachable, 1,
            "fake button is not in the tab order"
        );
        assert_eq!(
            g.pointer_only, 1,
            "fake button is pointer-only (mouse handler)"
        );
        assert!(!g.focus_trap);
    }

    #[test]
    fn qt_in_process_agent_groundtruth_detects_fake_button_gap() {
        // End-to-end contract proof for the in-process Qt operability agent
        // (runners/native/qt-agent/qt_agent.cpp). This is the VERBATIM
        // EXPLORE:GROUNDTRUTH line the built+run agent emits on Linux
        // (Qt 6.8.2, `QT_QPA_PLATFORM=offscreen`) for a window
        // holding a real QPushButton, a "fake button" (custom QWidget with a
        // mousePressEvent handler and no QAccessible role), and a correctly-built
        // accessible control. Graph 1 (QObject tree + wired signals / custom
        // subclass) joins graph 2 (QAccessibleInterface) by object identity. The
        // engine must score exactly one gap row (the fake button), failing all
        // three a11y dimensions; the real button is clean. The signature matches
        // the AppKit agent's (3854aea0): same three-control structural descriptor.
        // The marker lives in tests/golden/operability/qt.json; CI re-captures it.
        let g = gaps_from_golden("qt");
        assert_eq!(g.no_role, 1, "fake button has no QAccessible role");
        assert_eq!(
            g.keyboard_unreachable, 1,
            "fake button is not in the tab order"
        );
        assert_eq!(
            g.pointer_only, 1,
            "fake button is pointer-only (mousePressEvent)"
        );
        assert!(!g.focus_trap);
    }

    #[test]
    fn gtk_in_process_agent_groundtruth_detects_fake_button_gap() {
        // End-to-end contract proof for the in-process GTK operability agent
        // (runners/native/gtk-agent/gtk_agent.c). This is the VERBATIM
        // EXPLORE:GROUNDTRUTH line the built+run agent emits on Linux
        // (GTK 4.18.6, under `xvfb-run`) for a window holding a real
        // GtkButton, a "fake button" (a GtkBox carrying a GtkGestureClick +
        // handler with no button role / not focusable), and a correctly-built
        // accessible GtkButton. Graph 1 (GtkWidget tree + wired signals / click
        // gestures) joins graph 2 (GtkAccessible role/state) by object identity.
        // The fake button is the motivating finding: operable yet rolePresent
        // false and keyboard-unreachable. GTK4 also surfaces the window's
        // built-in click gesture (role:group#0, a focusless operable element) and
        // the buttons' inner GtkLabel children (operable:false, never gaps); the
        // engine counts every operable-but-inaccessible element, so no_role==1
        // (the fake button alone has no role) while the two focusless operable
        // elements (window + fake button) drive keyboard_unreachable/pointer_only.
        // The marker lives in tests/golden/operability/gtk.json; CI re-captures it.
        let g = gaps_from_golden("gtk");
        // The fake button is the only operable element with no accessible role.
        assert_eq!(g.no_role, 1, "fake button alone has no GtkAccessible role");
        // Two operable elements lack focus/keyboard reachability: the fake button
        // and GTK4's window-level click gesture; the real + good buttons are clean.
        assert_eq!(g.keyboard_unreachable, 2);
        assert_eq!(g.pointer_only, 2);
        assert!(!g.focus_trap);
    }

    #[test]
    fn flutter_in_process_agent_groundtruth_detects_fake_button_gap() {
        // End-to-end contract proof for the in-process Flutter operability agent
        // (sdk/reproit_flutter/.../operability_fixture_test.dart's groundTruth()).
        // This is the VERBATIM EXPLORE:GROUNDTRUTH line `flutter test` emits for
        // the operability fixture: a real ElevatedButton (clean) and a "fake
        // button" (a bare GestureDetector(onTap:) wrapping Text). Flutter's
        // semantics DO give the gesture a synthetic button role (rolePresent:true,
        // gestureKind "tap"), so the gap is NOT no_role; the fake button is the
        // motivating finding because it is operable by pointer yet has no Focus, so
        // it is keyboard-unreachable AND not keyboard-activatable. The marker lives
        // in tests/golden/operability/flutter.json and is RE-CAPTURED by the CI
        // capture-diff job (`flutter test`); see .github/workflows/ci.yml.
        let g = gaps_from_golden("flutter");
        // Flutter exposes the gesture's button role, so there is no no_role gap.
        assert_eq!(g.no_role, 0, "flutter gives the gesture a button role");
        assert_eq!(
            g.keyboard_unreachable, 1,
            "fake button has no Focus -> not in the tab order"
        );
        assert_eq!(
            g.pointer_only, 1,
            "fake button is pointer-only (onTap, not keyboard-activatable)"
        );
        assert!(!g.focus_trap);
    }

    #[test]
    fn merge_backfills_route_on_a_known_state() {
        // First run had no route; a later run that reports one backfills it.
        let mut map = AppMap {
            app: "t".into(),
            version: 1,
            states: BTreeMap::new(),
            transitions: vec![],
            invariants: vec![],
            interrupts: vec![],
        };
        merge(
            &mut map,
            &parse_run(r#"EXPLORE:STATE {"sig":"abc","labels":[]}"#),
        );
        assert!(map
            .states
            .values()
            .next()
            .unwrap()
            .signature
            .route
            .is_none());
        merge(
            &mut map,
            &parse_run(r#"EXPLORE:STATE {"sig":"abc","route":"/home","labels":[]}"#),
        );
        assert_eq!(
            map.states
                .values()
                .next()
                .unwrap()
                .signature
                .route
                .as_deref(),
            Some("/home")
        );
    }

    #[test]
    fn read_all_device_logs_unions_every_actor() {
        let dir = std::env::temp_dir().join(format!("reproit-maplogs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("drive-a.log"), "EXPLORE:STATE a-line").unwrap();
        std::fs::write(dir.join("drive-b.log"), "EXPLORE:STATE b-line").unwrap();
        // A non-device file must be ignored.
        std::fs::write(dir.join("other.log"), "ignore me").unwrap();
        let joined = read_all_device_logs(&dir).unwrap();
        assert!(joined.contains("a-line"), "device a's log is included");
        assert!(joined.contains("b-line"), "device b's log is included");
        assert!(
            !joined.contains("ignore me"),
            "non-device logs are excluded"
        );
        // Sorted by name: a before b.
        assert!(joined.find("a-line").unwrap() < joined.find("b-line").unwrap());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn absorb_run_writes_map_files_to_documented_layout() {
        let root = std::env::temp_dir().join(format!(
            "reproit-map-layout-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let loaded = crate::config::parse_str(
            "app:\n  platform: web\n  bundleId: test.app\n  webRunnerDir: /tmp/web\n  \
             url: http://localhost:3000\n\
             devices:\n  namePrefix: test\n\
             journeys:\n  driver: web\n  doneMarkers:\n    - done\n",
            root.clone(),
        )
        .unwrap();

        absorb_run(
            &root,
            &loaded.config,
            concat!(
                r#"EXPLORE:STATE {"sig":"abc","route":"/home","labels":["Home"],"#,
                r#""elements":[{"sel":"key:testid:sign-in","role":"button","#,
                r#""label":"Sign in","bounds":[10,20,100,32]}],"#,
                r#""texts":[{"text":"Sign in","bounds":[22,28,44,14]}]}"#
            ),
        )
        .unwrap();

        assert!(
            crate::layout::appmap_path(&root).exists(),
            "app map should be under .reproit/map/"
        );
        assert!(
            crate::layout::visits_path(&root).exists(),
            "visits should be under .reproit/map/"
        );
        assert!(
            !root.join(".reproit/appmap.json").exists(),
            "old root app map should not be written"
        );
        assert!(
            !root.join(".reproit/visits.json").exists(),
            "old root visits should not be written"
        );
        let map = load_map(&root, &loaded.config);
        let state = map.states.values().next().unwrap();
        assert_eq!(state.elements.len(), 1);
        assert_eq!(state.elements[0].label, "Sign in");
        assert_eq!(state.elements[0].sel, "key:testid:sign-in");
        assert_eq!(state.elements[0].bounds, Some([10, 20, 100, 32]));
        assert_eq!(state.texts.len(), 1);
        assert_eq!(state.texts[0].text, "Sign in");
        assert_eq!(state.texts[0].bounds, Some([22, 28, 44, 14]));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn provenance_detects_real_inputs_and_ignores_build_output() {
        let root = std::env::temp_dir().join(format!(
            "reproit-map-provenance-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("target")).unwrap();
        std::fs::create_dir_all(root.join(".reproit/map")).unwrap();
        std::fs::write(root.join("src/app.ts"), "export const screen = 'home';").unwrap();
        std::fs::write(root.join("reproit.yaml"), "app: {}\n").unwrap();
        std::fs::write(appmap_path(&root), "{}").unwrap();

        stamp_map(&root).unwrap();
        assert_eq!(map_freshness(&root).unwrap(), MapFreshness::Current);

        let original_map = std::fs::read(appmap_path(&root)).unwrap();
        let snapshot = begin_full_rebuild(&root).unwrap();
        assert!(!appmap_path(&root).exists());
        std::fs::write(appmap_path(&root), "partial replacement").unwrap();
        restore_map(snapshot).unwrap();
        assert_eq!(std::fs::read(appmap_path(&root)).unwrap(), original_map);
        assert_eq!(map_freshness(&root).unwrap(), MapFreshness::Current);

        std::fs::write(root.join("target/generated.js"), "ignored").unwrap();
        assert_eq!(map_freshness(&root).unwrap(), MapFreshness::Current);

        std::fs::write(root.join("src/app.ts"), "export const screen = 'settings';").unwrap();
        assert_eq!(
            map_freshness(&root).unwrap(),
            MapFreshness::Stale(vec!["application source changed"])
        );

        stamp_map(&root).unwrap();
        std::fs::write(root.join("reproit.yaml"), "app: { platform: web }\n").unwrap();
        assert_eq!(
            map_freshness(&root).unwrap(),
            MapFreshness::Stale(vec!["reproit configuration changed"])
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn parses_only_supported_proven_relationship_violations() {
        let obs = parse_run(concat!(
            "EXPLORE:STATE {\"sig\":\"nav\",\"labels\":[\"Liked You\"]}\n",
            "EXPLORE:RELATION {\"sig\":\"nav\",\"items\":[",
            "{\"kind\":\"indicator-anchor\",\"dependentKey\":\"key:id:dot\",",
            "\"ownerKey\":\"key:id:liked\",\"containerKey\":\"key:id:tabs\",",
            "\"violation\":\"detached\",\"maxGap\":8,\"gap\":123.45},",
            "{\"kind\":\"guessed-red-dot\",\"dependentKey\":\"x\",",
            "\"ownerKey\":\"y\",\"containerKey\":\"z\",\"violation\":\"detached\"}]}",
        ));
        let items = obs.relations.get("nav").expect("proven relationship");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, "indicator-anchor");
        assert_eq!(items[0].dependent_key, "key:id:dot");
        assert_eq!(items[0].owner_key, "key:id:liked");
        assert_eq!(items[0].container_key, "key:id:tabs");
        assert_eq!(items[0].violation, "detached");
        assert_eq!(items[0].max_gap, 8);
        assert_eq!(items[0].gap_centipx, 12_345);
    }
}
