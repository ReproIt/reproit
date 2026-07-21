use super::*;

#[test]
fn timing_line_lists_each_phase_then_total() {
    let phases = [
        ("sim", Duration::from_millis(1200)),
        ("reset", Duration::from_millis(300)),
        ("build", Duration::from_millis(10200)),
        ("launch", Duration::from_millis(4500)),
        ("walk", Duration::from_millis(150000)),
        ("teardown", Duration::from_millis(2000)),
    ];
    let total: Duration = phases.iter().map(|(_, d)| *d).sum();
    let line = PhaseTimer::format(&phases, total);
    assert_eq!(
        line,
        "timing: sim=1.2s reset=0.3s build=10.2s launch=4.5s walk=150.0s teardown=2.0s \
             total=168.2s"
    );
}

#[test]
fn disabled_timer_does_nothing() {
    let mut t = PhaseTimer::new(false);
    t.mark("sim");
    t.mark("walk");
    t.finish();
    assert!(t.phases.is_empty()); // no work accrued when disabled
}

#[test]
fn headless_targets_prefer_host_tests_and_keep_legacy_fallbacks() {
    assert_eq!(
        headless_target_candidates("integration_test", "explore"),
        [
            "test/fuzz_headless_explore.dart",
            "test/fuzz_headless_test.dart",
            "integration_test/fuzz_headless_explore.dart",
            "integration_test/fuzz_headless_test.dart",
        ]
    );
}

#[test]
fn discovers_runner_managed_video_recursively() {
    let test_name = std::thread::current()
        .name()
        .unwrap_or("test")
        .replace("::", "-");
    let root = std::env::temp_dir().join(format!(
        "reproit-runner-video-{}-{}",
        std::process::id(),
        test_name
    ));
    let nested = root.join("playwright");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(root.join("box-spec.json"), b"{}").unwrap();
    let video = nested.join("page-generated.webm");
    std::fs::write(&video, b"video").unwrap();

    assert_eq!(newest_runner_video(&root), Some(video));
    std::fs::remove_dir_all(root).unwrap();
}
