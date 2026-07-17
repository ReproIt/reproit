//! Source attribution for a runtime-found operability gap (for the FIX, not the
//! graph). reproit's operability oracle reports a gap by its runtime SELECTOR
//! (the `sel`/`key` grammar: `key:testid:<v>` / `key:id:<v>` / `key:<id>` /
//! `role:<role>#<idx>`). That tells a developer WHAT is wrong but not WHERE to
//! fix it. This module greps the project source for the likely DEFINING
//! location of that id and returns `file:line` candidates, so a found gap is
//! actionable.
//!
//! This is explicitly NOT a static graph (see docs/operability-graph.md "Why
//! not static code analysis"): it never tries to decide what's on screen. It
//! only locates an identifier that the RUNTIME already proved exists, across
//! the common UI dialects:
//!
//!   - React / web:  testID / data-testid / data-test-id / id / name="<id>"
//!   - Flutter:      Key('<id>') / ValueKey('<id>') / key: const Key("<id>")
//!   - XAML (WPF):   x:Name="<id>" / Name="<id>" / AutomationProperties...
//!   - generic:      a bare token match as a last resort
//!
//! Pure-ish: it takes a project root + a runtime id and reads files under it.
//! It does no network and mutates nothing. Output is deterministic (sorted), so
//! the same tree + id always yields the same ranked candidates.
//!
//! The public API (`attribute`, `id_from_selector`, `SourceLocation`) is the
//! fix helper; it is consumed by the gap-reporting / PR path, which the
//! operability feature wires in. `allow(dead_code)` mirrors `model/appmap.rs`:
//! these are model building blocks, populated/consumed by callers added
//! incrementally.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

/// One candidate defining location for a gap's id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceLocation {
    /// Path relative to the project root (forward-slashed for determinism).
    pub file: String,
    /// 1-based line number of the match.
    pub line: usize,
    /// The matched line, trimmed (for the report / PR body).
    pub snippet: String,
    /// Higher = a more specific/intentional match (an explicit testID/Key beats
    /// a bare token). Used only to rank; ties break by (file, line).
    pub score: u32,
}

/// Source files we attribute into. Anything else (assets, lockfiles, build
/// output, binaries) is skipped, both for speed and to avoid false hits.
const SOURCE_EXTS: &[&str] = &[
    "dart", "js", "jsx", "ts", "tsx", "mjs", "cjs", "vue", "svelte", "xaml", "axaml", "cs", "xml",
    "kt", "java", "swift", "html",
];

/// Directories never worth walking: dependency/build/VCS trees. Keeps the scan
/// bounded and avoids attributing a gap to a vendored copy.
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    ".git",
    "build",
    "dist",
    "out",
    "target",
    ".dart_tool",
    "Pods",
    ".reproit",
    "bin",
    "obj",
    ".next",
    ".svelte-kit",
    "coverage",
];

/// Extract the bare id from a runtime selector. The operability oracle keys
/// gaps by reproit's selector grammar; for attribution we only care about the
/// stable developer identifier inside it (the part a developer wrote in
/// source):   key:testid:Foo -> Foo      key:id:Foo -> Foo      key:name:Foo ->
/// Foo   key:Foo        -> Foo      (the RN/native single-id form)
///   role:button#3  -> None     (no developer id; nothing stable to grep)
///   Foo            -> Foo      (already a bare id)
pub fn id_from_selector(sel: &str) -> Option<String> {
    if let Some(role) = sel.strip_prefix("role:") {
        // role:<role>#<idx> carries no developer-authored id, so there is
        // nothing to attribute. (We still guard the `#` form explicitly.)
        if role.contains('#') {
            return None;
        }
        return None;
    }
    if let Some(body) = sel.strip_prefix("key:") {
        // key:<kind>:<v> (web grammar) or key:<id> (native single-id grammar).
        let id = match body.split_once(':') {
            Some(("testid" | "id" | "name", v)) => v,
            _ => body,
        };
        let id = id.trim();
        return if id.is_empty() {
            None
        } else {
            Some(id.to_string())
        };
    }
    let s = sel.trim();
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

/// Score how strongly `line` DEFINES `id` (vs merely mentioning it). Returns 0
/// when the line does not bind the id at all. Higher = a more intentional,
/// framework-specific binding. Pure string scan, no regex compile per line.
fn score_line(line: &str, id: &str) -> u32 {
    if !line.contains(id) {
        return 0;
    }
    // The bindings, strongest first. Each pattern is an attribute/constructor
    // that ASSIGNS the id (so the match is a definition, not a usage).
    // We test `<attr>="<id>"`, `<attr>='<id>'`, and the Flutter `Key('<id>')`.
    let dq = format!("\"{id}\"");
    let sq = format!("'{id}'");
    let has_quoted = line.contains(&dq) || line.contains(&sq);

    // Flutter widget keys: Key('x') / ValueKey("x") / key: const Key('x').
    if has_quoted && (line.contains("Key(") || line.contains("ValueKey(") || line.contains("key:"))
    {
        return 100;
    }
    // Explicit test ids (React Native testID, web data-testid, RTL).
    if has_quoted
        && (line.contains("testID")
            || line.contains("data-testid")
            || line.contains("data-test-id")
            || line.contains("testid"))
    {
        return 95;
    }
    // XAML / WPF names + automation ids (x:Name, Name=, AutomationId).
    if has_quoted
        && (line.contains("x:Name")
            || line.contains("Name=")
            || line.contains("AutomationId")
            || line.contains("AutomationProperties.Name"))
    {
        return 90;
    }
    // HTML/JSX id / name attributes.
    if has_quoted && (line.contains("id=") || line.contains("name=") || line.contains("id:")) {
        return 80;
    }
    // A quoted occurrence of the id anywhere (string literal): likely the value
    // passed to some prop we didn't name-match. Still a strong signal.
    if has_quoted {
        return 60;
    }
    // Bare token (last resort): the id appears unquoted, e.g. a const/var name.
    // Only count it when it is a whole-token match so `foo` doesn't hit `foobar`.
    if is_whole_token(line, id) {
        return 30;
    }
    0
}

/// True when `id` appears in `line` bounded by non-identifier characters on
/// both sides (so a substring inside a larger identifier does not count).
fn is_whole_token(line: &str, id: &str) -> bool {
    let bytes = line.as_bytes();
    let id_bytes = id.as_bytes();
    if id_bytes.is_empty() {
        return false;
    }
    let is_ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut start = 0;
    while let Some(pos) = line[start..].find(id) {
        let abs = start + pos;
        let before_ok = abs == 0 || !is_ident(bytes[abs - 1]);
        let after_idx = abs + id_bytes.len();
        let after_ok = after_idx >= bytes.len() || !is_ident(bytes[after_idx]);
        if before_ok && after_ok {
            return true;
        }
        start = abs + 1;
    }
    false
}

/// Whether a directory entry's name should be skipped (dependency/build/VCS).
fn is_skip_dir(name: &str) -> bool {
    SKIP_DIRS.contains(&name)
}

/// Whether a path has an extension we attribute into.
fn is_source_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| SOURCE_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// Recursively collect source files under `root` (bounded by SKIP_DIRS + a file
/// cap so an enormous monorepo can't make attribution unbounded). Sorted for
/// deterministic traversal.
fn collect_source_files(root: &Path, cap: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut dirs = Vec::new();
        let mut files = Vec::new();
        for e in entries.flatten() {
            let path = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if is_dir {
                if !is_skip_dir(&name) && !name.starts_with('.') {
                    dirs.push(path);
                }
            } else if is_source_file(&path) {
                files.push(path);
            }
        }
        // Sort within each directory so traversal order is stable.
        files.sort();
        dirs.sort();
        for f in files {
            out.push(f);
            if out.len() >= cap {
                return out;
            }
        }
        // Push reversed so the (sorted) first dir is popped first.
        for d in dirs.into_iter().rev() {
            stack.push(d);
        }
    }
    out
}

/// Maximum source files scanned (bounds the worst case on a huge tree).
const FILE_CAP: usize = 20_000;
/// Maximum candidates returned (the top-ranked defining locations).
const MAX_CANDIDATES: usize = 8;

/// Attribute a runtime-found gap (by its selector OR bare id) to candidate
/// `file:line` source locations under `root`, best match first. Returns an
/// empty vec when the selector carries no developer id (e.g. `role:button#3`)
/// or when nothing matches. Deterministic: same tree + id -> same ranked
/// output.
pub fn attribute(root: &Path, selector: &str) -> Vec<SourceLocation> {
    let Some(id) = id_from_selector(selector) else {
        return Vec::new();
    };
    let mut hits: Vec<SourceLocation> = Vec::new();
    for path in collect_source_files(root, FILE_CAP) {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue; // binary / unreadable: skip
        };
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        for (i, line) in text.lines().enumerate() {
            let score = score_line(line, &id);
            if score > 0 {
                hits.push(SourceLocation {
                    file: rel.clone(),
                    line: i + 1,
                    snippet: line.trim().to_string(),
                    score,
                });
            }
        }
    }
    // Rank: highest score first, then file then line for a stable order.
    hits.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then(a.file.cmp(&b.file))
            .then(a.line.cmp(&b.line))
    });
    hits.truncate(MAX_CANDIDATES);
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_extraction_handles_every_selector_grammar() {
        assert_eq!(
            id_from_selector("key:testid:SaveBtn").as_deref(),
            Some("SaveBtn")
        );
        assert_eq!(id_from_selector("key:id:login").as_deref(), Some("login"));
        assert_eq!(id_from_selector("key:name:email").as_deref(), Some("email"));
        // Native single-id form (RN resource-id / accessibility-id).
        assert_eq!(
            id_from_selector("key:submit_button").as_deref(),
            Some("submit_button")
        );
        // role:<role>#<idx> has no developer id -> nothing to attribute.
        assert_eq!(id_from_selector("role:button#3"), None);
        assert_eq!(id_from_selector("role:option#0"), None);
        // A bare id passes through.
        assert_eq!(id_from_selector("myWidget").as_deref(), Some("myWidget"));
        // Empty / id-less selectors yield None.
        assert_eq!(id_from_selector("key:"), None);
        assert_eq!(id_from_selector(""), None);
    }

    #[test]
    fn whole_token_match_does_not_fire_on_substrings() {
        assert!(is_whole_token("const submit = 1;", "submit"));
        assert!(!is_whole_token("const submitButton = 1;", "submit"));
        assert!(is_whole_token("foo(bar)", "bar"));
        assert!(!is_whole_token("foobar", "bar"));
    }

    #[test]
    fn scoring_prefers_intentional_bindings_over_bare_mentions() {
        let id = "saveBtn";
        // Flutter Key beats a testID beats an html id beats a bare token.
        assert!(
            score_line("key: const Key('saveBtn'),", id)
                > score_line("<View testID=\"saveBtn\">", id)
        );
        assert!(
            score_line("<View testID=\"saveBtn\">", id) > score_line("<div id=\"saveBtn\">", id)
        );
        assert!(score_line("<div id=\"saveBtn\">", id) > score_line("const saveBtn = ref();", id));
        // A line not containing the id scores 0.
        assert_eq!(score_line("const other = 1;", id), 0);
    }

    #[test]
    fn attributes_a_react_native_gap_to_its_component_definition() {
        // A runtime gap on testID "checkout" must be located in the JSX source.
        let dir = std::env::temp_dir().join(format!("reproit-attr-rn-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("node_modules/pkg")).unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("src/Checkout.tsx"),
            "import x from 'y';\nfunction Checkout() {\n\treturn <TouchableOpacity \
             testID=\"checkout\" onPress={pay} />;\n}\n",
        )
        .unwrap();
        // A vendored copy under node_modules MUST be ignored (never attributed).
        std::fs::write(
            dir.join("node_modules/pkg/index.js"),
            "export const checkout = 'checkout';\n",
        )
        .unwrap();

        let hits = attribute(&dir, "key:testid:checkout");
        assert!(!hits.is_empty(), "should find the checkout testID");
        let top = &hits[0];
        assert_eq!(top.file, "src/Checkout.tsx");
        assert_eq!(top.line, 3, "the testID is on line 3");
        // node_modules is skipped, so no hit points into it.
        assert!(
            hits.iter().all(|h| !h.file.contains("node_modules")),
            "vendored copies must not be attributed"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn attributes_a_flutter_widget_key_and_a_xaml_name() {
        let dir = std::env::temp_dir().join(format!("reproit-attr-multi-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("home.dart"),
            "ElevatedButton(\n  key: const Key('save'),\n  onPressed: doSave,\n);\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("MainWindow.xaml"),
            "<Button x:Name=\"save\" Content=\"Save\" />\n",
        )
        .unwrap();

        let hits = attribute(&dir, "save");
        // Both the Flutter Key and the XAML x:Name should be found; the Key
        // (score 100) ranks above the x:Name (score 90).
        assert!(hits.len() >= 2, "both bindings found, got {:?}", hits);
        assert_eq!(hits[0].file, "home.dart");
        assert_eq!(hits[0].line, 2);
        assert!(hits.iter().any(|h| h.file == "MainWindow.xaml"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn output_is_deterministic_and_empty_for_id_less_selectors() {
        let dir = std::env::temp_dir().join(format!("reproit-attr-det-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.js"), "const foo = 'bar';\n").unwrap();
        // role:<role>#<idx> carries no id -> empty, no scan needed.
        assert!(attribute(&dir, "role:button#2").is_empty());
        // Same tree + id -> identical ranked output on every call.
        assert_eq!(attribute(&dir, "key:id:bar"), attribute(&dir, "key:id:bar"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
