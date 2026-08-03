//! Linux desktop runner (AT-SPI2 backend), dispatched as `reproit __atspi` by
//! drive.rs. The Linux twin of the macOS `swift macos-ax.swift` and the Windows
//! `reproit __uia` runners: it drives ANY native Linux app (GTK, Qt, and any
//! toolkit that publishes to AT-SPI) through the accessibility tree and prints
//! the framework-agnostic marker protocol every backend emits.
//!
//! Oracle exclusions (documented ground-truth gaps): the SAFE-AREA oracle does
//! not run here -- a desktop window has no device safe-area inset, so there is
//! no inset geometry to measure. The PERMISSION-WALK oracle does not run here
//! either -- a desktop app has no runtime OS permission the runner can DENY, so
//! there is no denial sweep.
//!
//! This is an in-process port of the former runners/linux-atspi.py. It binds
//! the OFFICIAL AT-SPI C library (libatspi.so.0, the exact library the Python
//! `gi` / `Atspi` binding wrapped) directly via hand-declared `#[link]` FFI,
//! and REUSES the canonical signature core (crate::domain::signature) instead of
//! re-implementing it, so there is exactly one signature oracle in the binary.
//! Localized name/text NEVER enters the hash; it is kept only as a display-only
//! label list.
//!
//! Env (set by drive.rs):
//!   REPROIT_TARGET             app name substring, or path to launch
//!   REPROIT_FUZZ_CONFIG        fuzz config json (single {seed,...} or
//! {batch:[...]})   REPROIT_SCENARIO_BARRIER   conductor base URL for a
//! multi-actor scenario   REPROIT_SHOTS_DIR          where a `shoot:` step
//! writes <name>.png   REPROIT_DEVICE             this actor's role label
//! (scenario mode)

use anyhow::{Context, Result};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ffi::{CStr, CString};
use std::io::{Read, Write};
use std::os::raw::{c_char, c_int, c_long, c_void};
use std::ptr;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use regex::Regex;

use crate::domain::signature::{
    apply_value_nodes, content_fingerprint, signature, structural_only, value_class, Node, ValueCap,
};

const ACTION_BUDGET: u32 = 36;
const MAX_LABEL_LEN: usize = 40;
const MAX_LABELS_PER_STATE: usize = 24;
const HANG_FLOOR_MS: u64 = 2000;

mod actions;
mod capture;
mod protocol;
mod session;

use actions::*;
use capture::*;
use protocol::*;
use session::*;

pub fn run() -> Result<()> {
    let target = std::env::var("REPROIT_TARGET")
        .ok()
        .filter(|s| !s.is_empty())
        .context("REPROIT_TARGET (app name or launch path) required")?;

    let scenario_base = std::env::var("REPROIT_SCENARIO_BARRIER")
        .ok()
        .filter(|s| !s.is_empty());
    if scenario_base.is_none() {
        emit("JOURNEY claimed role=a");
    }

    unsafe {
        atspi_init();
    }

    let desktop = Acc::from_owned(unsafe { atspi_get_desktop(0) })
        .context("atspi_get_desktop(0) returned null (is the a11y bus running?)")?;

    // App-invariant scrape: only a child WE launch exposes a stderr we can pipe
    // (attaching to an already-running app by name does not), so this is set only
    // on the launch-by-path branch below.
    let mut invariant_scrape: Option<InvariantScrape> = None;

    // Launch if it looks like a path, then bind by pid (scenario) or by name.
    let app: Acc = {
        let looks_like_path =
            target.contains(std::path::MAIN_SEPARATOR) && std::path::Path::new(&target).exists();
        if looks_like_path {
            let mut child = std::process::Command::new(&target)
                // Pipe stderr so we can scrape the SDK's REPROIT_INVARIANT markers,
                // and gate the SDK on: seeing REPROIT_UNDER_FUZZER it evaluates its
                // invariant registry (inert without it, in production).
                .stderr(std::process::Stdio::piped())
                .env("REPROIT_UNDER_FUZZER", "1")
                .spawn()
                .with_context(|| format!("launching {target}"))?;
            if let Some(stderr) = child.stderr.take() {
                invariant_scrape = Some(InvariantScrape::spawn(stderr));
            }
            std::thread::sleep(Duration::from_millis(2500));
            let by_pid = if scenario_base.is_some() {
                find_app_by_pid(&desktop, child.id())
            } else {
                None
            };
            let base = std::path::Path::new(&target)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| target.clone());
            match by_pid.or_else(|| find_app_by_name(&desktop, &base)) {
                Some(a) => a,
                None => {
                    crash(
                        "target not found",
                        &format!("no AT-SPI application matching {target:?}"),
                    );
                    std::process::exit(3);
                }
            }
        } else {
            match find_app_by_name(&desktop, &target) {
                Some(a) => a,
                None => {
                    crash(
                        "target not found",
                        &format!("no AT-SPI application matching {target:?}"),
                    );
                    std::process::exit(3);
                }
            }
        }
    };
    std::thread::sleep(Duration::from_secs(1));

    let target_pid = acc_pid(&app);
    let value_selectors = load_value_node_selectors();
    let mut cap = ValueCap::new();

    if let Some(base) = scenario_base {
        return run_scenario_actor(&app, &value_selectors, &mut cap, &base);
    }

    let (batch, is_batch) = load_batch();
    let mut any_crash = false;
    for fuzz in &batch {
        if is_batch {
            reset_to_root();
            let seed = fuzz.get("seed").and_then(|v| v.as_u64()).unwrap_or(0);
            emit(&format!("SEED:BEGIN {seed}"));
        }
        any_crash |= run_seed(
            &app,
            &value_selectors,
            &mut cap,
            target_pid,
            fuzz,
            invariant_scrape.as_mut(),
        );
        if is_batch {
            let seed = fuzz.get("seed").and_then(|v| v.as_u64()).unwrap_or(0);
            emit(&format!("SEED:END {seed}"));
        }
        // A dead target cannot be driven further by later seeds in the batch.
        if any_crash {
            break;
        }
    }

    emit("JOURNEY DONE");
    emit(if any_crash {
        "Some tests failed"
    } else {
        "All tests passed"
    });
    Ok(())
}

// Read the optional `value_nodes:` selector list from reproit.yaml (Layer 3).
fn load_value_node_selectors() -> Vec<String> {
    let path = std::env::var("REPROIT_CONFIG").unwrap_or_else(|_| {
        std::env::current_dir()
            .map(|d| d.join("reproit.yaml").to_string_lossy().into_owned())
            .unwrap_or_else(|_| "reproit.yaml".into())
    });
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut in_block = false;
    for raw in text.lines() {
        let line = raw.trim_end();
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        if !line.starts_with(' ') && !line.starts_with('\t') {
            in_block = line.trim().trim_end_matches(':') == "value_nodes" && line.ends_with(':');
            continue;
        }
        if in_block {
            let item = line.trim();
            if let Some(sel) = item.strip_prefix('-') {
                let sel = sel.trim().trim_matches('"').trim_matches('\'');
                if !sel.is_empty() {
                    out.push(sel.to_string());
                }
            }
        }
    }
    out
}

// Keep the shared imports honest on all builds.
#[allow(dead_code)]
fn _unused_reexports() {
    let _ = value_class("0");
    let _ = signature(None, &Node::new("screen"));
    let _ = structural_only(&Node::new("screen"));
}

// These tests pin the pure text scan; the libatspi-facing walk is exercised
// live by the operability-golden GTK/Qt CI jobs.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tofu_detail_flags_a_rendered_replacement_char_with_context() {
        // A rendered U+FFFD is broken text encoding: flagged, with a clipped
        // excerpt around the char as the human detail.
        assert_eq!(
            tofu_detail("glitch \u{FFFD} here").as_deref(),
            Some("glitch \u{FFFD} here")
        );
        // Long text clips to a bounded excerpt that still shows the char.
        let long = format!("{}{}{}", "a".repeat(60), '\u{FFFD}', "b".repeat(60));
        let ex = tofu_detail(&long).expect("long tofu text must flag");
        assert!(ex.chars().count() <= 41 && ex.contains('\u{FFFD}'));
    }

    #[test]
    fn tofu_detail_stays_silent_on_clean_text() {
        // No U+FFFD, no finding: plain, empty, and non-ASCII labels are clean.
        assert_eq!(tofu_detail(""), None);
        assert_eq!(tofu_detail("Save changes"), None);
        assert_eq!(tofu_detail("caf\u{e9} \u{4f60}\u{597d} \u{1f600}"), None);
    }

    #[test]
    fn target_lost_flags_a_dead_pid_only() {
        // AT-SPI reports pid 0 for an application whose process has exited: the
        // single liveness invariant shared by the scenario and single-seed walks.
        // (The FFI acc_pid read and the full AT-SPI walk are exercised live by the
        // Linux GTK/Qt CI jobs; only this pure decision is unit-tested here.)
        assert!(target_lost(0));
        assert!(!target_lost(1));
        assert!(!target_lost(4242));
    }

    #[test]
    fn content_bug_flags_leak_artifacts_but_not_prose() {
        // The classic artifacts ARE the label (bare, or a short field prefix): flag.
        assert_eq!(content_bug_reason("null"), Some("null"));
        assert_eq!(content_bug_reason("Price: null"), Some("null"));
        assert_eq!(content_bug_reason("undefined"), Some("undefined"));
        assert_eq!(content_bug_reason("Qty: undefined"), Some("undefined"));
        assert_eq!(content_bug_reason("NaN"), Some("nan"));
        assert_eq!(content_bug_reason("Total: NaN"), Some("nan"));
        // Prose that merely mentions the word inside a sentence is not a leak: a
        // dialog body that happens to contain the word.
        assert_eq!(
            content_bug_reason("repro demo crash: null inventory record."),
            None
        );
        assert_eq!(
            content_bug_reason("The undefined behavior here is intentional and documented."),
            None
        );
        assert_eq!(
            content_bug_reason("Parsing produced NaN because the field was blank, so we retried."),
            None
        );
        // Templates are always artifacts, guard or not; whole-word only, so a word
        // that merely contains the token ("annulled") is clean.
        assert_eq!(
            content_bug_reason("Hello {{name}}"),
            Some("unrendered-template")
        );
        assert_eq!(content_bug_reason("annulled"), None);
    }

    #[test]
    fn parse_invariant_marker_reads_violations_and_ignores_noise() {
        let (sig, items) = parse_invariant_marker(
            r#"REPROIT_INVARIANT {"sig":"s1","items":[{"id":"balance","message":"< 0"}]}"#,
        )
        .expect("a marker parses");
        assert_eq!(sig, "s1");
        assert_eq!(items, vec![("balance".into(), "< 0".into())]);
        // A plain log line, malformed json, and an empty item list are silent
        // (a clean settle emits no marker, so None is the clean direction).
        assert!(parse_invariant_marker("[reproit] some batch json").is_none());
        assert!(parse_invariant_marker("REPROIT_INVARIANT {oops").is_none());
        assert!(parse_invariant_marker(r#"REPROIT_INVARIANT {"items":[]}"#).is_none());
    }

    #[test]
    fn invariant_scrape_dedups_per_state_and_matches_sig() {
        // Build the tracker directly with a pre-populated shared state (bypassing
        // the reader thread) so the assertion is deterministic.
        let mut state = InvariantState::default();
        state
            .by_sig
            .insert("s1".into(), vec![("inv".into(), "boom".into())]);
        state.fallback = Some(vec![("g".into(), String::new())]);
        let mut scr = InvariantScrape {
            state: Arc::new(Mutex::new(state)),
            emitted: BTreeSet::new(),
        };
        // Violating state s1 fires once; a re-visit is de-duped; a clean state
        // consumes the empty-sig fallback (attributed to the current sig).
        assert_eq!(
            scr.pending_for("s1"),
            Some(vec![("inv".into(), "boom".into())])
        );
        assert_eq!(scr.pending_for("s1"), None, "no repeat on revisit");
        assert_eq!(
            scr.pending_for("s2"),
            Some(vec![("g".into(), String::new())]),
            "empty-sig fallback lands on the current runner sig"
        );
        assert_eq!(scr.pending_for("s3"), None, "fallback is consumed once");
    }
}
