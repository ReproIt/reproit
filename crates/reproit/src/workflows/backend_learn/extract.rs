//! Static route derivation for `reproit init --learn`: line-level pattern
//! extraction per framework family over a bounded set of source files. This is
//! deliberately not a parser; anything a pattern cannot claim confidently is
//! skipped and counted rather than guessed.

use super::python_ast;
use super::rust_ast;

/// One extracted route: local path, method, and the handler that serves it.
pub(super) type RouteHit = (String, &'static str, Option<String>);
use regex::Regex;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// HTTP methods a derived draft may claim, in emission order.
pub(super) const METHODS: [&str; 7] = ["get", "post", "put", "patch", "delete", "head", "options"];

const MAX_FILES: usize = 400;
const MAX_FILE_BYTES: u64 = 256 * 1024;
const MAX_WALK_DEPTH: usize = 8;
/// Lines joined when a route definition spans an object literal (fastify).
const ROUTE_OBJECT_WINDOW: usize = 8;

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
}

pub(super) fn family_for(framework: &str) -> Option<Family> {
    Some(match framework {
        "axum" | "actix-web" | "rocket" | "warp" => Family::Rust,
        "express" | "fastify" | "koa" | "hapi" => Family::Node,
        "fastapi" | "flask" | "django" => Family::Python,
        "gin" | "echo" | "fiber" | "chi" | "net/http" => Family::Go,
        "rails" | "sinatra" => Family::Ruby,
        "spring" | "java" => Family::Spring,
        "laravel" => Family::Php,
        _ => return None,
    })
}

fn extensions(family: Family) -> &'static [&'static str] {
    match family {
        Family::Rust => &["rs"],
        Family::Node => &["js", "mjs", "cjs", "ts"],
        Family::Python => &["py"],
        Family::Go => &["go"],
        Family::Ruby => &["rb"],
        Family::Spring => &["java", "kt"],
        Family::Php => &["php"],
    }
}

/// Derive routes for a detected framework from the project's source files.
pub(super) fn derive(root: &Path, framework: &str) -> Option<Derived> {
    let family = family_for(framework)?;
    let patterns = Patterns::new();
    let files = source_files(root, extensions(family));
    if family == Family::Python {
        // Python reads through its grammar: the decorator, the handler it
        // decorates and the annotated model are one structure, so a wrapped
        // decorator or a comment between them stops mattering.
        let parsed = python_ast::read(root);
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
        return Some(derived);
    }
    if family == Family::Rust {
        // Rust reads through a real parser: an unreadable file is COUNTED
        // rather than looking like an empty one.
        let parsed = rust_ast::read(root);
        return Some(Derived {
            routes: parsed.routes,
            handlers: parsed.handlers,
            files_scanned: parsed.files_parsed,
            skipped: parsed.files_unparsed,
            unreadable: parsed.files_unparsed,
            bodies: parsed.bodies,
        });
    }
    // Pattern extraction still does the reading, but a grammar now says which
    // files it could make sense of: an absence over an unreadable file is not
    // evidence, and only a parse can tell the difference.
    let mut derived = Derived {
        unreadable: super::parsed_source::check(root, family).files_unreadable,
        ..Derived::default()
    };
    for file in files {
        let Ok(content) = std::fs::read_to_string(&file) else {
            continue;
        };
        derived.files_scanned += 1;
        let prefixes = patterns.prefixes(family, &content);
        let hits = match family {
            Family::Node => patterns.node(&content, &prefixes),
            Family::Python => unreachable!("handled by the grammar"),
            Family::Go => patterns.go(&content),
            Family::Ruby => patterns.ruby(&content),
            Family::Spring => patterns.spring(&content),
            Family::Php => patterns.php(&content),
            Family::Rust => unreachable!("handled by the parser"),
        };
        for (raw, method, handler) in hits {
            match normalize_path(&raw) {
                Some(path) => {
                    derived
                        .routes
                        .entry(path.clone())
                        .or_default()
                        .insert(method);
                    // Handlers are recorded for every family that can name one,
                    // so reading a family's types is a matter of teaching its
                    // signatures, not of plumbing.
                    if let Some(handler) = handler {
                        derived
                            .handlers
                            .insert((method.to_uppercase(), path), handler);
                    }
                }
                None => derived.skipped += 1,
            }
        }
    }
    Some(derived)
}

/// The sources of one family, so a type reader sees exactly the file set the
/// route reader saw.
pub(super) fn family_sources(root: &Path, family: Family) -> Vec<PathBuf> {
    source_files(root, extensions(family))
}

/// Bounded, deterministic source walk: sorted entries, capped depth and count,
/// skip directories that never hold first-party routes.
fn source_files(root: &Path, extensions: &[&str]) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        if depth > MAX_WALK_DEPTH || files.len() >= MAX_FILES {
            break;
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
            let small = std::fs::metadata(&path).is_ok_and(|meta| meta.len() <= MAX_FILE_BYTES);
            if extensions.contains(&extension) && small && files.len() < MAX_FILES {
                files.push(path);
            }
        }
    }
    files
}

fn method_const(method: &str) -> Option<&'static str> {
    let lower = method.to_ascii_lowercase();
    METHODS.into_iter().find(|known| **known == lower)
}

struct Patterns {
    node_call: Regex,
    node_route_url: Regex,
    node_route_method: Regex,
    node_handler: Regex,
    spring_method: Regex,
    php_action: Regex,
    flask_blueprint: Regex,
    fastapi_router: Regex,
    fastapi_include: Regex,
    node_router_mount: Regex,
    go_call: Regex,
    go_handle_func: Regex,
    ruby_verb: Regex,
    ruby_action: Regex,
    ruby_resources: Regex,
    spring_mapping: Regex,
    spring_bare_mapping: Regex,
    spring_prefix: Regex,
    php_route: Regex,
}

impl Patterns {
    fn new() -> Self {
        let compile = |pattern: &str| Regex::new(pattern).expect("static route pattern");
        Self {
            node_call: compile(
                r#"([\w$)\]]+)\.(get|post|put|patch|delete|head|options|all)\(\s*['"`]([^'"`]+)['"`]"#,
            ),
            node_route_url: compile(r#"\b(?:url|path)\s*:\s*['"`]([^'"`]+)['"`]"#),
            node_route_method: compile(r#"\bmethod\s*:\s*\[?\s*['"`]([A-Za-z]+)['"`]"#),
            node_handler: compile(r"[,(]\s*(?:[A-Za-z_$][\w$]*\s*\(\s*)?([A-Za-z_$][\w$]*)\s*[,)]"),
            spring_method: compile(r"\b(?:public|protected)\s+\S+\s+([A-Za-z_]\w*)\s*\("),
            php_action: compile(r#"[,\[]\s*['"]?([A-Za-z_]\w*)(?:::class)?"#),
            node_router_mount: compile(r#"\.use\(\s*['"`](/[^'"`]*)['"`]\s*,\s*([\w$]+)"#),
            flask_blueprint: compile(
                r#"(\w+)\s*=\s*Blueprint\([^)]*\burl_prefix\s*=\s*['"]([^'"]+)['"]"#,
            ),
            fastapi_router: compile(
                r#"(\w+)\s*=\s*APIRouter\([^)]*\bprefix\s*=\s*['"]([^'"]+)['"]"#,
            ),
            fastapi_include: compile(
                r#"include_router\(\s*(\w+)[^)]*\bprefix\s*=\s*['"]([^'"]+)['"]"#,
            ),
            go_call: compile(
                r#"\w\.(?i:(get|post|put|patch|delete|head|options))\(\s*"([^"]+)"\s*,\s*(?:[A-Za-z_]\w*\.)*([A-Za-z_]\w*)"#,
            ),
            go_handle_func: compile(
                r#"HandleFunc\(\s*"(GET|POST|PUT|PATCH|DELETE|HEAD|OPTIONS) ([^"]+)""#,
            ),
            ruby_verb: compile(r#"(?m)^\s*(get|post|put|patch|delete)\s+['"]([^'"]+)['"]([^\n]*)"#),
            ruby_action: compile(r##"to:\s*['"][^'"#]*#([a-z_]\w*)['"]"##),
            ruby_resources: compile(r"(?m)^\s*resources\s+:([a-z_]+)"),
            spring_mapping: compile(
                r#"@(Get|Post|Put|Patch|Delete)Mapping\(\s*(?:(?:value|path)\s*=\s*)?"([^"]+)""#,
            ),
            spring_bare_mapping: compile(
                r"(?m)^\s*@(Get|Post|Put|Patch|Delete)Mapping\s*(?:\(\s*\))?\s*$",
            ),
            spring_prefix: compile(r#"@RequestMapping\(\s*(?:(?:value|path)\s*=\s*)?"([^"]+)""#),
            php_route: compile(
                r#"Route::(get|post|put|patch|delete|any)\(\s*['"]([^'"]+)['"]([^\n]*)"#,
            ),
        }
    }

    /// File-scoped mount prefixes by router/blueprint variable, so routes defined
    /// on a nested router carry their real path. Resolved only where the prefix
    /// travels with the variable in the same file (Flask `Blueprint(url_prefix=)`,
    /// FastAPI `APIRouter(prefix=)` plus `include_router(prefix=)`, Express
    /// `app.use("/prefix", router)`). A prefix mounted via a function return
    /// (e.g. axum `.nest("/api", routes())`) is not followed: guessing would emit
    /// wrong paths, so those routes keep their local path rather than a fabricated
    /// one.
    fn prefixes(&self, family: Family, content: &str) -> BTreeMap<String, String> {
        let mut map = BTreeMap::new();
        match family {
            Family::Python => {
                for captures in self.flask_blueprint.captures_iter(content) {
                    map.insert(captures[1].to_string(), captures[2].to_string());
                }
                for captures in self.fastapi_router.captures_iter(content) {
                    map.insert(captures[1].to_string(), captures[2].to_string());
                }
                // include_router(prefix=) mounts a router under an ADDITIONAL
                // prefix, composed outside any prefix the router already carries.
                for captures in self.fastapi_include.captures_iter(content) {
                    let var = captures[1].to_string();
                    let base = map.get(&var).cloned().unwrap_or_default();
                    map.insert(var, join_prefix(Some(&captures[2].to_string()), &base));
                }
            }
            Family::Node => {
                for captures in self.node_router_mount.captures_iter(content) {
                    map.insert(captures[2].to_string(), captures[1].to_string());
                }
            }
            _ => {}
        }
        map
    }

    /// express/koa/hapi-style `app.get('/x', ...)` plus the fastify
    /// `fastify.route({ method: 'GET', url: '/x' })` object form (the object
    /// is matched across a small line window).
    fn node(&self, content: &str, prefixes: &BTreeMap<String, String>) -> Vec<RouteHit> {
        let mut hits = Vec::new();
        let lines: Vec<&str> = content.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            for captures in self.node_call.captures_iter(line) {
                let raw = &captures[3];
                // `all` claims every method; the draft claims only GET.
                let method = if &captures[2] == "all" {
                    "get"
                } else {
                    match method_const(&captures[2]) {
                        Some(method) => method,
                        None => continue,
                    }
                };
                if raw.starts_with('/') && !raw.contains("://") {
                    // The rest of the registration names either the handler or
                    // the validation schema it is wrapped in; both are keys the
                    // type reader can resolve.
                    let handler = self
                        .node_handler
                        .captures(&line[captures.get(0).map_or(0, |m| m.end())..])
                        .map(|found| found[1].to_string());
                    hits.push((
                        join_prefix(prefixes.get(&captures[1]), raw),
                        method,
                        handler,
                    ));
                }
            }
            if line.contains(".route(") {
                let end = (index + ROUTE_OBJECT_WINDOW).min(lines.len());
                let window = lines[index..end].join(" ");
                if let (Some(url), Some(method)) = (
                    self.node_route_url.captures(&window),
                    self.node_route_method.captures(&window),
                ) {
                    if let Some(method) = method_const(&method[1]) {
                        hits.push((url[1].to_string(), method, None));
                    }
                }
            }
        }
        hits
    }

    /// gin/echo/fiber `r.GET("/x", ...)`, chi `r.Get("/x", ...)`, and Go 1.22
    /// net/http `mux.HandleFunc("GET /x", ...)` method-prefixed patterns.
    fn go(&self, content: &str) -> Vec<RouteHit> {
        let mut hits = Vec::new();
        for line in content.lines() {
            for captures in self.go_call.captures_iter(line) {
                if let Some(method) = method_const(&captures[1]) {
                    if captures[2].starts_with('/') {
                        let handler = captures.get(3).map(|value| value.as_str().to_string());
                        hits.push((captures[2].to_string(), method, handler));
                    }
                }
            }
            if let Some(captures) = self.go_handle_func.captures(line) {
                if let Some(method) = method_const(&captures[1]) {
                    hits.push((captures[2].to_string(), method, None));
                }
            }
        }
        hits
    }

    /// Rails routes.rb verbs and `resources :name` (expanded to the standard
    /// five routes), plus Sinatra's identical top-level verb blocks.
    fn ruby(&self, content: &str) -> Vec<RouteHit> {
        let mut hits = Vec::new();
        for captures in self.ruby_verb.captures_iter(content) {
            if let Some(method) = method_const(&captures[1]) {
                // `to: 'blocks#create'` names the action that handles it.
                let handler = captures.get(3).and_then(|rest| {
                    self.ruby_action
                        .captures(rest.as_str())
                        .map(|found| found[1].to_string())
                });
                hits.push((captures[2].to_string(), method, handler));
            }
        }
        for captures in self.ruby_resources.captures_iter(content) {
            let name = &captures[1];
            for (suffix, method) in [
                ("", "get"),
                ("", "post"),
                ("/{id}", "get"),
                ("/{id}", "patch"),
                ("/{id}", "delete"),
            ] {
                hits.push((format!("/{name}{suffix}"), method, None));
            }
        }
        hits
    }

    /// Spring `@GetMapping("/x")` (and friends), with a class-level
    /// `@RequestMapping` prefix applied when one precedes the class keyword.
    /// Bare `@GetMapping` maps to the prefix itself.
    fn spring(&self, content: &str) -> Vec<RouteHit> {
        let lines: Vec<&str> = content.lines().collect();
        let class_line = content
            .lines()
            .position(|line| line.contains("class "))
            .unwrap_or(usize::MAX);
        let prefix = content
            .lines()
            .take(class_line)
            .find_map(|line| self.spring_prefix.captures(line))
            .map(|captures| captures[1].to_string())
            .unwrap_or_default();
        let mut hits = Vec::new();
        // The annotated method is the handler; it follows the mapping within a
        // few lines, past any further annotations.
        let handler_after = |index: usize| {
            lines
                .iter()
                .skip(index + 1)
                .take(6)
                .find_map(|next| self.spring_method.captures(next))
                .map(|captures| captures[1].to_string())
        };
        for (index, line) in lines.iter().enumerate() {
            if let Some(captures) = self.spring_mapping.captures(line) {
                if let Some(method) = method_const(&captures[1]) {
                    hits.push((
                        format!("{prefix}{}", &captures[2]),
                        method,
                        handler_after(index),
                    ));
                }
            } else if let Some(captures) = self.spring_bare_mapping.captures(line) {
                if let Some(method) = method_const(&captures[1]) {
                    let path = if prefix.is_empty() { "/" } else { &prefix };
                    hits.push((path.to_string(), method, handler_after(index)));
                }
            }
        }
        hits
    }

    /// Laravel `Route::get('/x', ...)` in routes/*.php (`any` claims only GET).
    fn php(&self, content: &str) -> Vec<RouteHit> {
        let mut hits = Vec::new();
        for captures in self.php_route.captures_iter(content) {
            let method = if &captures[1] == "any" {
                "get"
            } else {
                match method_const(&captures[1]) {
                    Some(method) => method,
                    None => continue,
                }
            };
            // `Route::post('/x', [StoreBlockController::class, 'store'])` and
            // the string form both name the class that validates the body.
            let handler = captures.get(3).and_then(|rest| {
                self.php_action
                    .captures(rest.as_str())
                    .map(|found| found[1].to_string())
            });
            hits.push((captures[2].to_string(), method, handler));
        }
        hits
    }
}

/// Prefix a route path with its router/blueprint mount, if any. A leading slash
/// on the path is preserved; an empty or root path yields the prefix itself, so
/// `Blueprint(url_prefix="/api")` + `@bp.route("")` is `/api`, not `/api/`.
fn join_prefix(prefix: Option<&String>, path: &str) -> String {
    let Some(prefix) = prefix.filter(|value| !value.is_empty()) else {
        return path.to_string();
    };
    let base = prefix.trim_end_matches('/');
    if path.is_empty() || path == "/" {
        return base.to_string();
    }
    if path.starts_with('/') {
        format!("{base}{path}")
    } else {
        format!("{base}/{path}")
    }
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
