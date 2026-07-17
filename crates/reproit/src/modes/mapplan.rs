//! `reproit debug map semantic`: derive the candidate map from app source
//! with the LLM (offline, no simulator), reconcile against the verified map,
//! and report coverage. The LLM proposes; only `map --verify` or a driven run
//! ever promotes a candidate to verified, so nothing here is trusted as ground
//! truth. The output is a worklist, not an assertion target.

use crate::config;
use crate::model::appmap::AppMap;
use crate::model::candidate::{self, Candidate, CandidateMap, Confidence, GapReason, Status};
use crate::model::map;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Cap on source bytes fed to the LLM in one extraction (keeps the prompt
/// sane).
const MAX_SOURCE_BYTES: usize = 120_000;

pub async fn plan(loaded: &config::Loaded, quiet: bool) -> Result<CandidateMap> {
    let project = loaded.root.join(&loaded.config.app.project_dir);
    let source = gather_source(&project);

    let mut cm = if source.trim().is_empty() {
        if !quiet {
            eprintln!(
                "  warn: no Dart source under {}; candidate map is empty",
                project.display()
            );
        }
        CandidateMap {
            app: loaded.config.app.bundle_id.clone(),
            ..Default::default()
        }
    } else {
        match extract(&loaded.config, &source).await {
            Ok(mut c) => {
                if c.app.is_empty() {
                    c.app = loaded.config.app.bundle_id.clone();
                }
                c
            }
            Err(e) => {
                eprintln!("  warn: candidate extraction failed ({e}); writing empty candidate map");
                CandidateMap {
                    app: loaded.config.app.bundle_id.clone(),
                    ..Default::default()
                }
            }
        }
    };

    // Reconcile against whatever the simulator has verified so far.
    let verified = map::load_map(&loaded.root, &loaded.config);
    cm.reconcile(&verified);
    candidate::save(&loaded.root, &cm)?;

    if !quiet {
        print_coverage(&cm);
        eprintln!(
            "  candidate map -> {}",
            candidate::candidate_path(&loaded.root).display()
        );
    }
    Ok(cm)
}

/// Walk the project's `lib/` for `.dart` files, most-relevant first (routing,
/// navigation, screens, API), concatenated with a `// FILE:` header per file
/// and capped so one extraction stays within a sane prompt size.
fn gather_source(project: &Path) -> String {
    let lib = project.join("lib");
    let root = if lib.is_dir() {
        lib
    } else {
        project.to_path_buf()
    };
    let mut files: Vec<PathBuf> = Vec::new();
    collect_dart(&root, &mut files);
    // Routing/navigation/api/screen files first: they carry the most screens.
    files.sort_by_key(|p| {
        let s = p.to_string_lossy().to_lowercase();
        let score = ["rout", "nav", "screen", "page", "api", "app"]
            .iter()
            .filter(|k| s.contains(**k))
            .count();
        std::cmp::Reverse(score)
    });
    let mut out = String::new();
    for f in files {
        if out.len() >= MAX_SOURCE_BYTES {
            break;
        }
        if let Ok(content) = std::fs::read_to_string(&f) {
            let rel = f.strip_prefix(project).unwrap_or(&f);
            out.push_str(&format!("\n// FILE: {}\n", rel.display()));
            let room = MAX_SOURCE_BYTES.saturating_sub(out.len());
            if content.len() > room {
                out.push_str(&content[..room]);
                break;
            }
            out.push_str(&content);
        }
    }
    out
}

fn collect_dart(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            let name = e.file_name().to_string_lossy().to_lowercase();
            if matches!(name.as_str(), "build" | ".dart_tool" | "test" | "generated") {
                continue;
            }
            collect_dart(&p, out);
        } else if p.extension().and_then(|x| x.to_str()) == Some("dart")
            && !p.to_string_lossy().ends_with(".g.dart")
        {
            out.push(p);
        }
    }
}

/// Multi-lens prompt: instruct the LLM to read the source through several
/// lenses and UNION the screens each finds (recall over precision: the
/// simulator is the precision filter later), each tagged with evidence,
/// confidence and a gap reason.
async fn extract(cfg: &config::Config, source: &str) -> Result<CandidateMap> {
    let provider = llm::from_spec(&cfg.llm.to_spec())?;
    let prompt = format!(
        "You are mapping a mobile app's SCREENS from its source. Read it through these lenses and \
         UNION the screens each finds (bias to recall: a wrong guess is cheap to refute, a missed \
         screen is invisible):\n- routes: declared route/page tables\n- push: imperative \
         Navigator.push / context.push call sites\n- api: API client methods (an endpoint usually \
         backs a screen)\n- widgets: screen/page-shaped widgets\n\nFor each screen output an \
         object with: id (snake_case), purpose (short), evidence (array of \
         {{\"lens\":..,\"ref\":\"file:symbol\"}}), confidence (\"high\"|\"medium\"|\"low\"; high \
         only if anchored to a real route/push/api, low for a genre guess), route (the declared \
         route string or null), preconditions (array of short strings), reach_hint (how to \
         navigate there or null), and gap_reason \
         (\"none\"|\"needs_data\"|\"needs_peer\"|\"needs_login\"|\"frontier\": why a blind \
         single-user crawl might not reach it).\n\nReply with ONLY a JSON array of these objects: \
         no prose, no code fences.\n\nSOURCE:\n{source}"
    );
    let response = provider.complete(&llm::Task::new(prompt)).await?;
    let candidates = parse_candidates(&response)?;
    Ok(CandidateMap {
        app: String::new(),
        lenses: vec![
            "routes".into(),
            "push".into(),
            "api".into(),
            "widgets".into(),
        ],
        candidates,
    })
}

/// Extract the JSON array of candidates from an LLM response, tolerating code
/// fences or surrounding prose.
fn parse_candidates(response: &str) -> Result<Vec<Candidate>> {
    let start = response
        .find('[')
        .context("no JSON array in extraction response")?;
    let end = response
        .rfind(']')
        .context("no closing ] in extraction response")?;
    if end <= start {
        anyhow::bail!("malformed JSON array in extraction response");
    }
    serde_json::from_str(&response[start..=end]).context("parsing candidate array")
}

/// `reproit debug map coverage`: reconcile the candidate map against the
/// verified map and print the coverage ledger + the pending worklist. No LLM,
/// no simulator, pure reporting.
pub fn cover(loaded: &config::Loaded, json: bool) -> Result<()> {
    let Some(mut cm) = candidate::load(&loaded.root) else {
        if json {
            println!(
                "{}",
                serde_json::json!({
                    "command": "debug map coverage",
                    "error": "no candidate map; run `reproit debug map semantic` first",
                })
            );
        } else {
            eprintln!("  no candidate map yet; run `reproit debug map semantic` first");
        }
        return Ok(());
    };
    let verified = map::load_map(&loaded.root, &loaded.config);
    cm.reconcile(&verified);
    candidate::save(&loaded.root, &cm)?;
    if json {
        let mut v = coverage_json(&cm);
        v["command"] = "coverage".into();
        println!("{v}");
    } else {
        print_coverage(&cm);
        for c in cm.pending() {
            let route = c
                .route
                .as_deref()
                .map(|r| format!(" {r}"))
                .unwrap_or_default();
            println!("    - {} ({}){route}", c.id, c.gap_reason.as_str());
        }
    }
    Ok(())
}

/// The coverage ledger plus the pending worklist as JSON, shared by `--plan`,
/// `--cover`, and the MCP tool. Callers add a `command` field.
pub fn coverage_json(cm: &CandidateMap) -> serde_json::Value {
    let cov = cm.coverage();
    let worklist: Vec<_> = cm
        .pending()
        .iter()
        .map(|c| {
            serde_json::json!({
                "id": c.id,
                "gap": c.gap_reason.as_str(),
                "route": c.route,
                "reach_hint": c.reach_hint,
                "preconditions": c.preconditions,
            })
        })
        .collect();
    serde_json::json!({
        "declared": cov.declared,
        "verified": cov.verified,
        "pending": cov.pending,
        "by_gap": cov.by_gap,
        "worklist": worklist,
    })
}

// ---- closed-loop convergence --------------------------------------------

/// The result of trying to reach one candidate.
pub enum Validation {
    /// Reached; carries the real structural signature to record.
    Reached(String),
    /// Source-anchored but not reachable yet, with a typed reason.
    Unreached(GapReason),
    /// No source anchor and unreachable: a hallucination, drop it.
    Refuted,
}

/// Something that can attempt to validate a candidate. Abstracted so the loop
/// is testable without a simulator, and so a future active validator (drive the
/// sim on demand) can drop in without touching the loop.
pub trait Validator {
    fn validate(&mut self, c: &Candidate) -> Validation;
}

/// The passive validator: a candidate is "reached" iff some driven run has
/// already populated a matching state in the verified map (every run feeds it,
/// via universal recording). Sound, no sim needed here, the sim's work is
/// already in appmap.json. Unanchored guesses the map never reached are
/// refuted.
pub struct MapValidator<'a> {
    pub map: &'a AppMap,
}

impl Validator for MapValidator<'_> {
    fn validate(&mut self, c: &Candidate) -> Validation {
        if let Some(state) = c.find_in(self.map) {
            return Validation::Reached(state.signature.semantics_hash.clone().unwrap_or_default());
        }
        if !c.anchored() && c.confidence == Confidence::Low {
            return Validation::Refuted;
        }
        Validation::Unreached(c.gap_reason)
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ConvergeReport {
    pub rounds: usize,
    pub verified: usize,
    pub unreached: usize,
    pub refuted: usize,
}

/// The closed loop: keep validating not-yet-resolved candidates until nothing
/// new resolves. Guards against spinning and hallucination amplification:
///   * status IS the memory: Verified and Refuted are terminal (never retried,
///     so a refuted hypothesis can't reappear), Pending is tried once,
///     Unreached is retried ONLY after a round made progress (something else
///     verified, possibly unblocking it).
///   * loop-until-dry: stop after `dry_limit` consecutive rounds with no
///     progress; `max_rounds` is the hard backstop.
pub fn converge(
    cm: &mut CandidateMap,
    v: &mut dyn Validator,
    max_rounds: usize,
    dry_limit: usize,
) -> ConvergeReport {
    let mut dry = 0;
    let mut rounds = 0;
    let mut progressed_last = false;
    while rounds < max_rounds && dry < dry_limit {
        rounds += 1;
        let to_try: Vec<usize> = cm
            .candidates
            .iter()
            .enumerate()
            .filter(|(_, c)| match c.status {
                Status::Pending => true,
                Status::Unreached => progressed_last, // retry only after progress
                Status::Verified | Status::Hallucinated => false, // terminal
            })
            .map(|(i, _)| i)
            .collect();
        if to_try.is_empty() {
            break;
        }
        let mut progressed = false;
        for i in to_try {
            let c = cm.candidates[i].clone();
            match v.validate(&c) {
                Validation::Reached(sig) => {
                    cm.candidates[i].status = Status::Verified;
                    cm.candidates[i].gap_reason = GapReason::None;
                    cm.candidates[i].verified_sig = Some(sig);
                    progressed = true;
                }
                Validation::Unreached(gap) => {
                    cm.candidates[i].status = Status::Unreached;
                    cm.candidates[i].gap_reason = gap;
                }
                Validation::Refuted => {
                    cm.candidates[i].status = Status::Hallucinated;
                }
            }
        }
        progressed_last = progressed;
        if progressed {
            dry = 0;
        } else {
            dry += 1;
        }
    }

    let mut report = ConvergeReport {
        rounds,
        ..Default::default()
    };
    for c in &cm.candidates {
        match c.status {
            Status::Verified => report.verified += 1,
            Status::Unreached => report.unreached += 1,
            Status::Hallucinated => report.refuted += 1,
            Status::Pending => {}
        }
    }
    report
}

/// `reproit debug map converge`: run the loop against the verified map
/// validator), prune hallucinations, and report. The LLM/sim stay out: this
/// reconciles accumulated verified reality and converges as more runs feed the
/// map. An active validator (drive the sim per candidate) plugs into
/// `Validator` later without changing the loop.
pub fn converge_cmd(loaded: &config::Loaded, json: bool) -> Result<()> {
    let Some(mut cm) = candidate::load(&loaded.root) else {
        if !json {
            eprintln!("  no candidate map yet; run `reproit debug map semantic` first");
        }
        return Ok(());
    };
    let verified = map::load_map(&loaded.root, &loaded.config);
    let mut validator = MapValidator { map: &verified };
    let report = converge(&mut cm, &mut validator, 20, 2);
    candidate::save(&loaded.root, &cm)?;
    if json {
        let mut v = coverage_json(&cm);
        v["command"] = "map converge".into();
        v["rounds"] = report.rounds.into();
        v["refuted"] = report.refuted.into();
        println!("{v}");
    } else {
        println!(
            "  converged in {} round(s): {} verified, {} unreached, {} refuted",
            report.rounds, report.verified, report.unreached, report.refuted
        );
        print_coverage(&cm);
    }
    Ok(())
}

fn print_coverage(cm: &CandidateMap) {
    let cov = cm.coverage();
    println!(
        "  coverage: {} declared, {} verified, {} pending",
        cov.declared, cov.verified, cov.pending
    );
    for (gap, n) in &cov.by_gap {
        println!("    pending {gap}: {n}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_candidates_from_fenced_response() {
        let resp = r#"Here you go:
```json
[ {"id":"home","purpose":"feed","route":"/home","confidence":"high",
   "evidence":[{"lens":"routes","ref":"router.dart:10"}],"gap_reason":"none"},
  {"id":"chat","route":"/chat","confidence":"high","gap_reason":"needs_peer"} ]
```
done"#;
        let cands = parse_candidates(resp).unwrap();
        assert_eq!(cands.len(), 2);
        assert_eq!(cands[0].id, "home");
        assert_eq!(cands[0].route.as_deref(), Some("/home"));
        assert_eq!(cands[1].id, "chat");
    }

    #[test]
    fn parse_errors_when_no_array() {
        assert!(parse_candidates("sorry, no JSON here").is_err());
    }

    fn mk(id: &str) -> Candidate {
        Candidate {
            id: id.into(),
            purpose: String::new(),
            evidence: vec![],
            confidence: Confidence::High,
            route: Some(format!("/{id}")),
            preconditions: vec![],
            reach_hint: None,
            verified_sig: None,
            status: Status::Pending,
            gap_reason: GapReason::None,
        }
    }

    #[test]
    fn converge_resolves_with_unblock_after_progress() {
        // chat only becomes reachable once beacon_detail is reached, so it must
        // be retried in a later round (after progress), not abandoned.
        struct Mock {
            reached: std::collections::HashSet<String>,
        }
        impl Validator for Mock {
            fn validate(&mut self, c: &Candidate) -> Validation {
                match c.id.as_str() {
                    "home" => Validation::Reached("h1".into()),
                    "beacon_detail" => {
                        self.reached.insert("beacon_detail".into());
                        Validation::Reached("b1".into())
                    }
                    "chat" => {
                        if self.reached.contains("beacon_detail") {
                            Validation::Reached("c1".into())
                        } else {
                            Validation::Unreached(GapReason::NeedsPeer)
                        }
                    }
                    _ => Validation::Refuted,
                }
            }
        }
        // chat listed BEFORE beacon_detail, so round 1 can't reach it.
        let mut cm = CandidateMap {
            candidates: vec![mk("home"), mk("chat"), mk("beacon_detail"), mk("ghost")],
            ..Default::default()
        };
        let mut v = Mock {
            reached: Default::default(),
        };
        let r = converge(&mut cm, &mut v, 20, 2);
        assert_eq!(r.verified, 3, "home + beacon_detail + chat");
        assert_eq!(r.refuted, 1, "ghost");
        assert_eq!(r.unreached, 0);
        let chat = cm.candidates.iter().find(|c| c.id == "chat").unwrap();
        assert_eq!(chat.status, Status::Verified);
        assert_eq!(chat.verified_sig.as_deref(), Some("c1"));
    }

    #[test]
    fn converge_terminates_when_stuck_and_does_not_spin() {
        struct Stuck;
        impl Validator for Stuck {
            fn validate(&mut self, _c: &Candidate) -> Validation {
                Validation::Unreached(GapReason::Frontier)
            }
        }
        let mut cm = CandidateMap {
            candidates: vec![mk("a"), mk("b")],
            ..Default::default()
        };
        let r = converge(&mut cm, &mut Stuck, 20, 2);
        assert_eq!(r.unreached, 2);
        assert_eq!(r.verified, 0);
        assert!(r.rounds <= 2, "must stop quickly, not spin to max_rounds");
    }
}
