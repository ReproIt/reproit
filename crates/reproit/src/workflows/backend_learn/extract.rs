//! Static route derivation for `reproit init --learn`.
//!
//! This file used to BE the extraction: a pattern per framework family, run
//! line by line over a bounded set of source files. It is now the dispatch and
//! the shared vocabulary, and every family reads through its own grammar. What
//! remains here is the file walk each reader shares, the path normalizer they
//! all emit through, and the shape they all return.

use super::dotnet_ast;
use super::go_ast;
use super::grammar::SourceRead;
use super::java_ast;
use super::node_ast;
use super::php_ast;
use super::python_ast;
use super::ruby_ast;
use super::rust_ast;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// HTTP methods a derived draft may claim, in emission order.
pub(super) const METHODS: [&str; 7] = ["get", "post", "put", "patch", "delete", "head", "options"];

// These bound the walk so a pathological tree cannot hang a CI job. They were
// set for a single small service and were wrong for real repositories: a
// canonical Spring controller lives at `src/main/java/com/example/.../X.java`,
// which is already at the old depth of 8, so one more package level made an
// entire codebase read as empty. Every limit is now generous enough that
// hitting one is unusual, and hitting one is REPORTED rather than silent,
// because a file a limit excluded is a file whose absence proves nothing.
const MAX_FILES: usize = 20_000;
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_WALK_DEPTH: usize = 24;

/// Directories never containing first-party route definitions.
const SKIP_DIRS: [&str; 13] = [
    "node_modules",
    "target",
    "vendor",
    "dist",
    "build",
    "__pycache__",
    "venv",
    "migrations",
    // Non-shipping targets. A Cargo `examples/`, `tests/` or `benches/` binary
    // declares routes that the service does not serve, and reading them as real
    // surface reports a stale bench fixture as an undeclared endpoint.
    "examples",
    "tests",
    "test",
    "benches",
    "testdata",
];

#[derive(Debug, Default)]
pub(crate) struct Derived {
    /// path -> methods, both normalized (`{id}` params, lowercase methods).
    pub(super) routes: BTreeMap<String, BTreeSet<&'static str>>,
    pub(super) files_scanned: usize,
    /// Pattern hits dropped because the path could not be normalized.
    pub(super) skipped: usize,
    /// (METHOD, resolved path) -> the handler function serving it. It is what
    /// lets the contract check compare declared TYPES against the code, not
    /// just declared paths.
    pub(super) handlers: BTreeMap<(String, String), String>,
    /// Source files the reader could not read at all. Any absence it reports
    /// while this is non-zero is unreliable, and the report must say so.
    pub(super) unreadable: usize,
    /// Source files a walk LIMIT excluded before any parse: too large, too
    /// deeply nested, or past the file cap.
    pub(super) unscanned: usize,
    /// handler -> request body fields, where the family has a parser for them.
    pub(super) bodies: BTreeMap<String, BTreeMap<String, super::field_facts::FieldFact>>,
}

impl Derived {
    pub(super) fn operation_count(&self) -> usize {
        self.routes.values().map(BTreeSet::len).sum()
    }
}

/// The framework families `--learn` can extract routes for, mapped from the
/// `backend_detect` framework names.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Family {
    Rust,
    Node,
    Python,
    Go,
    Ruby,
    Spring,
    Php,
    DotNet,
}

pub(super) fn family_for(framework: &str) -> Option<Family> {
    Some(match framework {
        "axum" | "actix-web" | "rocket" | "warp" => Family::Rust,
        "express" | "fastify" | "koa" | "hapi" | "nestjs" => Family::Node,
        "fastapi" | "flask" | "django" => Family::Python,
        "gin" | "echo" | "fiber" | "chi" | "net/http" => Family::Go,
        "rails" | "sinatra" => Family::Ruby,
        "spring" | "java" => Family::Spring,
        "laravel" => Family::Php,
        "aspnet" => Family::DotNet,
        _ => return None,
    })
}

fn extensions(family: Family) -> &'static [&'static str] {
    match family {
        Family::Rust => &["rs"],
        Family::Node => &["js", "mjs", "cjs", "ts", "tsx", "jsx"],
        Family::Python => &["py"],
        Family::Go => &["go"],
        Family::Ruby => &["rb"],
        Family::Spring => &["java", "kt"],
        Family::Php => &["php"],
        Family::DotNet => &["cs"],
    }
}

/// Derive routes for a detected framework from the project's source files.
///
/// Every family reads through a real parse now. That is not about matching
/// more: it is that a parser KNOWS WHEN IT FAILED, and a pattern cannot. Every
/// false "the source does not serve this operation" this tool has reported came
/// from an unreadable construct being indistinguishable from an absent one, so
/// a file that does not parse is COUNTED and the caller qualifies its own
/// conclusions rather than asserting an absence it has no standing to assert.
pub(super) fn derive(root: &Path, framework: &str) -> Option<Derived> {
    let parsed = match family_for(framework)? {
        Family::Python => python_ast::read(root),
        Family::Node => node_ast::read(root),
        Family::Go => go_ast::read(root),
        Family::Ruby => ruby_ast::read(root),
        Family::Php => php_ast::read(root),
        Family::Spring => java_ast::read(root),
        Family::DotNet => dotnet_ast::read(root),
        // Rust reads through `syn`, a full parse rather than a grammar, and
        // resolves its paths as it goes.
        Family::Rust => {
            let parsed = rust_ast::read(root);
            return Some(Derived {
                routes: parsed.routes,
                handlers: parsed.handlers,
                files_scanned: parsed.files_parsed,
                skipped: parsed.files_unparsed,
                unreadable: parsed.files_unparsed,
                unscanned: skipped_by_limit(root, Family::Rust),
                bodies: parsed.bodies,
            });
        }
    };
    let mut derived = from_parse(parsed);
    derived.unscanned = skipped_by_limit(root, family_for(framework)?);
    Some(derived)
}

/// Fold one grammar reader's result into the shared shape, normalizing paths.
fn from_parse(parsed: SourceRead) -> Derived {
    let mut derived = Derived {
        files_scanned: parsed.files_parsed,
        unreadable: parsed.files_unreadable,
        bodies: parsed.bodies,
        ..Derived::default()
    };
    for (raw, method, handler) in parsed.routes {
        match normalize_path(&raw) {
            Some(path) => {
                derived
                    .routes
                    .entry(path.clone())
                    .or_default()
                    .insert(method);
                if let Some(handler) = handler {
                    derived
                        .handlers
                        .insert((method.to_uppercase(), path), handler);
                }
            }
            None => derived.skipped += 1,
        }
    }
    derived
}

/// The sources of one family, so a type reader sees exactly the file set the
/// route reader saw.
pub(super) fn family_sources(root: &Path, family: Family) -> Vec<PathBuf> {
    source_files(root, extensions(family)).files
}

/// How many source files a LIMIT kept the reader from opening: too large, too
/// deeply nested, or past the file cap. Distinct from a file that was opened
/// and would not parse, and reported separately, because the two call for
/// different fixes. Either way an absence over them is not evidence.
pub(super) fn skipped_by_limit(root: &Path, family: Family) -> usize {
    source_files(root, extensions(family)).skipped
}

/// A bounded walk and what it had to leave out.
struct Scan {
    files: Vec<PathBuf>,
    skipped: usize,
}

/// Bounded, deterministic source walk: sorted entries, capped depth and count,
/// skip directories that never hold first-party routes.
fn source_files(root: &Path, extensions: &[&str]) -> Scan {
    let mut files = Vec::new();
    let mut skipped = 0usize;
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        if depth > MAX_WALK_DEPTH || files.len() >= MAX_FILES {
            // Whatever is under here is unread, and saying so is the point.
            skipped += count_sources(&dir, extensions);
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut entries: Vec<_> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
        entries.sort();
        for path in entries {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            if path.is_dir() {
                if !name.starts_with('.') && !SKIP_DIRS.contains(&name) {
                    stack.push((path, depth + 1));
                }
                continue;
            }
            let extension = path.extension().and_then(|ext| ext.to_str()).unwrap_or("");
            if !extensions.contains(&extension) {
                continue;
            }
            let small = std::fs::metadata(&path).is_ok_and(|meta| meta.len() <= MAX_FILE_BYTES);
            if small && files.len() < MAX_FILES {
                files.push(path);
            } else {
                skipped += 1;
            }
        }
    }
    Scan { files, skipped }
}

/// Source files under a subtree the walk refused to descend into, so the
/// report can say how much it did not look at.
fn count_sources(dir: &Path, extensions: &[&str]) -> usize {
    let mut found = 0usize;
    let mut stack = vec![dir.to_path_buf()];
    // Bounded independently: this is an accounting pass, not a read.
    while let Some(dir) = stack.pop() {
        if found >= MAX_FILES {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for path in entries.filter_map(Result::ok).map(|entry| entry.path()) {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if path.is_dir() {
                if !name.starts_with('.') && !SKIP_DIRS.contains(&name) {
                    stack.push(path);
                }
                continue;
            }
            let extension = path.extension().and_then(|ext| ext.to_str()).unwrap_or("");
            if extensions.contains(&extension) {
                found += 1;
            }
        }
    }
    found
}

/// Normalize an extracted raw path to an OpenAPI path template. Framework
/// parameter styles (`:id`, `<id>`, `<int:id>`, `{id:regex}`) all become
/// `{id}`. Anything not confidently expressible (wildcards, regex fragments,
/// URLs) is rejected and counted as skipped by the caller.
pub(super) fn normalize_path(raw: &str) -> Option<String> {
    if raw.contains("://") || raw.chars().any(char::is_whitespace) {
        return None;
    }
    let raw = raw.strip_prefix('/').unwrap_or(raw);
    let mut segments = Vec::new();
    for segment in raw.split('/') {
        if segment.is_empty() {
            continue;
        }
        segments.push(normalize_segment(segment)?);
    }
    Some(format!("/{}", segments.join("/")))
}

fn is_identifier(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
        && !name.starts_with(|character: char| character.is_ascii_digit())
}

fn normalize_segment(segment: &str) -> Option<String> {
    let param = |name: &str| is_identifier(name).then(|| format!("{{{name}}}"));
    if let Some(name) = segment.strip_prefix(':') {
        return param(name);
    }
    if let Some(name) = segment
        .strip_prefix('<')
        .and_then(|rest| rest.strip_suffix('>'))
    {
        // Flask/Django converters: `<int:id>` -> the name after the colon.
        return param(name.split(':').next_back().unwrap_or(name));
    }
    if let Some(name) = segment
        .strip_prefix('{')
        .and_then(|rest| rest.strip_suffix('}'))
    {
        // chi-style `{id:[0-9]+}` -> the name before the colon.
        return param(name.split(':').next().unwrap_or(name));
    }
    let literal = segment.chars().all(|character| {
        character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.' | '~' | '%')
    });
    literal.then(|| segment.to_string())
}

/// The parameter names of a normalized path template, in order.
pub(super) fn path_params(path: &str) -> Vec<&str> {
    path.split('/')
        .filter_map(|segment| segment.strip_prefix('{')?.strip_suffix('}'))
        .collect()
}
