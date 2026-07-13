//! `reproit fuzz <playwright-test.spec.ts>`: point reproit at a user's EXISTING
//! Playwright test, run it to reach its deep state, then fuzz OUTWARD from there
//! with reproit's oracles. The pitch: "you wrote the test; reproit finds the bugs
//! you didn't" -- in the user's own language, with zero new DSL to learn.
//!
//! We do NOT parse Playwright source. The Node helper `pw-capture.mjs` RUNS the
//! test with tracing on and reads its ACTION TRACE (the trace.zip NDJSON), maps
//! Playwright's structured selector engines to reproit finders, and emits a
//! structured JSON action list. This module is the Rust glue: it invokes the
//! helper, turns the mapped actions into a `from_prefix` (the per-seed prefix the
//! web runner replays before exploring), pulls the start URL from the test's
//! first `page.goto`, prints an honest capture summary (mapped vs skipped),
//! and hands off to the EXISTING fuzz pipeline.
//!
//! Auth: replaying the login click/fill actions in reproit's OWN runner
//! authenticates naturally -- no storageState transplant. The typed values come
//! straight from the trace, i.e. the user's own test data.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::path::Path;
use std::process::Command;

/// One mapped action from the captured trace.
#[derive(Debug, Clone, Deserialize)]
pub struct CapturedAction {
    /// "tap" | "type" (goto is hoisted to `gotoUrl`/`baseURL`, not an action).
    pub kind: String,
    /// The reproit finder (e.g. `key:testid:username`).
    #[allow(dead_code)]
    pub finder: String,
    /// Typed value for `type:` actions.
    #[serde(default)]
    pub value: Option<String>,
    /// The ready-to-replay reproit action string (`tap:<finder>` /
    /// `type:<finder>=<value>`), assembled host-side by the capture helper.
    pub action: String,
    /// The original Playwright selector, for the report.
    #[serde(default)]
    #[allow(dead_code)]
    pub raw: Option<String>,
}

/// A skipped selector reproit could not faithfully map.
#[derive(Debug, Clone, Deserialize)]
pub struct Unsupported {
    pub raw: String,
    pub reason: String,
}

/// The full capture result the helper emits.
#[derive(Debug, Clone, Deserialize)]
pub struct Capture {
    /// The app origin, from the test's first `page.goto`.
    #[serde(rename = "baseURL")]
    pub base_url: Option<String>,
    /// The exact start URL (origin + path), used as the runner's `gotoUrl`.
    #[serde(rename = "gotoUrl")]
    pub goto_url: Option<String>,
    pub actions: Vec<CapturedAction>,
    #[serde(default)]
    pub unsupported: Vec<Unsupported>,
    #[serde(default)]
    pub notes: Vec<String>,
    /// Whether the user's test itself passed (informational; reproit fuzzes
    /// onward regardless, since a failing test can still leave a usable prefix).
    #[serde(default)]
    pub passed: bool,
}

impl Capture {
    /// The replayable prefix: every mapped action's reproit string, in order.
    pub fn replay_prefix(&self) -> Vec<String> {
        self.actions.iter().map(|a| a.action.clone()).collect()
    }

    /// Mapped vs total (mapped + unsupported), for the capture summary.
    pub fn coverage(&self) -> (usize, usize) {
        let mapped = self.actions.len();
        (mapped, mapped + self.unsupported.len())
    }
}

/// Does this target look like a Playwright test FILE we should capture from? A
/// `.spec.ts/.spec.js/.test.ts/.test.js` file that imports `@playwright/test`.
/// (An explicit path the user passed is also accepted by the caller even when
/// the import probe can't read it; this is the cheap auto-detect.)
pub fn looks_like_playwright_test(t: &str) -> bool {
    let p = Path::new(t);
    if !has_pw_test_ext(t) {
        return false;
    }
    match std::fs::read_to_string(p) {
        Ok(src) => imports_playwright_test(&src),
        Err(_) => false,
    }
}

/// The spec/test file extensions reproit captures from.
pub fn has_pw_test_ext(t: &str) -> bool {
    let lower = t.to_ascii_lowercase();
    lower.ends_with(".spec.ts")
        || lower.ends_with(".spec.js")
        || lower.ends_with(".test.ts")
        || lower.ends_with(".test.js")
        || lower.ends_with(".spec.mjs")
        || lower.ends_with(".test.mjs")
}

/// Does the source import the Playwright test runner? (`@playwright/test`.)
pub fn imports_playwright_test(src: &str) -> bool {
    src.contains("@playwright/test")
}

/// Run the capture helper against `test_path`, using `runner_dir` (the provisioned
/// web runner: playwright + browsers + `pw-capture.mjs`). Returns the parsed
/// `Capture`. The helper exits 0 even on a failing test (a failing test can still
/// leave a usable trace prefix), so a non-zero status here is a real harness error.
pub fn capture(runner_dir: &Path, test_path: &Path, log: &dyn Fn(&str)) -> Result<Capture> {
    let script = runner_dir.join("pw-capture.mjs");
    if !script.exists() {
        bail!(
            "pw-capture.mjs not found in the web runner dir ({}). Re-provision the \
             runner (a `reproit fuzz <url>` run installs it).",
            runner_dir.display()
        );
    }
    let test_abs = std::fs::canonicalize(test_path)
        .with_context(|| format!("resolving test path {}", test_path.display()))?;
    log(&format!(
        "running your Playwright test under trace to read its actions: {}",
        test_abs.display()
    ));
    let out = Command::new("node")
        .arg(&script)
        .arg("--test")
        .arg(&test_abs)
        .current_dir(runner_dir)
        // The helper streams the test's own stdout/stderr to OUR stderr; only its
        // final JSON object lands on stdout, so we can parse stdout cleanly.
        .stderr(std::process::Stdio::inherit())
        .output()
        .context("spawning node pw-capture.mjs (is `node` on PATH?)")?;
    if !out.status.success() {
        bail!(
            "pw-capture failed (exit {:?}). See the test output above.",
            out.status.code()
        );
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    // The JSON is the LAST object on stdout (the helper prints nothing else there).
    let json = stdout.trim();
    let cap: Capture = serde_json::from_str(json)
        .with_context(|| format!("parsing pw-capture output:\n{json}"))?;
    Ok(cap)
}

/// Print the honest capture summary: how many
/// of the test's actions reproit replayed, how many it skipped and why, plus the
/// start URL and the auth note. `say` routes lines (stdout, or stderr under --json).
pub fn report(cap: &Capture, test_path: &Path, say: &dyn Fn(&str)) {
    let (mapped, total) = cap.coverage();
    let pct = (mapped * 100).checked_div(total).unwrap_or(100);
    let name = test_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("test");
    let test_state = if cap.passed { "passed" } else { "failed" };
    say(&format!(
        "\nfuzz from Playwright test `{name}` (the test itself {test_state}):"
    ));
    say(&format!(
        "  replayed {mapped}/{total} action(s) from the test ({} skipped, {pct}% mapped), then fuzzed from there",
        cap.unsupported.len()
    ));
    if let Some(u) = &cap.goto_url {
        say(&format!("  start url: {u}"));
    }
    // The replayed prefix, so the user sees exactly what reproit reached step 2 by.
    if !cap.actions.is_empty() {
        say("  replayed actions:");
        for a in &cap.actions {
            let shown = match a.kind.as_str() {
                "type" => format!(
                    "type {} = {}",
                    a.action
                        .trim_start_matches("type:")
                        .split('=')
                        .next()
                        .unwrap_or(&a.action),
                    a.value.clone().unwrap_or_default()
                ),
                _ => a.action.clone(),
            };
            say(&format!("    {shown}"));
        }
    }
    if cap.unsupported.is_empty() {
        say("  every captured action had a faithful reproit finder.");
    } else {
        say("  skipped (no faithful reproit finder; never guessed):");
        for u in &cap.unsupported {
            say(&format!("    {} ({})", u.raw, u.reason));
        }
        say(
            "  fix: give these elements a stable data-testid / id / name, or address \
             them by getByRole(name)/getByText so reproit can replay them.",
        );
    }
    // Honest about notes (extra gotos, weak role finders, etc.).
    for n in &cap.notes {
        if n.starts_with("# note") || n.starts_with("weak finder") {
            say(&format!("  {n}"));
        }
    }
    // Auth: replaying the login fills/clicks authenticates in reproit's OWN runner.
    if cap
        .actions
        .iter()
        .any(|a| a.kind == "type" || a.action.contains("login") || a.action.contains("password"))
    {
        say(
            "  auth: replaying the test's own fill/click actions signs in inside reproit's \
             runner (no storageState transplant; the typed values are your test's data).",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cap_from(json: &str) -> Capture {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn capture_json_converts_to_from_prefix_in_order() {
        let cap = cap_from(
            r#"{
              "baseURL": "http://localhost:8099",
              "gotoUrl": "http://localhost:8099/",
              "actions": [
                {"kind":"type","finder":"key:testid:username","value":"ada","action":"type:key:testid:username=ada","raw":"internal:testid=[data-testid=\"username\"s]"},
                {"kind":"type","finder":"key:testid:password","value":"secret123","action":"type:key:testid:password=secret123","raw":"x"},
                {"kind":"tap","finder":"key:testid:continue","action":"tap:key:testid:continue","raw":"x"}
              ],
              "unsupported": [{"raw":"xpath=//div","reason":"xpath selector"}],
              "notes": ["start url from page.goto: http://localhost:8099/"],
              "passed": true
            }"#,
        );
        assert_eq!(
            cap.replay_prefix(),
            vec![
                "type:key:testid:username=ada".to_string(),
                "type:key:testid:password=secret123".to_string(),
                "tap:key:testid:continue".to_string(),
            ]
        );
        assert_eq!(cap.base_url.as_deref(), Some("http://localhost:8099"));
        assert_eq!(cap.goto_url.as_deref(), Some("http://localhost:8099/"));
        assert_eq!(cap.coverage(), (3, 4)); // 3 mapped, 1 unsupported -> 4 total
        assert!(cap.passed);
    }

    #[test]
    fn detects_pw_test_extensions() {
        assert!(has_pw_test_ext("demo.spec.ts"));
        assert!(has_pw_test_ext("path/to/login.spec.js"));
        assert!(has_pw_test_ext("a.test.ts"));
        assert!(has_pw_test_ext("a.test.js"));
        assert!(has_pw_test_ext("a.spec.mjs"));
        assert!(!has_pw_test_ext("demo.ts"));
        assert!(!has_pw_test_ext("README.md"));
        assert!(!has_pw_test_ext("server.spec")); // no js/ts tail
    }

    #[test]
    fn detects_playwright_import() {
        assert!(imports_playwright_test(
            "import { test, expect } from '@playwright/test';"
        ));
        assert!(imports_playwright_test(
            "const { test } = require(\"@playwright/test\")"
        ));
        assert!(!imports_playwright_test("import { test } from 'vitest';"));
    }

    #[test]
    fn empty_capture_reports_full_coverage() {
        let cap = cap_from(
            r#"{"baseURL":null,"gotoUrl":null,"actions":[],"unsupported":[],"notes":[],"passed":false}"#,
        );
        assert_eq!(cap.coverage(), (0, 0));
        assert!(cap.replay_prefix().is_empty());
    }
}
