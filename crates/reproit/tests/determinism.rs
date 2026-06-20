//! End-to-end determinism: the same seed must drive the same run. We point the
//! TUI backend at `sleep`: a process that emits NOTHING, so the screen is
//! always blank and action selection is purely RNG-driven (no app-output timing
//! to introduce flake). Two same-seed runs must produce byte-identical action
//! streams; different seeds must diverge. This is the "author once, reproduce
//! forever" promise, tested through the real binary.

use std::process::Command;

fn run(seed: u32) -> String {
    let cfg = std::env::temp_dir().join(format!("reproit_det_{seed}.json"));
    std::fs::write(&cfg, format!("{{\"seed\":{seed},\"budget\":16}}")).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_reproit"))
        .arg("__tui")
        .env("REPROIT_TUI_CMD", "sleep 30")
        .env("REPROIT_FUZZ_CONFIG", &cfg)
        .output()
        .expect("run reproit __tui");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.starts_with("FUZZ:ACT") || l.starts_with("EXPLORE:STATE"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn same_seed_replays_identically() {
    let a = run(7);
    let b = run(7);
    assert!(!a.is_empty(), "the run should emit actions/states");
    assert_eq!(a, b, "same seed must produce an identical action stream");
}

#[test]
fn different_seeds_diverge() {
    // guards against a degenerate/constant fuzzer that ignores the seed.
    assert_ne!(run(1), run(2));
}
