//! Lifecycle-metamorphic invariants: rotation, background, scroll round-trips.

use crate::adapters::config::InvariantsCfg;
use crate::domain::invariants::finding::finding;
use crate::domain::invariants::Observations;
use serde_json::Value;

pub(super) fn evaluate_lifecycle_invariants(obs: &Observations, cfg: &InvariantsCfg) -> Vec<Value> {
    let mut out = Vec::new();

    // no-rotation-loss: a lifecycle-metamorphic violation across a device
    // rotation. The explorer rotated the surface (portrait <-> landscape /
    // split-screen), reflowed, then rotated BACK to the original orientation,
    // and the screen did not rebuild the same structure -- content or state was
    // lost across the rotation lifecycle and never came back. From
    // EXPLORE:ROTATION; round-trip identity (same orientation in and out) with
    // value-state excluded, so it is deterministic and false-positive-free.
    // Essentials before any parenthesis: scan detail truncates at the first " (".
    if cfg.no_rotation_loss {
        for (sig, (expected, got)) in &obs.obs.rotation_losses {
            out.push(finding(
                "no-rotation-loss",
                "ROTATION",
                format!(
                    "state {sig} does not survive rotation: after rotating the screen and \
                     rotating back, the app rebuilt a different structure than before, so content \
                     or state was lost across the orientation change (expected structure \
                     {expected}, got {got})"
                ),
                Some(sig),
            ));
        }
    }

    // no-background-loss: a lifecycle-metamorphic violation across the app
    // background -> foreground cycle. The explorer sent the app to the
    // background (paused/hidden) then restored it (resumed/visible), and it came
    // back to a DIFFERENT screen or lost its state. From EXPLORE:BGRESTORE; no
    // size change and value-state excluded, so it is deterministic and
    // false-positive-free. Essentials before any parenthesis (scan detail
    // truncates at the first " (").
    if cfg.no_background_loss {
        for (sig, (expected, got)) in &obs.obs.background_losses {
            out.push(finding(
                "no-background-loss",
                "BGRESTORE",
                format!(
                    "state {sig} does not survive backgrounding: sending the app to the \
                     background and restoring it dropped the user on a different screen or lost \
                     state, instead of returning to the same screen (expected structure \
                     {expected}, got {got})"
                ),
                Some(sig),
            ));
        }
    }

    // no-stuck-keyboard: the soft keyboard must never be visible without a
    // focused text input. From EXPLORE:STUCKKEYBOARD, emitted only on violation
    // by the native mobile explorers off platform ground truth (IME visibility
    // + focus tree), so it is deterministic and false-positive-free. Keep any
    // remedy out of parentheses: scan detail truncates at the first " (".
    if cfg.no_stuck_keyboard {
        for sig in &obs.obs.stuck_keyboards {
            out.push(finding(
                "no-stuck-keyboard",
                "STUCKKEYBOARD",
                format!(
                    "state {sig} keeps the soft keyboard open with no text field focused: the \
                     keyboard covers content the user never asked for and cannot dismiss by \
                     leaving the field"
                ),
                Some(sig),
            ));
        }
    }

    // no-wakelock-leak: a wakelock (or a window FLAG_KEEP_SCREEN_ON) acquired on a
    // screen must be released when the user leaves it. From EXPLORE:WAKELOCK,
    // emitted only on a violation by the Android/Appium explorer off platform
    // ground truth (dumpsys power app-owned wake locks + the focused window's
    // keep-screen-on flag, compared before vs after leaving the screen). The
    // runner excludes app-global/baseline and released locks and attributes each
    // leak to its origin screen once, so it is deterministic and
    // false-positive-free. Keep essentials before any parenthesis: scan detail
    // truncates at the first " (".
    if cfg.no_wakelock_leak {
        for (sig, items) in &obs.obs.wakelock_leaks {
            if items.is_empty() {
                continue;
            }
            let detail = items
                .iter()
                .take(3)
                .map(|(tag, kind)| format!("{tag} [{kind}]"))
                .collect::<Vec<_>>()
                .join(", ");
            out.push(finding(
                "no-wakelock-leak",
                "WAKELOCK",
                format!(
                    "state {sig} keeps a wakelock held after you navigate away: {detail} still \
                     held off the screen that needed it, draining the battery by keeping the \
                     CPU/screen awake"
                ),
                Some(sig),
            ));
        }
    }

    out
}
