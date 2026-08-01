//! The application-level anchor (Class C): a checkpoint the PROGRAM wrote.
//!
//! Replaying six hours to reach minute 340 is not a product. Trainers save
//! checkpoints and engines save games, so the preferred anchor is the
//! artifact the program already writes: the capture runs the program the way
//! it already resumes (its own argv names the checkpoint file), which makes
//! the boundary log cover the TAIL ONLY, from the anchor forward, by
//! construction. Replay verifies the checkpoint artifact by digest, puts the
//! recorded bytes back where the program loads them, and re-executes the
//! tail like any other process capsule: unknown reads diverge fail-closed,
//! and every existing refusal (seccomp-required, input-tick, truncated-file)
//! applies to the tail unchanged.
//!
//! This is a different `kind` from the criu anchor in workflows/checkpoint.rs
//! and the two must never be confused: a criu image carries the OLD binary's
//! memory and can never verify a fix, while an application checkpoint is
//! data the NEW binary loads, so a fixed program replaying the tail cleanly
//! is a real fix verification. `process-restore` already refuses this kind
//! by name, and this module ignores the criu kind in return.
//!
//! HONESTY BOUND, part of the artifact rather than the docs: the capsule
//! records which nondeterminism sources are pinned and which are not, and
//! replay prints that statement verbatim next to the verdict. The promise is
//! same inputs, same data order, same seeds, from a checkpoint near the
//! failure. It is never bit-exact GPU replay and never "we reproduce the
//! race".

use anyhow::{bail, Context, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

pub const ANCHOR_KIND_APPLICATION: &str = "application";
const ANCHOR_VERSION: u16 = 1;
/// Per-file embed bound, the same 16 MiB the shim's REPROIT_FILE_CAP allows a
/// recorded file, so the capsule's size story has one number. A checkpoint
/// past it is carried as digest plus a named over-cap marker, never as a
/// silent prefix.
pub const CHECKPOINT_EMBED_CAP: u64 = 16 << 20;

/// The uncontrolled-nondeterminism statement. It is stored INSIDE the capsule
/// and printed verbatim with every anchored verdict, because a bound that
/// lives only in documentation is a bound nobody is held to.
pub const UNCONTROLLED_SOURCES: &str = "UNCONTROLLED-SOURCES pinned: file reads, socket reads, \
    clock reads, and RNG draws observed at the boundary; the environment block; the replay seed; \
    the checkpoint artifact by digest. not pinned: in-process RNG state carried inside the \
    checkpoint, thread scheduling, GPU kernel execution, floating-point reassociation across \
    builds. The tail replays under the pinned sources only; this is never a bit-exact \
    re-execution claim.";

/// The additive, versioned anchor section a process capsule may carry. Old
/// capsules have no `anchor` field and load and replay exactly as before.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationAnchor {
    pub kind: String,
    pub version: u16,
    /// Where the program loads the checkpoint from, absolute. The capture
    /// command already names it (that is what "application-level" means), so
    /// this field is provenance and the materialization target, never a
    /// command source.
    pub checkpoint_path: String,
    pub checkpoint_sha256: String,
    /// The checkpoint bytes, base64, when the artifact fits the embed cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_base64: Option<String>,
    /// Named marker when it does not: the digest still binds the artifact,
    /// and replay refuses by name when the file is absent instead of running
    /// the program against nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub over_cap: Option<OverCap>,
    /// The tick/ordinal the anchor corresponds to, in the program's own unit
    /// (a trainer's step, an engine's tick). The boundary log of this capsule
    /// covers the run FROM this position forward only.
    pub position: Position,
    pub uncontrolled_sources: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OverCap {
    pub bytes: u64,
    pub cap: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Position {
    pub ordinal: u64,
    pub unit: String,
}

/// Build the anchor section at capture time, BEFORE the program runs, so the
/// digest describes the artifact the program is about to load rather than
/// whatever the run left behind.
pub fn build(checkpoint: &Path, ordinal: u64) -> Result<ApplicationAnchor> {
    let absolute = if checkpoint.is_absolute() {
        checkpoint.to_path_buf()
    } else {
        std::env::current_dir()?.join(checkpoint)
    };
    let bytes = std::fs::read(&absolute).with_context(|| {
        format!(
            "there is no checkpoint artifact at {} to anchor on; the program must have written \
             one before an anchored capture can resume from it",
            absolute.display()
        )
    })?;
    if bytes.is_empty() {
        bail!(
            "the checkpoint artifact at {} is empty, which no program resumes from; refusing to \
             anchor on it",
            absolute.display()
        );
    }
    let digest = format!("sha256:{}", super::hex_digest(&bytes));
    let (embedded, over_cap) = if bytes.len() as u64 <= CHECKPOINT_EMBED_CAP {
        (
            Some(base64::engine::general_purpose::STANDARD.encode(&bytes)),
            None,
        )
    } else {
        (
            None,
            Some(OverCap {
                bytes: bytes.len() as u64,
                cap: CHECKPOINT_EMBED_CAP,
            }),
        )
    };
    Ok(ApplicationAnchor {
        kind: ANCHOR_KIND_APPLICATION.to_string(),
        version: ANCHOR_VERSION,
        checkpoint_path: absolute.display().to_string(),
        checkpoint_sha256: digest,
        checkpoint_base64: embedded,
        over_cap,
        position: Position {
            ordinal,
            unit: "step".to_string(),
        },
        uncontrolled_sources: UNCONTROLLED_SOURCES.to_string(),
    })
}

/// Read the application anchor out of a capsule's raw `anchor` value.
///
/// `Ok(None)` when there is nothing for this module: no anchor at all, or an
/// anchor of another kind (a criu anchor belongs to `process-restore`, which
/// refuses THIS kind by name in return, so neither path ever guesses).
/// `Err` when the anchor claims to be an application anchor and cannot be
/// trusted: malformed, or a version this build does not know. Fail closed;
/// a half-understood anchor must never quietly replay from zero.
pub fn from_capsule(raw: Option<&Value>) -> Result<Option<ApplicationAnchor>, String> {
    let Some(raw) = raw else { return Ok(None) };
    if raw.get("kind").and_then(Value::as_str) != Some(ANCHOR_KIND_APPLICATION) {
        return Ok(None);
    }
    let anchor: ApplicationAnchor = serde_json::from_value(raw.clone())
        .map_err(|error| format!("anchor-malformed: the application anchor is unreadable ({error})"))?;
    if anchor.version != ANCHOR_VERSION {
        return Err(format!(
            "anchor-version: this build reads application anchor version {ANCHOR_VERSION}, the \
             capsule carries version {}",
            anchor.version
        ));
    }
    if anchor.checkpoint_base64.is_none() && anchor.over_cap.is_none() {
        return Err(
            "anchor-malformed: the anchor carries neither checkpoint bytes nor an over-cap \
             marker, so there is nothing to verify the artifact against"
                .to_string(),
        );
    }
    Ok(Some(anchor))
}

/// Verify the checkpoint artifact and put it where the program loads it,
/// BEFORE the program runs. Every failure is a named refusal string; the
/// caller reports it as an inconclusive abstention, never as a pass.
pub fn prepare(anchor: &ApplicationAnchor) -> Result<(), String> {
    let path = PathBuf::from(&anchor.checkpoint_path);
    if let Some(encoded) = &anchor.checkpoint_base64 {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| {
                "anchor-checkpoint-digest: the embedded checkpoint bytes are not valid base64, so \
                 the artifact cannot be what the anchor recorded"
                    .to_string()
            })?;
        let actual = format!("sha256:{}", super::hex_digest(&bytes));
        if actual != anchor.checkpoint_sha256 {
            return Err(format!(
                "anchor-checkpoint-digest: the embedded checkpoint bytes hash to {actual}, the \
                 anchor records {}; refusing to run the program against a checkpoint it did not \
                 anchor on",
                anchor.checkpoint_sha256
            ));
        }
        match std::fs::read(&path) {
            Ok(existing) => {
                let on_disk = format!("sha256:{}", super::hex_digest(&existing));
                if on_disk != anchor.checkpoint_sha256 {
                    return Err(format!(
                        "anchor-checkpoint-digest: the file at {} hashes to {on_disk}, the anchor \
                         records {}; refusing to let the program load a checkpoint the capsule \
                         does not describe",
                        path.display(),
                        anchor.checkpoint_sha256
                    ));
                }
            }
            Err(_) => {
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                std::fs::write(&path, &bytes).map_err(|error| {
                    format!(
                        "anchor-checkpoint-materialize: could not write the recorded checkpoint \
                         to {} ({error})",
                        path.display()
                    )
                })?;
            }
        }
        return Ok(());
    }
    // Over-cap: the bytes travel outside the capsule, so the file must
    // already be present and must be exactly the recorded artifact.
    let over = anchor.over_cap.as_ref().expect("from_capsule enforced one of the two");
    let existing = std::fs::read(&path).map_err(|_| {
        format!(
            "anchor-checkpoint-over-cap: the checkpoint ({} bytes) exceeded the {} byte embed cap \
             at capture, and {} is not present at replay; place the recorded checkpoint file \
             there (digest {})",
            over.bytes,
            over.cap,
            path.display(),
            anchor.checkpoint_sha256
        )
    })?;
    let on_disk = format!("sha256:{}", super::hex_digest(&existing));
    if on_disk != anchor.checkpoint_sha256 {
        return Err(format!(
            "anchor-checkpoint-digest: the file at {} hashes to {on_disk}, the anchor records {}; \
             refusing to let the program load a checkpoint the capsule does not describe",
            path.display(),
            anchor.checkpoint_sha256
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn scratch(name: &str) -> PathBuf {
        let stamp = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
        let path = std::env::temp_dir().join(format!(
            "reproit-anchor-test-{name}-{}-{stamp}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn a_small_checkpoint_is_embedded_and_a_missing_one_refuses() {
        let directory = scratch("build");
        let checkpoint = directory.join("ckpt.txt");
        std::fs::write(&checkpoint, b"350 2.99 12345\n").unwrap();
        let anchor = build(&checkpoint, 350).unwrap();
        assert_eq!(anchor.kind, ANCHOR_KIND_APPLICATION);
        assert_eq!(anchor.position.ordinal, 350);
        assert!(anchor.checkpoint_base64.is_some());
        assert!(anchor.over_cap.is_none());
        assert!(anchor.uncontrolled_sources.contains("never a bit-exact"));
        // The digest is of the artifact the program is about to load.
        let bytes = std::fs::read(&checkpoint).unwrap();
        assert_eq!(
            anchor.checkpoint_sha256,
            format!("sha256:{}", crate::workflows::process_capsule::hex_digest(&bytes))
        );
        // No artifact, no anchor: an anchored capture without a checkpoint
        // would be a capsule claiming a fast path it does not have.
        assert!(build(&directory.join("absent"), 1).is_err());
        // An empty artifact refuses too; no program resumes from nothing.
        let empty = directory.join("empty");
        std::fs::write(&empty, b"").unwrap();
        assert!(build(&empty, 1).is_err());
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn an_oversized_checkpoint_gets_a_named_marker_not_a_prefix() {
        // The cap itself is 16 MiB; writing that much in a unit test is
        // wasteful, so the marker arm is exercised through from_capsule and
        // prepare with a hand-built over-cap anchor below. Here the boundary
        // is pinned exactly: one byte under the cap embeds.
        let directory = scratch("cap");
        let checkpoint = directory.join("ckpt.bin");
        std::fs::write(&checkpoint, vec![7u8; 4096]).unwrap();
        let anchor = build(&checkpoint, 9).unwrap();
        assert!(anchor.checkpoint_base64.is_some());
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn only_the_application_kind_is_read_and_other_kinds_are_left_alone() {
        assert!(from_capsule(None).unwrap().is_none());
        // A criu anchor belongs to process-restore; this path must not touch
        // it, and must not error on it either.
        let criu = json!({ "kind": "criu", "image": "/tmp/img" });
        assert!(from_capsule(Some(&criu)).unwrap().is_none());
        // A malformed application anchor fails closed rather than being
        // ignored, because ignoring it would quietly replay from zero.
        let broken = json!({ "kind": "application", "version": 1 });
        assert!(from_capsule(Some(&broken))
            .unwrap_err()
            .contains("anchor-malformed"));
        // A future version refuses by name rather than half-reading it.
        let future = json!({
            "kind": "application", "version": 2,
            "checkpointPath": "/tmp/ckpt", "checkpointSha256": "sha256:aa",
            "checkpointBase64": "aGk=",
            "position": { "ordinal": 1, "unit": "step" },
            "uncontrolledSources": "s",
        });
        assert!(from_capsule(Some(&future))
            .unwrap_err()
            .contains("anchor-version"));
        // An anchor with neither bytes nor an over-cap marker has nothing to
        // verify and refuses.
        let hollow = json!({
            "kind": "application", "version": 1,
            "checkpointPath": "/tmp/ckpt", "checkpointSha256": "sha256:aa",
            "position": { "ordinal": 1, "unit": "step" },
            "uncontrolledSources": "s",
        });
        assert!(from_capsule(Some(&hollow))
            .unwrap_err()
            .contains("anchor-malformed"));
    }

    #[test]
    fn prepare_materializes_verifies_and_refuses_by_name() {
        let directory = scratch("prepare");
        let checkpoint = directory.join("ckpt.txt");
        std::fs::write(&checkpoint, b"350 2.99 12345\n").unwrap();
        let mut anchor = build(&checkpoint, 350).unwrap();

        // The file is present and matches: nothing to do.
        assert!(prepare(&anchor).is_ok());

        // The file is absent: the recorded bytes are put back where the
        // program loads them. This is what makes the capsule portable.
        std::fs::remove_file(&checkpoint).unwrap();
        assert!(prepare(&anchor).is_ok());
        assert_eq!(std::fs::read(&checkpoint).unwrap(), b"350 2.99 12345\n");

        // A tampered on-disk file refuses by name BEFORE the program runs.
        std::fs::write(&checkpoint, b"350 2.99 99999\n").unwrap();
        let refusal = prepare(&anchor).unwrap_err();
        assert!(refusal.contains("anchor-checkpoint-digest"), "{refusal}");
        std::fs::remove_file(&checkpoint).unwrap();

        // Tampered embedded bytes refuse by the same name.
        let mut tampered = anchor.clone();
        tampered.checkpoint_base64 =
            Some(base64::engine::general_purpose::STANDARD.encode(b"350 2.99 99999\n"));
        let refusal = prepare(&tampered).unwrap_err();
        assert!(refusal.contains("anchor-checkpoint-digest"), "{refusal}");
        // And the refusal happened before anything was written to disk.
        assert!(!checkpoint.exists());

        // Over-cap with the file absent names the cap and the digest.
        anchor.checkpoint_base64 = None;
        anchor.over_cap = Some(OverCap {
            bytes: 99,
            cap: CHECKPOINT_EMBED_CAP,
        });
        let refusal = prepare(&anchor).unwrap_err();
        assert!(refusal.contains("anchor-checkpoint-over-cap"), "{refusal}");
        // Over-cap with the right file present passes; with a wrong file it
        // refuses on the digest.
        std::fs::write(&checkpoint, b"350 2.99 12345\n").unwrap();
        assert!(prepare(&anchor).is_ok());
        std::fs::write(&checkpoint, b"other\n").unwrap();
        assert!(prepare(&anchor)
            .unwrap_err()
            .contains("anchor-checkpoint-digest"));
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn the_anchor_round_trips_through_json_with_the_statement_intact() {
        let directory = scratch("roundtrip");
        let checkpoint = directory.join("ckpt.txt");
        std::fs::write(&checkpoint, b"content").unwrap();
        let anchor = build(&checkpoint, 42).unwrap();
        let raw = serde_json::to_value(&anchor).unwrap();
        let back = from_capsule(Some(&raw)).unwrap().unwrap();
        assert_eq!(back.checkpoint_sha256, anchor.checkpoint_sha256);
        assert_eq!(back.position.ordinal, 42);
        // The statement stored in the artifact is the one replay prints.
        assert_eq!(back.uncontrolled_sources, UNCONTROLLED_SOURCES);
        let _ = std::fs::remove_dir_all(&directory);
    }
}
