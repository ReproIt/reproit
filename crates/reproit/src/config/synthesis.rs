//! Zero-config web and TUI configuration synthesis.

use super::{parse_str, Loaded};
use anyhow::Result;
use std::path::{Path, PathBuf};

/// Build a web `Loaded` for the zero-config `reproit fuzz <url>` run, with
/// `.reproit/` output under `root` (the cwd). The synthesized config is also
/// persisted to `<root>/.reproit/reproit.yaml` so a follow-up `reproit <id>` /
/// `keep` / `repros` can replay the run without a hand-written reproit.yaml.
pub fn synthesize_web(url: &str, web_runner_dir: &Path, root: PathBuf) -> Result<Loaded> {
    // Serialize the URL and path as JSON strings: YAML is a JSON superset, so a
    // JSON-quoted scalar is a valid, fully-escaped YAML scalar. Raw interpolation
    // would let a `"`, backslash, or newline in the URL/path break the document.
    let url = serde_json::to_string(url).unwrap_or_else(|_| "\"\"".to_string());
    let wrd = serde_json::to_string(&web_runner_dir.display().to_string())
        .unwrap_or_else(|_| "\"\"".to_string());
    let yaml = format!(
        "app:\n  platform: web\n  webRunnerDir: {wrd}\n  url: {url}\n  defines: {{}}\ndevices:\n  \
         namePrefix: web\nreset:\n  steps: []\njourneys:\n  dir: integration_test\n  driver: \
         web\n  readyMarker: \"claimed role\"\n  doneMarkers:\n    - All tests passed\n    - Some \
         tests failed\n  deviceDoneMarker: \"JOURNEY DONE\"\n  actionPrefix: \"JOURNEY\"\n  \
         timeoutSec: 300\nevidence:\n  outDir: .reproit/runs\n  video: false\n",
    );
    let loaded = parse_str(&yaml, root)?;
    // Persist so the zero-config flow is replayable: `find_config` picks this up
    // as a fallback and `load` roots it back at the cwd (the `.reproit` parent).
    // Best-effort: a write failure leaves the run working but non-replayable,
    // exactly as before this was persisted.
    let path = crate::layout::config_path(&loaded.root);
    if let Some(dir) = path.parent() {
        if std::fs::create_dir_all(dir).is_ok() {
            let _ = std::fs::write(path, &yaml);
        }
    }
    Ok(loaded)
}

/// Zero-config TUI run: synthesize a `platform: tui` config that drives the
/// given terminal executable in a PTY (the built-in `reproit __tui` runner).
/// The analogue of [`synthesize_web`] for `reproit scan <executable>` (e.g.
/// `lazygit`, `htop`). `executable` is the command line run via `sh -c`, so
/// args and PATH resolution work. Persisted to `.reproit/reproit.yaml` so a
/// follow-up check/keep replays.
pub fn synthesize_tui(executable: &str, root: PathBuf) -> Result<Loaded> {
    // JSON-quote into the YAML (a JSON scalar is a valid, fully-escaped YAML
    // scalar), so a quote/backslash/space in the command can't break the document.
    let exe = serde_json::to_string(executable).unwrap_or_else(|_| "\"\"".to_string());
    // Same ready/done/action markers the `__tui` runner emits (tui.rs: "JOURNEY
    // claimed role=a", "JOURNEY DONE", "All tests passed"), so the orchestrator
    // contract matches without a hand-written reproit.yaml.
    let yaml = format!(
        "app:\n  platform: tui\n  executable: {exe}\n  defines: {{}}\ndevices:\n  namePrefix: \
         tui\nreset:\n  steps: []\njourneys:\n  driver: \"\"\n  readyMarker: \"claimed role\"\n  \
         doneMarkers:\n    - All tests passed\n    - Some tests failed\n  deviceDoneMarker: \
         \"JOURNEY DONE\"\n  actionPrefix: \"JOURNEY\"\n  timeoutSec: 300\nevidence:\n  outDir: \
         .reproit/runs\n  video: false\n",
    );
    let loaded = parse_str(&yaml, root)?;
    let path = crate::layout::config_path(&loaded.root);
    if let Some(dir) = path.parent() {
        if std::fs::create_dir_all(dir).is_ok() {
            let _ = std::fs::write(path, &yaml);
        }
    }
    Ok(loaded)
}
