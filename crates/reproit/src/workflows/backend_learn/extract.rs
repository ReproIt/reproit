//! Static route derivation for `reproit init`.
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
const SKIP_DIRS: [&str; 19] = [
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
    "__tests__",
    "__mocks__",
    "e2e",
    "integration",
    "spec",
    "fixtures",
    "benches",
    "testdata",
];

/// Filename markers for a test source. A supertest call like
/// `request(app).get('/1/abc')` is a URL the test DRIVES, not one the service
/// serves, and reading them made 144 of 162 reported NestJS paths fictional.
const TEST_MARKERS: [&str; 6] = [
    ".spec.",
    ".test.",
    "_test.",
    "test_",
    ".e2e-spec.",
    ".integration.",
];

/// Whether this file is a test rather than served source.
fn is_test_source(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    TEST_MARKERS.iter().any(|marker| name.contains(marker))
}

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
    /// handler -> the response statuses and body shapes its code states.
    pub(super) responses: BTreeMap<String, super::response_facts::ResponseFact>,
    /// Serializer types the responses resolve named bodies against.
    pub(super) serializers: super::response_facts::Serializers,
}

impl Derived {
    pub(super) fn operation_count(&self) -> usize {
        self.routes.values().map(BTreeSet::len).sum()
    }
}

/// The framework families `init` can extract routes for, mapped from the
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
        "gin" | "echo" | "fiber" | "chi" | "gorilla/mux" | "net/http" => Family::Go,
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
                responses: parsed.responses,
                serializers: parsed.serializers,
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
        responses: parsed.responses,
        serializers: parsed.serializers,
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
            if is_test_source(&path) {
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
    if raw.contains("://") {
        return None;
    }
    // A Flask converter may carry arguments with spaces:
    // `<any(xhr, jquery, fetch):js>`. Those spaces are inside the parameter,
    // not in the path, and rejecting the whole route for them lost a real
    // endpoint. Whitespace anywhere ELSE still means this is not a path.
    let raw = &strip_parameter_spaces(raw);
    if raw.chars().any(char::is_whitespace) {
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

/// Remove whitespace that sits inside a `<...>` or `{...}` parameter.
fn strip_parameter_spaces(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut depth = 0usize;
    for character in raw.chars() {
        match character {
            '<' | '{' => depth += 1,
            '>' | '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
        if !(depth > 0 && character.is_whitespace()) {
            out.push(character);
        }
    }
    out
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
    // Catch-alls: gin `*any`, echo/chi `*`, rocket `<path..>`. The segment is a
    // real part of the surface, and dropping the whole route lost `/swagger/*any`
    // and every static-file mount. OpenAPI has no wildcard, so it becomes a
    // named template parameter, which is what a generator can exercise.
    if let Some(name) = segment.strip_prefix('*') {
        return Some(if is_identifier(name) {
            format!("{{{name}}}")
        } else {
            "{wildcard}".to_string()
        });
    }
    // Fiber permits a wildcard after a literal prefix in the same segment:
    // `/web*` matches `/webanything`, not `/web/anything`. Keep that shape in
    // the template rather than inventing a slash.
    if let Some((literal, name)) = segment.split_once('*') {
        let valid_literal = !literal.is_empty()
            && literal
                .chars()
                .all(|character| character.is_alphanumeric() || matches!(character, '-' | '_'));
        if valid_literal && !name.contains('*') {
            let name = if is_identifier(name) {
                name
            } else {
                "wildcard"
            };
            return Some(format!("{literal}{{{name}}}"));
        }
    }
    if let Some(name) = segment
        .strip_prefix('<')
        .and_then(|rest| rest.strip_suffix("..>"))
    {
        return param(name.split(':').next_back().unwrap_or(name));
    }
    if let Some(name) = segment.strip_prefix(':') {
        return param(name.strip_suffix('?').unwrap_or(name));
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
        // axum 0.8 moved catch-alls from `*rest` to `{*rest}`.
        if let Some(name) = name.strip_prefix('*') {
            return Some(if is_identifier(name) {
                format!("{{{name}}}")
            } else {
                "{wildcard}".to_string()
            });
        }
        // chi-style `{id:[0-9]+}` -> the name before the colon.
        let name = name.strip_suffix('?').unwrap_or(name);
        return param(name.split(':').next().unwrap_or(name));
    }
    // A literal segment may be non-ASCII: `#[get("/мир")]` is a real Rocket
    // route, and requiring ASCII dropped it silently. What must be rejected is
    // a character that is structural in a URL, not one that is merely foreign.
    // A regex fragment is not a path segment, and a URL-structural character
    // means this is not a literal either.
    const REJECT: [char; 15] = [
        '/', '?', '#', '[', ']', '{', '}', '*', '^', '$', '(', ')', '+', '|', '\\',
    ];
    let literal = !segment.is_empty() && !segment.chars().any(|c| REJECT.contains(&c));
    literal.then(|| segment.to_string())
}

/// The parameter names of a normalized path template, in order.
pub(super) fn path_params(path: &str) -> Vec<&str> {
    path.split('/')
        .filter_map(|segment| segment.strip_prefix('{')?.strip_suffix('}'))
        .collect()
}
