//! `reproit fix`: the agentic fix generator. Everyone else's self-healing
//! patches the TEST; this patches the APP, grounded in evidence.
//!
//! Flow: read structured findings (exceptions.jsonl) from a run, resolve each
//! to an app source `file:line` (the top app frame in the framework's own
//! stack), read a window of source around it, and build a tight prompt that
//! carries the bug class + message, the code context, and the minimized repro,
//! asking for the SMALLEST surgical change that fixes the root cause. A
//! write-capable CLI agent applies it as a working-tree diff (the proposal for
//! review); we print the diff and never auto-commit. If no write-capable
//! provider is configured, we print the constructed prompt + the target
//! file:line and stop (a dry-run), rather than failing silently.
//!
//! Acceptance for a fix is the same as for a test: the gate. The working-tree
//! diff IS the proposed patch, for a human (or the calling agent) to review.

use crate::config::Config;
use anyhow::{Context, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};

const MAX_FIXES_PER_RUN: usize = 2;
/// Lines of source on each side of the suspected line to show the model.
const SOURCE_WINDOW: u32 = 40;

struct Finding {
    kind: String,
    message: String,
    /// App-relative source path (relative to project_dir) and line.
    file: String,
    line: u32,
}

/// A11y fix class: states with unlabeled tappables, from the live map.
/// One agent task covers all of them (they share a grep strategy).
pub async fn fix_a11y(cfg: &Config, root: &Path) -> Result<()> {
    let provider = llm::from_spec(&cfg.llm.to_spec())?;
    if !provider.can_write() {
        anyhow::bail!(
            "reproit fix needs a write-capable CLI provider (codex-cli or claude-cli); configured: {}",
            provider.name()
        );
    }
    let map = crate::map::load_map(root, cfg);
    let offenders: Vec<(String, &crate::appmap::State)> = map
        .states
        .iter()
        .filter(|(_, st)| st.unlabeled_tappables > 0)
        .map(|(id, st)| (id.clone(), st))
        .collect();
    if offenders.is_empty() {
        println!("  a11y: no unlabeled tappables in the map (run reproit map/fuzz first)");
        return Ok(());
    }
    let mut listing = String::new();
    for (id, st) in &offenders {
        listing.push_str(&format!(
            "- screen `{id}` (visible labels: {}): {} unlabeled tappable(s)\n",
            st.description, st.unlabeled_tappables
        ));
    }
    println!("  a11y findings:\n{listing}");
    let project_dir = root.join(&cfg.app.project_dir);
    let prompt = format!(
        r#"You are reproit's accessibility fix generator. Exploration of this Flutter app
found tappable widgets with NO semantics label (invisible to screen readers and
to label-driven automation) on these screens:

{listing}
Locate each offender in lib/ (typical culprits: IconButton without tooltip,
GestureDetector/InkWell wrapping a bare Icon without a Semantics label) and add
the SMALLEST appropriate fix: a tooltip on IconButtons, or a Semantics(label:)
wrapper otherwise. Choose labels that describe the action. Do not refactor or
change behavior. After editing, reply with one line per fix: file:line and the
label you added."#
    );
    let summary = provider
        .complete(&llm::Task::new(prompt).workdir(&project_dir).write())
        .await?;
    let analyze = crate::exec::run_shell("flutter analyze lib 2>&1 | tail -2", &project_dir).await;
    let diff = crate::exec::run_shell(
        &format!("git diff --stat -- {}/lib", cfg.app.project_dir),
        root,
    )
    .await;
    println!("  agent: {}", summary.trim());
    println!("  diff:\n{}", diff.stdout.trim());
    println!("  analyze: {}", analyze.stdout.trim());
    println!("  verify: re-run reproit map; unlabeled counts should drop to 0, then gate");
    Ok(())
}

pub async fn fix(cfg: &Config, root: &Path, run: Option<&str>) -> Result<()> {
    let provider = llm::from_spec(&cfg.llm.to_spec())?;
    let runs_dir = root.join(&cfg.evidence.out_dir);
    let run_dir = match run {
        Some(name) => runs_dir.join(name),
        None => latest_run(&runs_dir)?,
    };
    let project_dir = root.join(&cfg.app.project_dir);
    let findings = load_findings(&run_dir, &project_dir)?;
    if findings.is_empty() {
        println!(
            "  no app-source findings in {} (exceptions.jsonl)",
            run_dir.display()
        );
        return Ok(());
    }
    // The minimized repro is shared across this run's findings; read it once.
    let repro = read_repro(&run_dir);

    // Decide write vs dry-run ONCE, up front, and say which clearly. A dry-run
    // is not a failure: we still build and show the exact prompt + target.
    let writeable = provider.can_write();
    if writeable {
        println!(
            "  provider {} can apply patches; {} app-source finding(s), attempting up to {MAX_FIXES_PER_RUN}",
            provider.name(),
            findings.len()
        );
    } else {
        println!(
            "  provider {} is read-only: no patch will be applied. Dry-run below shows the\n  \
exact prompt + target each finding WOULD be fixed with. Configure a write-capable\n  \
provider (codex-cli or claude-cli) in reproit.yaml to apply.",
            provider.name()
        );
        println!(
            "  {} app-source finding(s), showing up to {MAX_FIXES_PER_RUN}",
            findings.len()
        );
    }

    let mut report = String::from("# reproit fix report\n\n");
    for finding in findings.iter().take(MAX_FIXES_PER_RUN) {
        println!(
            "  finding {}:{} ({})",
            finding.file, finding.line, finding.kind
        );
        let context = read_source_window(&project_dir, &finding.file, finding.line);
        let prompt = build_prompt(finding, &context, repro.as_deref());

        if !writeable {
            // Dry-run: print the target + the constructed prompt, do not apply.
            println!(
                "  DRY-RUN (no write-capable provider): would fix {}:{}",
                finding.file, finding.line
            );
            println!("  ---- constructed prompt ----\n{prompt}\n  ---- end prompt ----");
            report.push_str(&format!(
                "## {}:{} (dry-run)\n\n{}: no write-capable provider configured.\n\n\
Target: `{}:{}`\n\nConstructed prompt:\n\n```\n{}\n```\n\n",
                finding.file, finding.line, finding.kind, finding.file, finding.line, prompt
            ));
            continue;
        }

        let result = provider
            .complete(&llm::Task::new(prompt).workdir(&project_dir).write())
            .await;
        match result {
            Ok(summary) => {
                let diff = crate::exec::run_shell(
                    &format!("git diff -- {}/{}", cfg.app.project_dir, finding.file),
                    root,
                )
                .await;
                // Verify clean only for Flutter projects (where we have a fast
                // analyzer). Other platforms: rely on the gate after review.
                let analyze = if is_flutter(cfg) {
                    crate::exec::run_shell(
                        &format!("flutter analyze {} 2>&1 | tail -3", finding.file),
                        &project_dir,
                    )
                    .await
                    .stdout
                } else {
                    String::new()
                };
                let verdict = if diff.stdout.trim().is_empty() {
                    "NO CHANGE (agent made no edit)"
                } else if analyze.contains("error") {
                    "PATCHED BUT ANALYZE REPORTS ERRORS: review required"
                } else {
                    "patched (review the diff, then gate)"
                };
                println!("  {verdict}: {}:{}", finding.file, finding.line);
                if !diff.stdout.trim().is_empty() {
                    println!(
                        "  ---- proposed diff ----\n{}\n  ---- end diff ----",
                        diff.stdout.trim()
                    );
                }
                report.push_str(&format!(
                    "## {}:{}\n\n{} ({})\n\nAgent summary: {}\n\n```diff\n{}\n```\n\n",
                    finding.file,
                    finding.line,
                    finding.kind,
                    verdict,
                    summary.trim(),
                    diff.stdout.trim(),
                ));
                if !analyze.trim().is_empty() {
                    report.push_str(&format!("flutter analyze: {}\n\n", analyze.trim()));
                }
            }
            Err(e) => {
                println!("  warn: fix attempt failed: {e}");
                report.push_str(&format!(
                    "## {}:{}\n\nfix attempt failed: {e}\n\n",
                    finding.file, finding.line
                ));
            }
        }
    }
    let out = run_dir.join("fixes.md");
    std::fs::write(&out, &report)?;
    println!("  report: {}", out.display());
    if writeable {
        println!("  next: review the diff, then gate the relevant journey before committing");
    } else {
        println!("  next: configure a write-capable provider, then re-run reproit fix");
    }
    Ok(())
}

/// True for Flutter projects (we have `flutter analyze` to verify the patch).
fn is_flutter(cfg: &Config) -> bool {
    cfg.app.platform.starts_with("flutter")
}

/// Build the per-finding prompt: bug class + message, the code context window,
/// the minimized repro, and the surgical-change constraints. Kept pure so it
/// can be unit-tested and shown verbatim in the dry-run path.
fn build_prompt(finding: &Finding, context: &str, repro: Option<&str>) -> String {
    let repro_block = match repro {
        Some(r) if !r.trim().is_empty() => format!(
            "\nThe deterministic minimized repro (the input sequence that triggers it):\n\
```\n{}\n```\n",
            r.trim()
        ),
        _ => String::new(),
    };
    format!(
        r#"You are reproit's fix generator. A deterministic test run of this app captured
this failure, with the source location taken from the framework's own diagnostics
(the top app frame in the exception stack):

kind:    {kind}
message: {message}
where:   {file}:{line}
{repro_block}
Source around {file}:{line} (line {line} is the suspected site):

```
{context}
```

Edit ONLY {file} with the SMALLEST change that fixes the ROOT CAUSE of this
defect. Hard constraints:
- Touch ONLY {file}. Do not refactor, rename, reformat unrelated code, or add
  dependencies.
- Preserve all intended behavior and visual design; fix only the defect.
- Make a surgical change, not a rewrite. Match the surrounding code's style.

Class-specific guidance (apply only the one that fits):
- Leaked resource (e.g. "A <T> was found in <state> ... not disposed", a leaked
  AnimationController/StreamSubscription/Timer/controller): add or extend the
  owning State's `dispose()` override to dispose/cancel it, calling
  super.dispose() last. Create the dispose() override if it does not exist.
- Layout overflow (RenderFlex overflowed): make the offending content scrollable
  or constrained (SingleChildScrollView / Flexible / Expanded), matching intent.
- Null/range/state error: add the minimal guard at the failing site.

After editing, reply with 2-3 sentences: what was wrong and exactly what you
changed (the method/lines).
"#,
        kind = finding.kind,
        message = finding.message,
        file = finding.file,
        line = finding.line,
        repro_block = repro_block,
        context = context,
    )
}

/// Read a window of source around `line` (1-based), with line-number gutters so
/// the model can anchor its edit. Returns a short note if the file is missing.
fn read_source_window(project_dir: &Path, rel: &str, line: u32) -> String {
    let abs = project_dir.join(rel);
    let Ok(src) = std::fs::read_to_string(&abs) else {
        return format!("(could not read {})", abs.display());
    };
    let lines: Vec<&str> = src.lines().collect();
    let n = lines.len() as u32;
    let start = line.saturating_sub(SOURCE_WINDOW).max(1);
    let end = (line + SOURCE_WINDOW).min(n.max(1));
    let mut out = String::new();
    for i in start..=end {
        let idx = (i - 1) as usize;
        if let Some(text) = lines.get(idx) {
            let marker = if i == line { ">>" } else { "  " };
            out.push_str(&format!("{marker} {i:>5} | {text}\n"));
        }
    }
    out
}

/// The minimized repro for this run's finding, read from fuzz.md's repro block
/// (the fenced block under "## repro"). Best-effort: None if absent.
fn read_repro(run_dir: &Path) -> Option<String> {
    let md = std::fs::read_to_string(run_dir.join("fuzz.md")).ok()?;
    parse_repro_block(&md)
}

/// Extract the fenced code block that follows a "## repro" heading in fuzz.md.
fn parse_repro_block(md: &str) -> Option<String> {
    let mut lines = md.lines();
    // Find the repro heading.
    for line in lines.by_ref() {
        if line.trim_start().starts_with("## repro") {
            break;
        }
    }
    // Skip to the opening fence.
    let mut in_block = false;
    let mut body = Vec::new();
    for line in lines {
        if line.trim_start().starts_with("```") {
            if in_block {
                break;
            }
            in_block = true;
            continue;
        }
        if in_block {
            body.push(line);
        }
    }
    let joined = body.join("\n");
    (!joined.trim().is_empty()).then_some(joined)
}

/// Parse exceptions.jsonl into app-source findings (frames inside the app),
/// deduped by file:line.
fn load_findings(run_dir: &Path, project_dir: &Path) -> Result<Vec<Finding>> {
    let path = run_dir.join("exceptions.jsonl");
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("no exceptions.jsonl in {}", run_dir.display()))?;
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for line in raw.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let frames = v
            .get("frames")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for frame in &frames {
            let Some(frame) = frame.as_str() else {
                continue;
            };
            if let Some((file, lineno)) = app_location(frame, project_dir) {
                if seen.insert(format!("{file}:{lineno}")) {
                    out.push(Finding {
                        kind: v
                            .get("kind")
                            .and_then(Value::as_str)
                            .unwrap_or("EXCEPTION")
                            .to_string(),
                        message: v
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .chars()
                            .take(300)
                            .collect(),
                        file,
                        line: lineno,
                    });
                }
                break; // first app frame per exception is the fix target
            }
        }
    }
    Ok(out)
}

/// Extract a project-relative app source path + line from a single stack frame,
/// across the frame formats reproit produces:
///
/// - Dart `package:` "package:bugzoo/main.dart:672:45" -> lib/main.dart:672
/// - Dart `file://` "Column:file:///abs/.../lib/x.dart:90:18" -> lib/x.dart:90
/// - JS "at fn (webpack:///./src/app.js:12:3)" or file:// -> src/app.js:12
/// - Swift "file:///abs/.../Sources/Foo.swift:88" -> the rel path
/// - Kotlin/JVM "at com.x.MainActivity.onCreate(MainActivity.kt:42)"
///
/// Only frames that resolve to a file UNDER project_dir (or, for Dart packages,
/// to an existing lib/ file) are returned: third-party/SDK frames are skipped.
fn app_location(frame: &str, project_dir: &Path) -> Option<(String, u32)> {
    // Dart package frames map to lib/ within the app's own package.
    if let Some(idx) = frame.find("package:") {
        let rest = &frame[idx + "package:".len()..];
        // rest = "<app>/sub/path.ext:LINE[:COL]"
        let (_pkg, sub_with_loc) = rest.split_once('/')?;
        let (sub, line) = strip_line_col(sub_with_loc)?;
        let rel = format!("lib/{sub}");
        return project_dir.join(&rel).exists().then_some((rel, line));
    }

    // file:// frames: take the absolute path, strip to project-relative.
    if let Some(idx) = frame.find("file://") {
        let after = &frame[idx + "file://".len()..];
        let after = after.trim_start_matches('/'); // file:///abs -> abs (re-add /)
        let abs_str = format!("/{after}");
        let (path_str, line) = strip_line_col(&abs_str)?;
        let abs = PathBuf::from(path_str);
        let rel = abs.strip_prefix(project_dir).ok()?;
        return Some((rel.to_string_lossy().into_owned(), line));
    }

    // webpack:// JS frames: "webpack:///./src/app.js:12:3".
    if let Some(idx) = frame.find("webpack://") {
        let after = &frame[idx + "webpack://".len()..];
        let after = after.trim_start_matches('/').trim_start_matches("./");
        let (sub, line) = strip_line_col(after)?;
        return project_dir.join(&sub).exists().then_some((sub, line));
    }

    // Plain path frames with a recognized source extension (Swift/Kotlin/JS),
    // possibly wrapped in "(...)" (Kotlin) or prefixed by a symbol. Pull the
    // longest token that ends in a known extension + :LINE.
    if let Some((path, line)) = scan_path_with_ext(frame) {
        let abs = PathBuf::from(&path);
        // Absolute path under the project, or a relative path that exists there.
        if abs.is_absolute() {
            if let Ok(rel) = abs.strip_prefix(project_dir) {
                return Some((rel.to_string_lossy().into_owned(), line));
            }
            return None;
        }
        return project_dir.join(&path).exists().then_some((path, line));
    }
    None
}

/// Source file extensions reproit knows how to map back to app source.
const SOURCE_EXTS: &[&str] = &[".dart", ".js", ".jsx", ".ts", ".tsx", ".swift", ".kt"];

/// Split "some/path.ext:LINE[:COL][ trailing]" into ("some/path.ext", LINE).
/// Accepts an optional column and ignores anything after it (e.g. a ")" ).
fn strip_line_col(s: &str) -> Option<(String, u32)> {
    let ext = SOURCE_EXTS.iter().find_map(|e| s.find(e).map(|i| (i, e)))?;
    let (idx, e) = ext;
    let path_end = idx + e.len();
    let path = s[..path_end].to_string();
    // After the extension we expect ":LINE". Take the first all-digit run.
    let after = &s[path_end..];
    let after = after.strip_prefix(':')?;
    let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
    let line: u32 = digits.parse().ok()?;
    Some((path, line))
}

/// Find a `<path><ext>:LINE` anywhere in a free-form frame string (Swift/Kotlin/
/// JS stack lines), tolerating surrounding punctuation. Returns the path token
/// and line. Picks the path by walking back from the extension to the start of
/// the token (stopping at whitespace, '(', or '"').
fn scan_path_with_ext(frame: &str) -> Option<(String, u32)> {
    let bytes = frame.as_bytes();
    for e in SOURCE_EXTS {
        let mut search_from = 0;
        while let Some(rel) = frame[search_from..].find(e) {
            let ext_start = search_from + rel;
            let ext_end = ext_start + e.len();
            // The next char after the ext must be ':' (a line follows).
            if frame[ext_end..].starts_with(':') {
                // Walk back to the token start.
                let mut start = ext_start;
                while start > 0 {
                    let c = bytes[start - 1];
                    if c == b' ' || c == b'\t' || c == b'(' || c == b'"' || c == b'\'' {
                        break;
                    }
                    start -= 1;
                }
                if let Some((path, line)) = strip_line_col(&frame[start..]) {
                    return Some((path, line));
                }
            }
            search_from = ext_end;
        }
    }
    None
}

fn latest_run(runs_dir: &Path) -> Result<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(runs_dir)
        .with_context(|| format!("no runs under {}", runs_dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    dirs.pop()
        .with_context(|| format!("no runs under {}", runs_dir.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dart_package_frame_maps_to_lib_when_file_exists() {
        let tmp = std::env::temp_dir().join(format!("reproit-fix-dart-{}", std::process::id()));
        std::fs::create_dir_all(tmp.join("lib")).unwrap();
        std::fs::write(tmp.join("lib/main.dart"), "x\n").unwrap();
        let got = app_location(
            "#5 _ComposeSheetState (package:bugzoo/main.dart:672:45)",
            &tmp,
        );
        assert_eq!(got, Some(("lib/main.dart".to_string(), 672)));
        // A package frame for a file that does not exist (a dependency) -> None.
        assert_eq!(
            app_location("package:flutter/src/widgets/foo.dart:10:1", &tmp),
            None
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn dart_file_uri_frame_strips_to_project_relative() {
        let tmp = std::env::temp_dir().join(format!("reproit-fix-file-{}", std::process::id()));
        let rel = "lib/screens/profile.dart";
        std::fs::create_dir_all(tmp.join("lib/screens")).unwrap();
        std::fs::write(tmp.join(rel), "x\n").unwrap();
        let frame = format!("Column:file://{}:90:18", tmp.join(rel).display());
        assert_eq!(app_location(&frame, &tmp), Some((rel.to_string(), 90)));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn kotlin_frame_extracts_path_and_line() {
        let tmp = std::env::temp_dir().join(format!("reproit-fix-kt-{}", std::process::id()));
        let rel = "app/MainActivity.kt";
        std::fs::create_dir_all(tmp.join("app")).unwrap();
        std::fs::write(tmp.join(rel), "x\n").unwrap();
        // Kotlin stack frame shape; relative path resolved against project_dir.
        let frame = "at com.x.MainActivity.onCreate(app/MainActivity.kt:42)";
        assert_eq!(app_location(frame, &tmp), Some((rel.to_string(), 42)));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn swift_absolute_frame_strips_to_relative() {
        let tmp = std::env::temp_dir().join(format!("reproit-fix-swift-{}", std::process::id()));
        let rel = "Sources/Feed.swift";
        std::fs::create_dir_all(tmp.join("Sources")).unwrap();
        std::fs::write(tmp.join(rel), "x\n").unwrap();
        let frame = format!("0  MyApp  file://{}:88:5", tmp.join(rel).display());
        assert_eq!(app_location(&frame, &tmp), Some((rel.to_string(), 88)));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn js_webpack_frame_resolves() {
        let tmp = std::env::temp_dir().join(format!("reproit-fix-js-{}", std::process::id()));
        let rel = "src/app.js";
        std::fs::create_dir_all(tmp.join("src")).unwrap();
        std::fs::write(tmp.join(rel), "x\n").unwrap();
        let frame = "at handler (webpack:///./src/app.js:12:3)";
        assert_eq!(app_location(frame, &tmp), Some((rel.to_string(), 12)));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn repro_block_parsed_from_fuzz_md() {
        let md = "\
# fuzz finding (seed 1)

## invariants violated

- **no-leak** (1)

## repro (2 actions, shrunk from 9)

```
tap:Compose
back
```

Replay: write ...
";
        assert_eq!(parse_repro_block(md).as_deref(), Some("tap:Compose\nback"));
        assert_eq!(parse_repro_block("no repro here"), None);
    }

    #[test]
    fn source_window_marks_the_suspect_line() {
        let tmp = std::env::temp_dir().join(format!("reproit-fix-win-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("a.dart"), "l1\nl2\nl3\nl4\nl5\n").unwrap();
        let w = read_source_window(&tmp, "a.dart", 3);
        assert!(w.contains(">>     3 | l3"), "got: {w}");
        assert!(w.contains("     1 | l1"));
        assert!(w.contains("     5 | l5"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn prompt_includes_context_repro_and_target() {
        let f = Finding {
            kind: "EXCEPTION CAUGHT BY WIDGETS LIBRARY".into(),
            message: "A leaked AnimationController was found".into(),
            file: "lib/main.dart".into(),
            line: 672,
        };
        let p = build_prompt(
            &f,
            ">>   672 | late final AnimationController _spinner",
            Some("tap:Compose\nback"),
        );
        assert!(p.contains("lib/main.dart:672"));
        assert!(p.contains("leaked AnimationController"));
        assert!(p.contains("_spinner"));
        assert!(p.contains("tap:Compose"));
        assert!(p.contains("dispose()"));
        // No repro block when none supplied.
        let p2 = build_prompt(&f, "ctx", None);
        assert!(!p2.contains("minimized repro"));
    }
}
