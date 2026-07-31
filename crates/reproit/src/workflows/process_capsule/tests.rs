//! Unit tests for the process capsule: outcome equality, ELF linkage
//! judgement, and format routing.

use super::*;

#[test]
fn outcome_failure_and_equality_follow_how_the_program_died() {
    let clean = Outcome {
        exit_code: Some(0),
        signal: None,
    };
    let aborted = Outcome {
        exit_code: None,
        signal: Some(6),
    };
    let nonzero = Outcome {
        exit_code: Some(4),
        signal: None,
    };
    assert!(!clean.failed());
    assert!(aborted.failed());
    assert!(nonzero.failed());
    assert!(aborted.same_as(&aborted));
    assert!(!aborted.same_as(&nonzero));
    assert_eq!(aborted.describe(), "fatal signal 6");
    assert_eq!(nonzero.describe(), "exit 4");
    // A shell reports the same abort as 128 + SIGABRT; the two spellings
    // of one death must compare equal, and must not swallow a genuinely
    // different exit code.
    let through_shell = Outcome {
        exit_code: Some(134),
        signal: None,
    };
    assert!(through_shell.same_as(&aborted));
    assert!(aborted.same_as(&through_shell));
    assert_eq!(through_shell.describe(), "fatal signal 6");
    assert!(!through_shell.same_as(&nonzero));
    assert!(!clean.same_as(&aborted));
}

/// A minimal 64 bit little endian ELF carrying one program header of the
/// given type. Synthetic on purpose: the host running these tests may not
/// be an ELF platform at all, and the property under test is how the
/// parser reads program headers, not what this machine links.
fn synthetic_elf(program_header_type: u32) -> Vec<u8> {
    const HEADER: usize = 64;
    const ENTRY: usize = 56;
    let mut bytes = vec![0u8; HEADER + ENTRY];
    bytes[0..4].copy_from_slice(b"\x7fELF");
    bytes[4] = 2; // 64 bit
    bytes[5] = 1; // little endian
    bytes[0x20..0x28].copy_from_slice(&(HEADER as u64).to_le_bytes()); // e_phoff
    bytes[0x36..0x38].copy_from_slice(&(ENTRY as u16).to_le_bytes()); // e_phentsize
    bytes[0x38..0x3a].copy_from_slice(&1u16.to_le_bytes()); // e_phnum
    bytes[HEADER..HEADER + 4].copy_from_slice(&program_header_type.to_le_bytes());
    bytes
}

#[test]
fn static_linkage_is_judged_from_the_program_headers() {
    let directory = std::env::temp_dir().join(format!("reproit-elf-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    // PT_INTERP present: the loader resolves symbols, so the shim is
    // reachable and capture may proceed.
    let dynamic = directory.join("dynamic.elf");
    std::fs::write(&dynamic, synthetic_elf(3)).unwrap();
    assert_eq!(elf_is_dynamic(&dynamic), Some(true));
    // PT_LOAD only: nothing is interposed, so capture must refuse rather
    // than write a capsule of nothing.
    let statik = directory.join("static.elf");
    std::fs::write(&statik, synthetic_elf(1)).unwrap();
    assert_eq!(elf_is_dynamic(&statik), Some(false));
    // Not an ELF: say nothing rather than guess, so a script or a wrapper
    // is never refused as "static".
    let script = directory.join("script.sh");
    std::fs::write(&script, b"#!/bin/sh\necho hi\n").unwrap();
    assert_eq!(elf_is_dynamic(&script), None);
    // A truncated header is unjudgeable too.
    let stub = directory.join("stub.elf");
    std::fs::write(&stub, b"\x7fELF\x02\x01").unwrap();
    assert_eq!(elf_is_dynamic(&stub), None);
    let _ = std::fs::remove_dir_all(&directory);
}

#[test]
fn a_non_capsule_file_does_not_route_to_the_process_path() {
    let directory = std::env::temp_dir().join(format!("reproit-capsule-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let backend = directory.join("backend.json");
    std::fs::write(&backend, br#"{"format":"reproit-backend-capture"}"#).unwrap();
    assert!(!is_process_capsule(&backend));
    let capsule = directory.join("process.json");
    std::fs::write(&capsule, br#"{"format":"reproit-process-capsule"}"#).unwrap();
    assert!(is_process_capsule(&capsule));
    assert!(!is_process_capsule(&directory.join("absent.json")));
    let _ = std::fs::remove_dir_all(&directory);
}

#[test]
fn two_different_assertions_are_not_the_same_failure() {
    // Both of these die with SIGABRT, so the exit status alone cannot
    // tell them apart. This is the false proof the failure identity
    // exists to prevent.
    let recorded = failure_signature(&[
        "engine: engine.c:52: main: Assertion `thrust <= MAX_THRUST' failed.".to_string(),
    ]);
    let other =
        failure_signature(
            &["engine: engine.c:81: main: Assertion `fuel >= 0' failed.".to_string()],
        );
    assert!(recorded.is_some());
    assert_ne!(recorded, other);
}

#[test]
fn only_addresses_fold_because_everything_else_is_stable_across_a_replay() {
    // ASLR moves addresses between two runs of the same defect, so they
    // fold. Nothing else does: a replay runs the same binary, so the file,
    // the line, and the predicate are all stable.
    let first = failure_signature(&[
        "app: src/main.c:52: run: Assertion `n < 8' failed. at 0x7ffd12ab".to_string(),
    ]);
    let second = failure_signature(&[
        "app: src/main.c:52: run: Assertion `n < 8' failed. at 0x55aa9001".to_string(),
    ]);
    assert_eq!(first, second);
    // A different asserted value is a DIFFERENT failure. Folding decimal
    // digits would have made these equal, which is the false proof this
    // guards against.
    let third = failure_signature(&[
        "app: src/main.c:52: run: Assertion `n < 9' failed. at 0x55aa9001".to_string(),
    ]);
    assert_ne!(first, third);
}

/// One determinism envelope contract across every capture kind. The
/// backend SDKs emit these keys as their `determinism-envelope`
/// checkpoint, and a process capsule must carry the same ones so a single
/// reader can pin a replay's clock, timezone and seed without asking
/// which capture produced it.
#[test]
fn the_envelope_matches_the_shape_every_capture_kind_emits() {
    let envelope = determinism_envelope("c0ffee00c0ffee00");
    for key in ["observedAtMs", "tz", "os", "arch", "replaySeed"] {
        assert!(
            envelope.get(key).is_some(),
            "the shared envelope must carry {key}"
        );
    }
    assert_eq!(
        envelope.get("replaySeed").and_then(Value::as_str),
        Some("c0ffee00c0ffee00")
    );
}

/// A field the capture cannot know is ABSENT, never guessed. The SDKs
/// carry imageDigest only when the environment states one, and a process
/// capsule follows the same rule, so a reader can trust that a present
/// field was observed.
#[test]
fn an_unknowable_envelope_field_is_absent_rather_than_invented() {
    // The test process may or may not have the variable set, so assert
    // the RULE rather than one environment's answer.
    let envelope = determinism_envelope("seed");
    match std::env::var("REPROIT_IMAGE_DIGEST") {
        Ok(digest) if !digest.is_empty() => {
            assert_eq!(
                envelope.get("imageDigest").and_then(Value::as_str),
                Some(digest.as_str())
            );
        }
        _ => assert!(
            envelope.get("imageDigest").is_none(),
            "an unstated image digest must not appear at all"
        ),
    }
}

#[test]
fn a_program_that_dies_without_declaring_why_has_no_signature() {
    // A silent SIGSEGV leaves the signal as the whole story, so the
    // capsule must not invent an identity it never observed.
    assert_eq!(failure_signature(&["Segmentation fault".to_string()]), None);
    assert_eq!(failure_signature(&[]), None);
}

#[test]
fn rust_and_go_failure_text_is_recognized_too() {
    assert!(
        failure_signature(&["thread 'main' panicked at src/lib.rs:9:5:".to_string()]).is_some()
    );
    assert!(failure_signature(&["panic: runtime error: index out of range".to_string()]).is_some());
}

#[test]
fn a_static_exec_in_the_recording_is_found_and_named() {
    // The shim records one `exec` entry per new program image, and only for
    // the statically linked ones, so the line's presence is the whole signal
    // and its key is the image capture must name in its refusal.
    let clean = vec![
        "open\t/etc/hosts\t-\t3\t12\t0".to_string(),
        "read\t/etc/hosts\tYQ==\t3\t0\t0".to_string(),
    ];
    assert_eq!(static_exec(&clean), None);
    let mut wrapped = clean.clone();
    wrapped.push("exec\t/usr/bin/busybox\t-\t0\t0\t0".to_string());
    assert_eq!(static_exec(&wrapped), Some("/usr/bin/busybox"));
    // A malformed line names nothing rather than refusing on an empty image.
    assert_eq!(static_exec(&["exec\t-\t-\t0\t0\t0".to_string()]), None);
    assert_eq!(static_exec(&["exec\t".to_string()]), None);
}
