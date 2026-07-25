//! Cross-function mount resolution for Rust routers.
//!
//! Every other framework family mounts a prefix in the same statement that
//! names the router variable, so a file-scoped map resolves it. Rust does not:
//! the idiom is `.nest("/api/v1", users::routes())`, where the prefix lives at
//! the call site and the routes live in another function, usually another file.
//! Resolving only within a file meant an axum service reported every route at
//! its LOCAL path with no warning, which is worse than not extracting at all:
//! `/users` looks like a real route and 404s.
//!
//! So Rust extraction is two-pass. Pass one attributes each route and each
//! `.nest(...)` to the function that lexically contains it. Pass two walks the
//! resulting mount graph from its roots, composing prefixes. A function nobody
//! nests is a root and keeps its local paths, which is exactly the old
//! behaviour, so nothing that used to resolve stops resolving.

use regex::Regex;
use std::collections::{BTreeMap, BTreeSet};

/// Mount chains deeper than this are not composed. Real services nest two or
/// three levels; a deeper chain means the parse went wrong.
const MAX_NEST_DEPTH: usize = 8;
/// Bound the composed set so a pathological graph cannot blow up.
const MAX_RESOLVED_MOUNTS: usize = 4_096;

/// Routes and mounts of one file, attributed to their enclosing function.
#[derive(Debug, Default)]
pub(super) struct RustUnits {
    /// enclosing function -> routes declared directly in it.
    pub(super) routes: BTreeMap<String, Vec<(String, &'static str)>>,
    /// (enclosing function, prefix, mounted function).
    pub(super) mounts: Vec<(String, String, String)>,
}

/// Where a route hit sat when we found it. Anything outside a `fn` body (a
/// `const` router, a macro) is attributed to the empty name, which resolves as
/// a root.
const FILE_SCOPE: &str = "";

pub(super) struct RustScanner {
    function: Regex,
    nest: Regex,
}

impl RustScanner {
    pub(super) fn new(compile: impl Fn(&str) -> Regex) -> Self {
        Self {
            function: compile(r"\bfn\s+([A-Za-z_][A-Za-z0-9_]*)"),
            // `.nest("/p", users::routes())` and `.nest_service("/p", f())`.
            // The mounted expression's LAST path segment is the function name.
            nest: compile(
                r#"\.nest(?:_service)?\(\s*"([^"]*)"\s*,\s*(?:[A-Za-z_][A-Za-z0-9_]*::)*([A-Za-z_][A-Za-z0-9_]*)\s*\("#,
            ),
        }
    }

    /// Attribute each line's hits to the function whose body encloses it, by
    /// tracking brace depth. `route_hits` is the existing per-line extractor, so
    /// this adds attribution without changing what counts as a route.
    pub(super) fn scan(
        &self,
        content: &str,
        route_hits: impl Fn(&str) -> Vec<(String, &'static str)>,
    ) -> RustUnits {
        let mut units = RustUnits::default();
        // The function whose body we are inside, with the brace depth it opened
        // at. A nested block does not change attribution; leaving the body does.
        let mut current: Option<(String, i32)> = None;
        let mut pending: Option<String> = None;
        let mut depth: i32 = 0;
        for line in content.lines() {
            let code = strip_line_comment(line);
            // A signature can span lines, so remember the name until its `{`.
            if let Some(captures) = self.function.captures(code) {
                pending = Some(captures[1].to_string());
            }
            let opened = code.matches('{').count() as i32;
            let closed = code.matches('}').count() as i32;
            if opened > 0 {
                if let Some(name) = pending.take() {
                    if current.is_none() {
                        current = Some((name, depth));
                    }
                }
            }
            let scope = current
                .as_ref()
                .map(|(name, _)| name.as_str())
                .unwrap_or(FILE_SCOPE);
            for hit in route_hits(code) {
                units.routes.entry(scope.to_string()).or_default().push(hit);
            }
            for captures in self.nest.captures_iter(code) {
                units.mounts.push((
                    scope.to_string(),
                    captures[1].to_string(),
                    captures[2].to_string(),
                ));
            }
            depth += opened - closed;
            if let Some((_, opened_at)) = &current {
                if depth <= *opened_at {
                    current = None;
                }
            }
            if depth <= 0 {
                depth = 0;
                current = None;
            }
        }
        units
    }
}

/// Compose the mount graph into final paths.
///
/// A function reachable from a root through `.nest(prefix, f())` carries the
/// joined prefix. A function nobody mounts is a root and keeps its local paths,
/// so an unresolvable mount degrades to the previous behaviour rather than to a
/// fabricated path.
pub(super) fn resolve(
    units: &[RustUnits],
    join: impl Fn(Option<&String>, &str) -> String,
    normalize: impl Fn(&str) -> Option<String>,
) -> (BTreeMap<String, BTreeSet<&'static str>>, usize) {
    let mut routes: BTreeMap<String, Vec<(String, &'static str)>> = BTreeMap::new();
    let mut mounts: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    let mut mounted: BTreeSet<String> = BTreeSet::new();
    for unit in units {
        for (function, hits) in &unit.routes {
            routes
                .entry(function.clone())
                .or_default()
                .extend(hits.iter().cloned());
        }
        for (parent, prefix, child) in &unit.mounts {
            mounts
                .entry(parent.clone())
                .or_default()
                .push((prefix.clone(), child.clone()));
            mounted.insert(child.clone());
        }
    }

    // Roots: every function that declares routes or mounts and is not itself
    // mounted by someone. File scope is always a root.
    let mut queue: Vec<(String, String, usize)> = routes
        .keys()
        .chain(mounts.keys())
        .filter(|function| !mounted.contains(*function))
        .map(|function| (function.clone(), String::new(), 0usize))
        .collect();
    queue.sort();
    queue.dedup();

    let mut resolved: BTreeMap<String, BTreeSet<&'static str>> = BTreeMap::new();
    let mut skipped = 0usize;
    let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
    let mut emitted = 0usize;
    while let Some((function, prefix, depth)) = queue.pop() {
        if depth > MAX_NEST_DEPTH || emitted >= MAX_RESOLVED_MOUNTS {
            break;
        }
        // A cycle, or the same function mounted twice under one prefix.
        if !seen.insert((function.clone(), prefix.clone())) {
            continue;
        }
        for (path, method) in routes.get(&function).into_iter().flatten() {
            let full = if prefix.is_empty() {
                path.clone()
            } else {
                join(Some(&prefix), path)
            };
            match normalize(&full) {
                Some(path) => {
                    resolved.entry(path).or_default().insert(method);
                    emitted += 1;
                }
                None => skipped += 1,
            }
        }
        for (mount_prefix, child) in mounts.get(&function).into_iter().flatten() {
            let composed = if prefix.is_empty() {
                mount_prefix.clone()
            } else {
                join(Some(&prefix), mount_prefix)
            };
            queue.push((child.clone(), composed, depth + 1));
        }
    }
    (resolved, skipped)
}

/// Drop a trailing `//` comment so a commented-out route or brace does not move
/// attribution. Not string-aware, which is fine: a `//` inside a route literal
/// would already be a malformed path.
fn strip_line_comment(line: &str) -> &str {
    match line.find("//") {
        Some(at) => &line[..at],
        None => line,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scanner() -> RustScanner {
        RustScanner::new(|pattern| Regex::new(pattern).expect("valid pattern"))
    }

    /// Stand-in for the real per-line route extractor.
    fn hits(line: &str) -> Vec<(String, &'static str)> {
        let route = Regex::new(r#"\.route\(\s*"([^"]+)"\s*,\s*(get|post)\("#).expect("pattern");
        route
            .captures_iter(line)
            .map(|captures| {
                let method: &'static str = if &captures[2] == "get" { "get" } else { "post" };
                (captures[1].to_string(), method)
            })
            .collect()
    }

    fn join(prefix: Option<&String>, path: &str) -> String {
        match prefix {
            Some(prefix) => format!("{}{}", prefix.trim_end_matches('/'), path),
            None => path.to_string(),
        }
    }

    fn resolve_all(sources: &[&str]) -> BTreeMap<String, BTreeSet<&'static str>> {
        let scanner = scanner();
        let units: Vec<RustUnits> = sources
            .iter()
            .map(|source| scanner.scan(source, hits))
            .collect();
        resolve(&units, join, |path| Some(path.to_string())).0
    }

    #[test]
    fn a_nested_router_carries_its_mount_prefix() {
        // The reported case: hey's 35 routes all reported unprefixed.
        let routes = resolve_all(&[
            r#"
            fn app() -> Router {
                Router::new().nest("/api/v1", users::routes())
            }
            "#,
            r#"
            pub fn routes() -> Router {
                Router::new()
                    .route("/users", get(list))
                    .route("/users/{id}", get(show))
            }
            "#,
        ]);
        assert!(routes.contains_key("/api/v1/users"), "{routes:?}");
        assert!(routes.contains_key("/api/v1/users/{id}"), "{routes:?}");
        assert!(
            !routes.contains_key("/users"),
            "the unprefixed local path must not survive: {routes:?}"
        );
    }

    #[test]
    fn mount_chains_compose() {
        let routes = resolve_all(&[
            r#"fn app() -> Router { Router::new().nest("/api", v1()) }"#,
            r#"fn v1() -> Router { Router::new().nest("/v1", users()) }"#,
            r#"fn users() -> Router { Router::new().route("/users", get(list)) }"#,
        ]);
        assert!(routes.contains_key("/api/v1/users"), "{routes:?}");
    }

    #[test]
    fn an_unmounted_function_keeps_its_local_path() {
        // Degrading to the previous behaviour is the point: never fabricate.
        let routes =
            resolve_all(&[r#"fn app() -> Router { Router::new().route("/health", get(h)) }"#]);
        assert!(routes.contains_key("/health"), "{routes:?}");
    }

    #[test]
    fn routes_on_the_root_router_stay_unprefixed_alongside_a_nest() {
        let routes = resolve_all(&[
            r#"
            fn app() -> Router {
                Router::new()
                    .route("/healthz", get(health))
                    .nest("/api", inner())
            }
            "#,
            r#"fn inner() -> Router { Router::new().route("/posts", post(create)) }"#,
        ]);
        assert!(routes.contains_key("/healthz"), "{routes:?}");
        assert!(routes.contains_key("/api/posts"), "{routes:?}");
    }

    #[test]
    fn a_mount_cycle_terminates() {
        let routes = resolve_all(&[
            r#"fn a() -> Router { Router::new().nest("/a", b()) }"#,
            r#"fn b() -> Router { Router::new().nest("/b", a()).route("/x", get(h)) }"#,
        ]);
        // Whatever it resolves to, it must terminate and stay bounded.
        assert!(routes.len() < MAX_RESOLVED_MOUNTS, "{routes:?}");
    }

    #[test]
    fn a_commented_out_route_does_not_move_attribution() {
        let routes = resolve_all(&[
            r#"fn app() -> Router { Router::new().nest("/api", inner()) }"#,
            r#"
            fn inner() -> Router {
                // .route("/old", get(gone))
                Router::new().route("/new", get(h))
            }
            "#,
        ]);
        assert!(routes.contains_key("/api/new"), "{routes:?}");
        assert!(!routes.contains_key("/api/old"), "{routes:?}");
    }

    #[test]
    fn nest_service_is_followed_too() {
        let routes = resolve_all(&[
            r#"fn app() -> Router { Router::new().nest_service("/static", assets()) }"#,
            r#"fn assets() -> Router { Router::new().route("/logo", get(h)) }"#,
        ]);
        assert!(routes.contains_key("/static/logo"), "{routes:?}");
    }
}
