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
