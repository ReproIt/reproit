//! The candidate (hypothesized) map: the LLM's source-derived guess at every
//! screen the app should have, before the simulator has confirmed any of it.
//!
//! Two tiers, kept strictly apart:
//!   * the VERIFIED map (`appmap.json`): only sim-confirmed reality. Sound.
//!   * the CANDIDATE map (`candidate_map.json`, here): the LLM's hypotheses,
//!     each tagged with the source evidence it came from. Complete-ish, unsound
//!     until verified.
//!
//! Reconciliation matches candidates against the verified map and computes a
//! coverage ledger, so "not fully mapped" is an explicit, attributed list
//! (this screen needs data, that one needs a peer) rather than a silent gap.
//! Nothing downstream trusts a candidate: the LLM proposes, the simulator
//! disposes. The candidate map is a worklist, never an assertion target.

#![allow(dead_code)]

use crate::layout;
use crate::model::appmap::AppMap;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// How much the LLM trusts a candidate, driven by its evidence: source-anchored
/// (a real route/push site) is `high`; a genre prior ("apps like this have
/// settings") with no anchor is `low` and is discarded if the sim refutes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    High,
    Medium,
    Low,
}

/// Where a candidate stands against the verified map.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Hypothesized, not yet reached by the simulator.
    #[default]
    Pending,
    /// The simulator reached it; matched to a verified state.
    Verified,
    /// Source-anchored but the simulator couldn't reach it (see `gap_reason`).
    Unreached,
    /// No source anchor and the simulator refuted it: drop it.
    Hallucinated,
}

/// Why a pending/unreached candidate isn't verified yet. Drives the
/// auto-unblock in the converge loop: each reason maps to a mechanism (seed,
/// dual-user, scripted login) that opens that region.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum GapReason {
    #[default]
    None,
    /// A precondition record must exist (a beacon, a post): seed it.
    NeedsData,
    /// Needs a second logged-in actor (a live conversation): dual-user journey.
    NeedsPeer,
    /// Behind authentication: scripted login prelude.
    NeedsLogin,
    /// Reachable in principle but the crawl never expanded that edge.
    Frontier,
}

impl GapReason {
    pub fn as_str(self) -> &'static str {
        match self {
            GapReason::None => "none",
            GapReason::NeedsData => "needs_data",
            GapReason::NeedsPeer => "needs_peer",
            GapReason::NeedsLogin => "needs_login",
            GapReason::Frontier => "frontier",
        }
    }
}

/// One source artifact that implies a screen exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    /// routes | push | api | widgets | tests | genre
    pub lens: String,
    /// file:line or symbol the screen was inferred from.
    #[serde(rename = "ref")]
    pub reference: String,
}

fn default_confidence() -> Confidence {
    Confidence::Medium
}

/// A hypothesized screen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    /// Stable snake_case id, matched against verified state names.
    pub id: String,
    #[serde(default)]
    pub purpose: String,
    #[serde(default)]
    pub evidence: Vec<Evidence>,
    #[serde(default = "default_confidence")]
    pub confidence: Confidence,
    /// Declared route/page identity, when the app exposes one (the strongest
    /// match key against the verified map's `signature.route`).
    #[serde(default)]
    pub route: Option<String>,
    /// What must be true to reach it (free text, mirrored by `gap_reason`).
    #[serde(default)]
    pub preconditions: Vec<String>,
    /// A hint for the validator on how to get here ("from beacons, tap a
    /// card").
    #[serde(default)]
    pub reach_hint: Option<String>,
    /// Filled when validated: the real structural signature the sim captured.
    #[serde(default)]
    pub verified_sig: Option<String>,
    #[serde(default)]
    pub status: Status,
    #[serde(default)]
    pub gap_reason: GapReason,
}

impl Candidate {
    /// Whether a verified state corresponds to this candidate. Matched, in
    /// descending reliability, by: captured signature, declared route, or id
    /// equal to the state's name (states are named only after the labeling
    /// pass, so this is the weakest key).
    fn matches(&self, name: &str, state: &crate::model::appmap::State) -> bool {
        if let (Some(sig), Some(hash)) = (&self.verified_sig, &state.signature.semantics_hash) {
            if sig == hash {
                return true;
            }
        }
        if let (Some(route), Some(sroute)) = (&self.route, &state.signature.route) {
            if route == sroute {
                return true;
            }
        }
        name == self.id || name == format!("s_{}", self.id)
    }

    /// The verified state this candidate corresponds to, if the simulator has
    /// reached it. Shared by reconciliation and the converge validator.
    pub fn find_in<'a>(&self, map: &'a AppMap) -> Option<&'a crate::model::appmap::State> {
        map.states
            .iter()
            .find(|(name, st)| self.matches(name, st))
            .map(|(_, s)| s)
    }

    /// Whether the candidate is anchored to real source (a route or evidence),
    /// vs a genre prior. Anchored failures are "real but unreached"; unanchored
    /// ones the simulator can't find are hallucinations.
    pub fn anchored(&self) -> bool {
        self.route.is_some() || !self.evidence.is_empty()
    }
}

/// The whole candidate map.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CandidateMap {
    #[serde(default)]
    pub app: String,
    /// Which extraction lenses produced this (routes, push, api, ...).
    #[serde(default)]
    pub lenses: Vec<String>,
    #[serde(default)]
    pub candidates: Vec<Candidate>,
}

/// The honest coverage ledger: how much of the candidate map the simulator has
/// confirmed, and why the rest is still pending.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Coverage {
    pub declared: usize,
    pub verified: usize,
    pub pending: usize,
    /// Count of pending candidates per gap reason (snake_case key).
    pub by_gap: BTreeMap<String, usize>,
}

impl CandidateMap {
    /// Mark candidates verified when a verified state matches them. Returns how
    /// many newly flipped to verified this pass. Pure: no LLM, no sim.
    pub fn reconcile(&mut self, map: &AppMap) -> usize {
        let mut newly = 0;
        for c in &mut self.candidates {
            if c.status == Status::Verified {
                continue;
            }
            if let Some((_, state)) = map.states.iter().find(|(name, st)| c.matches(name, st)) {
                c.status = Status::Verified;
                c.gap_reason = GapReason::None;
                if c.verified_sig.is_none() {
                    c.verified_sig = state.signature.semantics_hash.clone();
                }
                newly += 1;
            }
        }
        newly
    }

    /// The coverage ledger after the current reconciliation state.
    pub fn coverage(&self) -> Coverage {
        let mut cov = Coverage {
            declared: self.candidates.len(),
            ..Default::default()
        };
        for c in &self.candidates {
            if c.status == Status::Verified {
                cov.verified += 1;
            } else {
                cov.pending += 1;
                *cov.by_gap
                    .entry(c.gap_reason.as_str().to_string())
                    .or_default() += 1;
            }
        }
        cov
    }

    /// The pending worklist (everything not yet verified), the input the agent
    /// or the converge loop drives down to zero.
    pub fn pending(&self) -> Vec<&Candidate> {
        self.candidates
            .iter()
            .filter(|c| c.status != Status::Verified)
            .collect()
    }
}

/// `.reproit/map/candidate_map.json`.
pub fn candidate_path(root: &Path) -> PathBuf {
    layout::candidate_map_path(root)
}

pub fn load(root: &Path) -> Option<CandidateMap> {
    let raw = std::fs::read_to_string(candidate_path(root)).ok()?;
    serde_json::from_str(&raw).ok()
}

pub fn save(root: &Path, map: &CandidateMap) -> Result<()> {
    let path = candidate_path(root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(map).context("serializing candidate map")?;
    std::fs::write(&path, json).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_map_with(states: &[(&str, Option<&str>, Option<&str>)]) -> AppMap {
        let mut m = serde_json::json!({
            "app": "t", "version": 1, "states": {}, "transitions": [], "invariants": []
        });
        let obj = m["states"].as_object_mut().unwrap();
        for (name, hash, route) in states {
            obj.insert(
                name.to_string(),
                serde_json::json!({
                    "description": "d",
                    "signature": {
                        "screenshot_phash": null,
                        "semantics_hash": hash,
                        "route": route
                    },
                }),
            );
        }
        serde_json::from_value(m).unwrap()
    }

    fn cand(id: &str, route: Option<&str>, gap: GapReason) -> Candidate {
        Candidate {
            id: id.to_string(),
            purpose: String::new(),
            evidence: vec![],
            confidence: Confidence::High,
            route: route.map(String::from),
            preconditions: vec![],
            reach_hint: None,
            verified_sig: None,
            status: Status::Pending,
            gap_reason: gap,
        }
    }

    #[test]
    fn reconcile_matches_by_route_then_marks_verified() {
        let map = app_map_with(&[("home", Some("h1"), Some("/home"))]);
        let mut cm = CandidateMap {
            candidates: vec![
                cand("home", Some("/home"), GapReason::None), // matches by route
                cand("beacon_detail", Some("/beacon/:id"), GapReason::NeedsData), // no state
            ],
            ..Default::default()
        };
        let newly = cm.reconcile(&map);
        assert_eq!(newly, 1);
        assert_eq!(cm.candidates[0].status, Status::Verified);
        assert_eq!(cm.candidates[0].verified_sig.as_deref(), Some("h1"));
        assert_eq!(cm.candidates[1].status, Status::Pending);
    }

    #[test]
    fn reconcile_matches_by_id_name() {
        let map = app_map_with(&[("profile", None, None)]);
        let mut cm = CandidateMap {
            candidates: vec![cand("profile", None, GapReason::None)],
            ..Default::default()
        };
        assert_eq!(cm.reconcile(&map), 1);
    }

    #[test]
    fn coverage_groups_pending_by_gap() {
        let map = app_map_with(&[("home", Some("h1"), Some("/home"))]);
        let mut cm = CandidateMap {
            candidates: vec![
                cand("home", Some("/home"), GapReason::None),
                cand("beacon_detail", Some("/b"), GapReason::NeedsData),
                cand("chat", Some("/c"), GapReason::NeedsPeer),
                cand("settings", Some("/s"), GapReason::Frontier),
            ],
            ..Default::default()
        };
        cm.reconcile(&map);
        let cov = cm.coverage();
        assert_eq!(cov.declared, 4);
        assert_eq!(cov.verified, 1);
        assert_eq!(cov.pending, 3);
        assert_eq!(cov.by_gap.get("needs_data"), Some(&1));
        assert_eq!(cov.by_gap.get("needs_peer"), Some(&1));
        assert_eq!(cov.by_gap.get("frontier"), Some(&1));
        assert_eq!(cm.pending().len(), 3);
    }

    #[test]
    fn candidate_map_round_trips_json() {
        let cm = CandidateMap {
            app: "example".into(),
            lenses: vec!["routes".into()],
            candidates: vec![cand("home", Some("/home"), GapReason::None)],
        };
        let json = serde_json::to_string(&cm).unwrap();
        let back: CandidateMap = serde_json::from_str(&json).unwrap();
        assert_eq!(back.candidates.len(), 1);
        assert_eq!(back.candidates[0].id, "home");
    }

    #[test]
    fn save_writes_candidate_map_to_documented_layout() {
        let root = std::env::temp_dir().join(format!(
            "reproit-candidate-layout-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        let cm = CandidateMap {
            app: "example".into(),
            lenses: vec!["routes".into()],
            candidates: vec![cand("home", Some("/home"), GapReason::None)],
        };
        save(&root, &cm).unwrap();

        assert!(
            crate::layout::candidate_map_path(&root).exists(),
            "candidate map should be under .reproit/map/"
        );
        assert!(
            !root.join(".reproit/candidate_map.json").exists(),
            "old root candidate map should not be written"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
