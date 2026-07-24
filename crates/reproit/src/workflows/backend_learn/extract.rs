//! Static route derivation for `reproit init --learn`: line-level pattern
//! extraction per framework family over a bounded set of source files. This is
//! deliberately not a parser; anything a pattern cannot claim confidently is
//! skipped and counted rather than guessed.

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
const SKIP_DIRS: [&str; 8] = [
    "node_modules",
    "target",
    "vendor",
    "dist",
    "build",
    "__pycache__",
    "venv",
    "migrations",
];

#[derive(Debug, Default)]
pub(super) struct Derived {
    /// path -> methods, both normalized (`{id}` params, lowercase methods).
    pub(super) routes: BTreeMap<String, BTreeSet<&'static str>>,
    pub(super) files_scanned: usize,
    /// Pattern hits dropped because the path could not be normalized.
    pub(super) skipped: usize,
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
    let mut derived = Derived::default();
    for file in source_files(root, extensions(family)) {
        let Ok(content) = std::fs::read_to_string(&file) else {
            continue;
        };
        derived.files_scanned += 1;
        let file_name = file
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        let hits = match family {
            Family::Rust => patterns.rust(&content),
            Family::Node => patterns.node(&content),
            Family::Python => patterns.python(&content, file_name),
            Family::Go => patterns.go(&content),
            Family::Ruby => patterns.ruby(&content),
            Family::Spring => patterns.spring(&content),
            Family::Php => patterns.php(&content),
        };
        for (raw, method) in hits {
            match normalize_path(&raw) {
                Some(path) => {
                    derived.routes.entry(path).or_default().insert(method);
                }
                None => derived.skipped += 1,
            }
        }
    }
    Some(derived)
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
    rust_route: Regex,
    rust_method_call: Regex,
    rust_attribute: Regex,
    node_call: Regex,
    node_route_url: Regex,
    node_route_method: Regex,
    python_decorator: Regex,
    flask_route: Regex,
    flask_methods: Regex,
    django_path: Regex,
    go_call: Regex,
    go_handle_func: Regex,
    ruby_verb: Regex,
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
            rust_route: compile(r#"\.route\(\s*"([^"]+)"\s*,(.*)"#),
            rust_method_call: compile(r"\b(get|post|put|patch|delete|head|options)\s*\("),
            rust_attribute: compile(
                r##"#\[(?:\w+::)?(get|post|put|patch|delete|head|options)\(\s*"([^"]+)""##,
            ),
            node_call: compile(
                r#"[\w$\])]\.(get|post|put|patch|delete|head|options|all)\(\s*['"`]([^'"`]+)['"`]"#,
            ),
            node_route_url: compile(r#"\b(?:url|path)\s*:\s*['"`]([^'"`]+)['"`]"#),
            node_route_method: compile(r#"\bmethod\s*:\s*\[?\s*['"`]([A-Za-z]+)['"`]"#),
            python_decorator: compile(
                r#"@[\w.]+\.(get|post|put|patch|delete|head|options)\(\s*[rf]?['"]([^'"]+)['"]"#,
            ),
            flask_route: compile(r#"@[\w.]+\.route\(\s*[rf]?['"]([^'"]+)['"](.*)"#),
            flask_methods: compile(r"methods\s*=\s*\[([^\]]*)\]"),
            django_path: compile(r#"\bpath\(\s*[rf]?['"]([^'"]*)['"]"#),
            go_call: compile(r#"\w\.(?i:(get|post|put|patch|delete|head|options))\(\s*"([^"]+)""#),
            go_handle_func: compile(
                r#"HandleFunc\(\s*"(GET|POST|PUT|PATCH|DELETE|HEAD|OPTIONS) ([^"]+)""#,
            ),
            ruby_verb: compile(r#"(?m)^\s*(get|post|put|patch|delete)\s+['"]([^'"]+)['"]"#),
            ruby_resources: compile(r"(?m)^\s*resources\s+:([a-z_]+)"),
            spring_mapping: compile(
                r#"@(Get|Post|Put|Patch|Delete)Mapping\(\s*(?:(?:value|path)\s*=\s*)?"([^"]+)""#,
            ),
            spring_bare_mapping: compile(
                r"(?m)^\s*@(Get|Post|Put|Patch|Delete)Mapping\s*(?:\(\s*\))?\s*$",
            ),
            spring_prefix: compile(r#"@RequestMapping\(\s*(?:(?:value|path)\s*=\s*)?"([^"]+)""#),
            php_route: compile(r#"Route::(get|post|put|patch|delete|any)\(\s*['"]([^'"]+)['"]"#),
        }
    }

    /// axum `.route("/x", get(a).post(b))`, actix `.route("/x", web::get())`,
    /// and actix/rocket attribute routes `#[get("/x")]`.
    fn rust(&self, content: &str) -> Vec<(String, &'static str)> {
        let mut hits = Vec::new();
        for line in content.lines() {
            if let Some(captures) = self.rust_route.captures(line) {
                let path = captures[1].to_string();
                for method in self.rust_method_call.captures_iter(&captures[2]) {
                    if let Some(method) = method_const(&method[1]) {
                        hits.push((path.clone(), method));
                    }
                }
            }
            if let Some(captures) = self.rust_attribute.captures(line) {
                if let Some(method) = method_const(&captures[1]) {
                    // Rocket paths may carry a `?<query>` suffix; the path
                    // part before it is the route.
                    let path = captures[2].split('?').next().unwrap_or("").to_string();
                    hits.push((path, method));
                }
            }
        }
        hits
    }

    /// express/koa/hapi-style `app.get('/x', ...)` plus the fastify
    /// `fastify.route({ method: 'GET', url: '/x' })` object form (the object
    /// is matched across a small line window).
    fn node(&self, content: &str) -> Vec<(String, &'static str)> {
        let mut hits = Vec::new();
        let lines: Vec<&str> = content.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            for captures in self.node_call.captures_iter(line) {
                let path = captures[2].to_string();
                // `all` claims every method; the draft claims only GET.
                let method = if &captures[1] == "all" {
                    "get"
                } else {
                    match method_const(&captures[1]) {
                        Some(method) => method,
                        None => continue,
                    }
                };
                if path.starts_with('/') && !path.contains("://") {
                    hits.push((path, method));
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
                        hits.push((url[1].to_string(), method));
                    }
                }
            }
        }
        hits
    }

    /// FastAPI `@app.get("/x")`, Flask `@app.route("/x", methods=[...])`, and
    /// Django `path("x/", ...)` entries (urls.py only; methods are not
    /// declared there, so the draft claims only GET).
    fn python(&self, content: &str, file_name: &str) -> Vec<(String, &'static str)> {
        let mut hits = Vec::new();
        for line in content.lines() {
            if let Some(captures) = self.python_decorator.captures(line) {
                if let Some(method) = method_const(&captures[1]) {
                    hits.push((captures[2].to_string(), method));
                }
            }
            if let Some(captures) = self.flask_route.captures(line) {
                let path = captures[1].to_string();
                let methods = self
                    .flask_methods
                    .captures(&captures[2])
                    .map(|list| {
                        list[1]
                            .split(',')
                            .filter_map(|item| method_const(item.trim().trim_matches(['\'', '"'])))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_else(|| vec!["get"]);
                for method in methods {
                    hits.push((path.clone(), method));
                }
            }
            if file_name == "urls.py" {
                if let Some(captures) = self.django_path.captures(line) {
                    hits.push((format!("/{}", &captures[1]), "get"));
                }
            }
        }
        hits
    }

    /// gin/echo/fiber `r.GET("/x", ...)`, chi `r.Get("/x", ...)`, and Go 1.22
    /// net/http `mux.HandleFunc("GET /x", ...)` method-prefixed patterns.
    fn go(&self, content: &str) -> Vec<(String, &'static str)> {
        let mut hits = Vec::new();
        for line in content.lines() {
            for captures in self.go_call.captures_iter(line) {
                if let Some(method) = method_const(&captures[1]) {
                    if captures[2].starts_with('/') {
                        hits.push((captures[2].to_string(), method));
                    }
                }
            }
            if let Some(captures) = self.go_handle_func.captures(line) {
                if let Some(method) = method_const(&captures[1]) {
                    hits.push((captures[2].to_string(), method));
                }
            }
        }
        hits
    }

    /// Rails routes.rb verbs and `resources :name` (expanded to the standard
    /// five routes), plus Sinatra's identical top-level verb blocks.
    fn ruby(&self, content: &str) -> Vec<(String, &'static str)> {
        let mut hits = Vec::new();
        for captures in self.ruby_verb.captures_iter(content) {
            if let Some(method) = method_const(&captures[1]) {
                hits.push((captures[2].to_string(), method));
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
                hits.push((format!("/{name}{suffix}"), method));
            }
        }
        hits
    }

    /// Spring `@GetMapping("/x")` (and friends), with a class-level
    /// `@RequestMapping` prefix applied when one precedes the class keyword.
    /// Bare `@GetMapping` maps to the prefix itself.
    fn spring(&self, content: &str) -> Vec<(String, &'static str)> {
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
        for line in content.lines() {
            if let Some(captures) = self.spring_mapping.captures(line) {
                if let Some(method) = method_const(&captures[1]) {
                    hits.push((format!("{prefix}{}", &captures[2]), method));
                }
            } else if let Some(captures) = self.spring_bare_mapping.captures(line) {
                if let Some(method) = method_const(&captures[1]) {
                    let path = if prefix.is_empty() { "/" } else { &prefix };
                    hits.push((path.to_string(), method));
                }
            }
        }
        hits
    }

    /// Laravel `Route::get('/x', ...)` in routes/*.php (`any` claims only GET).
    fn php(&self, content: &str) -> Vec<(String, &'static str)> {
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
            hits.push((captures[2].to_string(), method));
        }
        hits
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
