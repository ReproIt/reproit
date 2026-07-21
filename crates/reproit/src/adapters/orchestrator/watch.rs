use super::*;

/// Per-phase wall-clock timer for `--profile-timing`. Each `mark(name)` closes
/// the previous phase and opens a new one; `finish()` closes the last and, if
/// enabled, prints a single `timing:` line. Off (no allocation churn, no
/// output) unless enabled.
pub(super) struct PhaseTimer {
    enabled: bool,
    started: Instant,
    last: Instant,
    pub(super) phases: Vec<(&'static str, Duration)>,
}

impl PhaseTimer {
    pub(super) fn new(enabled: bool) -> Self {
        let now = Instant::now();
        PhaseTimer {
            enabled,
            started: now,
            last: now,
            phases: Vec::new(),
        }
    }

    pub(super) fn mark(&mut self, next: &'static str) {
        if !self.enabled {
            return;
        }
        let now = Instant::now();
        if let Some((_, d)) = self.phases.last_mut() {
            *d = now - self.last;
        }
        self.phases.push((next, Duration::ZERO));
        self.last = now;
    }

    pub(super) fn finish(&mut self) {
        if !self.enabled {
            return;
        }
        let now = Instant::now();
        if let Some((_, d)) = self.phases.last_mut() {
            *d = now - self.last;
        }
        eprintln!("  {}", Self::format(&self.phases, now - self.started));
    }

    /// The single `timing:` line: each phase's seconds plus the total. Pure, so
    /// the format is unit-testable without going through the device run.
    pub(super) fn format(phases: &[(&'static str, Duration)], total: Duration) -> String {
        let parts: Vec<String> = phases
            .iter()
            .map(|(name, d)| format!("{name}={:.1}s", d.as_secs_f64()))
            .collect();
        format!(
            "timing: {} total={:.1}s",
            parts.join(" "),
            total.as_secs_f64()
        )
    }
}

/// Per-device liveness announcements: prints each device's "live" and
/// "done" transition exactly once across the whole run, so long waits are
/// never silent.
pub(super) struct DriveWatch {
    started: Instant,
    ready: Vec<bool>,
    done: Vec<bool>,
    exited: Vec<bool>,
}

impl DriveWatch {
    pub(super) fn new(n: usize) -> Self {
        DriveWatch {
            started: Instant::now(),
            ready: vec![false; n],
            done: vec![false; n],
            exited: vec![false; n],
        }
    }

    fn tick(&mut self, drives: &[Drive]) {
        let t = self.started.elapsed().as_secs();
        for (i, d) in drives.iter().enumerate() {
            if !self.ready[i] && d.is_ready() {
                self.ready[i] = true;
                eprintln!("  live  device {} (t+{t}s)", d.label);
            }
            if !self.done[i] && d.is_done() {
                self.done[i] = true;
                let verdict = match d.passed() {
                    Some(true) => "PASS",
                    Some(false) => "FAIL",
                    None => "?",
                };
                eprintln!("  done  device {}: {verdict} (t+{t}s)", d.label);
            }
            // A drive whose process exited WITHOUT reporting done crashed (or quit
            // early): announce it so the wait ending here reads as a crash, not a
            // silent stall. The verdict pass judges it by the captured evidence.
            if !self.exited[i] && !d.is_done() && d.has_exited() {
                self.exited[i] = true;
                eprintln!(
                    "  exit  device {}: process exited, no verdict (t+{t}s)",
                    d.label
                );
            }
        }
    }
}

/// Wait for a runner state notification, with a periodic progress tick and a
/// hard deadline. Readiness and verdicts no longer pay a polling delay.
pub(super) async fn wait_watching<F: Fn(&[Drive]) -> bool>(
    watch: &mut DriveWatch,
    drives: &[Drive],
    cond: F,
    deadline: Instant,
) -> bool {
    loop {
        let notifications: Vec<_> = drives
            .iter()
            .map(|drive| Box::pin(drive.changed().notified()))
            .collect();
        watch.tick(drives);
        if cond(drives) {
            return true;
        }
        if Instant::now() >= deadline {
            watch.tick(drives);
            return cond(drives);
        }
        let until_deadline = deadline.saturating_duration_since(Instant::now());
        tokio::select! {
            _ = futures_util::future::select_all(notifications) => {}
            _ = tokio::time::sleep(Duration::from_secs(2)) => {}
            _ = tokio::time::sleep(until_deadline) => {}
        }
    }
}

/// Summarize memory-<label>.jsonl into first/last/peak heap (bytes) and
/// print the trend line. None when no samples were collected.
pub(super) fn memory_summary(run_dir: &Path, label: &str) -> Option<serde_json::Value> {
    let raw = std::fs::read_to_string(run_dir.join(format!("memory-{label}.jsonl"))).ok()?;
    let samples: Vec<u64> = raw
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter_map(|v| v.get("heap_used").and_then(serde_json::Value::as_u64))
        .collect();
    let (first, last) = (*samples.first()?, *samples.last()?);
    let peak = *samples.iter().max()?;
    let mb = |b: u64| b as f64 / 1_048_576.0;
    eprintln!(
        "  memory device {label}: heap {:.1}MB -> {:.1}MB (peak {:.1}MB, {} samples)",
        mb(first),
        mb(last),
        mb(peak),
        samples.len()
    );
    Some(serde_json::json!({
        "samples": samples.len(),
        "heap_first": first,
        "heap_last": last,
        "heap_peak": peak,
    }))
}

/// Find the finalized clip emitted by a runner-managed recording backend.
/// Playwright uses generated filenames in a per-device directory, while native
/// runners use stable names such as `clip.mov`, so discovery is intentionally
/// extension-based and recursive.
pub(super) fn newest_runner_video(dir: &Path) -> Option<PathBuf> {
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    let mut pending = vec![dir.to_path_buf()];
    while let Some(current) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            let is_video = path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| {
                    matches!(ext.to_ascii_lowercase().as_str(), "webm" | "mov" | "mp4")
                });
            if !is_video {
                continue;
            }
            let modified = entry
                .metadata()
                .and_then(|meta| meta.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            if newest.as_ref().is_none_or(|(best, _)| modified > *best) {
                newest = Some((modified, path));
            }
        }
    }
    newest.map(|(_, path)| path)
}
