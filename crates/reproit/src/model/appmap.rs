//! The app model: the single schema shared by all modes (flows, fuzz, soak).
//! Built by the LLM during exploration, verified and refined empirically by
//! the runner. Types only for now; the authoring agent and fuzzer populate
//! and consume these.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Serialize, Deserialize)]
pub struct AppMap {
    pub app: String,
    /// Bumped whenever states/transitions change; fuzz seeds are only
    /// replayable against the same model version.
    pub version: u32,
    pub states: BTreeMap<String, State>,
    pub transitions: Vec<Transition>,
    pub invariants: Vec<Invariant>,
    /// One-off overlays (dialogs, toasts, notifications) layered over the
    /// base graph. Checked on every settle, before state matching.
    #[serde(default)]
    pub interrupts: Vec<Interrupt>,
}

/// A named, recognizable configuration of the UI. States are screen
/// TEMPLATES: profile-of-Alice and profile-of-Bob are one state with a
/// `user` parameter. Signatures hash structure, not content; content binds
/// to parameters. Otherwise the graph explodes.
#[derive(Debug, Serialize, Deserialize)]
pub struct State {
    pub description: String,
    pub signature: StateSignature,
    /// Parameter names this screen template binds (e.g. "user", "item_id").
    #[serde(default)]
    pub parameters: Vec<String>,
    /// Tappable semantics nodes with no label observed on this screen:
    /// the a11y fix class's findings.
    #[serde(default)]
    pub unlabeled_tappables: u32,
    /// Operability/accessibility gaps on this screen: the diff between the
    /// ground-truth operable set (graph 1, what a pointer user can do) and the
    /// accessibility/keyboard-operable set (graph 2). Populated from a runner's
    /// `EXPLORE:GROUNDTRUTH` records; see docs/operability-graph.md.
    #[serde(default)]
    pub operability_gaps: OperabilityGaps,
}

/// Per-screen operability/accessibility gap counts (graph 1 minus graph 2).
/// Every field is the count of ground-truth-operable elements that fail a
/// specific accessibility dimension, so an all-zero value is the healthy case.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OperabilityGaps {
    /// Operable by pointer but not reachable/operable by keyboard (WCAG 2.1.1).
    #[serde(default)]
    pub pointer_only: u32,
    /// Operable by pointer but not reachable by the tab/focus order.
    #[serde(default)]
    pub keyboard_unreachable: u32,
    /// Operable but exposes no programmatic role/name to AT (WCAG 4.1.2).
    #[serde(default)]
    pub no_role: u32,
    /// A focus trap was observed on this screen.
    #[serde(default)]
    pub focus_trap: bool,
    /// Per-element gap detail: which ground-truth-operable element (by reproit
    /// selector) failed which accessibility dimension(s). This is what makes the
    /// diff GROUNDED and actionable (a coordinate the agent can act on) rather
    /// than a bare count. Empty on older maps that only recorded counts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<OperabilityGap>,
}

/// One ground-truth-operable element that is missing from the accessibility
/// graph in one or more ways. `selector` is reproit's existing selector grammar
/// (the same join key the runner emits), so the agent can address it directly.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OperabilityGap {
    pub selector: String,
    /// The failed dimensions: any of `pointer_only`, `keyboard_unreachable`,
    /// `no_role` (a single element can fail more than one).
    pub kinds: Vec<String>,
}

/// An interrupt is NOT a state: it can appear on top of any state. The
/// runner recognizes it by signature and applies the policy; the fuzzer may
/// also inject interrupts deliberately as an action.
#[derive(Debug, Serialize, Deserialize)]
pub struct Interrupt {
    pub id: String,
    pub description: String,
    pub signature: StateSignature,
    pub policy: InterruptPolicy,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterruptPolicy {
    /// Noise (rating prompts, tooltips): tap the given finder to clear it.
    Dismiss { finder: String },
    /// Required to proceed (permission dialogs handled in-app).
    Accept { finder: String },
    /// Flow-relevant (a match dialog): treat as a real state of this name.
    Promote { state: String },
}

/// How a state is recognized: settled screenshot + semantics fingerprint.
#[derive(Debug, Serialize, Deserialize)]
pub struct StateSignature {
    /// Perceptual hash of the settled screenshot.
    pub screenshot_phash: Option<String>,
    /// Hash over the semantics tree shape (labels + roles, order-sensitive).
    pub semantics_hash: Option<String>,
    /// Route/page identity when the app exposes one.
    pub route: Option<String>,
}

/// An action from one state to another.
#[derive(Debug, Serialize, Deserialize)]
pub struct Transition {
    pub from: String,
    pub to: String,
    pub action: Action,
    /// Conditions required to take this edge (logged in, item exists, role).
    #[serde(default)]
    pub guards: Vec<String>,
    pub reversibility: Reversibility,
    /// Expected visual signature: diff confined to a bounding box, settled
    /// within a bound. Numeric checks, no LLM in the loop.
    pub expected: Option<TransitionExpectation>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    Tap { finder: String },
    Type { finder: String, text: String },
    Scroll { finder: String, dy: i32 },
    Back,
    System { event: String },
}

/// Reversibility is proposed by the LLM and verified empirically: an edge is
/// reversible iff some path returns to a state whose signature matches the
/// pre-edge state. Irreversible edges require a state checkpoint to cross.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reversibility {
    /// LLM-proposed, not yet verified by the runner.
    ProposedReversible,
    ProposedIrreversible,
    /// Empirically confirmed by walk-back signature comparison.
    VerifiedReversible,
    VerifiedIrreversible,
    /// Marked destructive (money, deletion, messages to real users):
    /// never taken by the fuzzer without a checkpoint, regardless of proposal.
    Destructive,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TransitionExpectation {
    /// Diff must be confined to this box (x, y, w, h) on the settled frame.
    pub diff_bbox: Option<[u32; 4]>,
    /// UI must settle (two consecutive frames match) within this bound.
    pub settle_ms: u32,
}

/// A property that must hold. Global ones are compiled checks; app-specific
/// ones are LLM-proposed and human-confirmed before they gate anything.
#[derive(Debug, Serialize, Deserialize)]
pub struct Invariant {
    pub id: String,
    pub description: String,
    pub scope: InvariantScope,
    /// Human confirmed: only confirmed invariants fail runs.
    #[serde(default)]
    pub confirmed: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvariantScope {
    Global,
    State {
        state: String,
    },
    Transition {
        from: String,
        to: String,
    },
    /// Soak-mode: a reversible cycle must be resource-neutral after GC.
    Cycle {
        states: Vec<String>,
    },
}
