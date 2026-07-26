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

/// One extracted route: its local path, method, and the handler that serves it.
/// The handler is what lets a resolved path be mapped back to the code, which is
/// how declared TYPES get checked and not just declared paths.
pub(super) type RouteHit = (String, &'static str, Option<String>);

/// Routes and mounts of one file, attributed to their enclosing function.
#[derive(Debug, Default)]
pub(super) struct RustUnits {
    /// enclosing function -> routes declared directly in it.
    pub(super) routes: BTreeMap<String, Vec<RouteHit>>,
    /// (enclosing function, prefix, mounted function).
    pub(super) mounts: Vec<(String, String, String)>,
    /// (enclosing function, prefix, mounted local variable).
    pub(super) variable_mounts: Vec<(String, String, String)>,
    /// (enclosing function, local variable, functions called to build it).
    pub(super) bindings: Vec<(String, String, Vec<String>)>,
}

/// Where a route hit sat when we found it. Anything outside a `fn` body (a
/// `const` router, a macro) is attributed to the empty name, which resolves as
/// a root.
const FILE_SCOPE: &str = "";

pub(super) struct RustScanner {
    function: Regex,
    nest: Regex,
    nest_variable: Regex,
    binding: Regex,
    call: Regex,
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
            // `.nest("/v1", v1)` where the router was built into a local first.
            // Real services do this whenever the router is conditional, e.g.
            // `let v1 = match cfg.plane { A => a::routes(), B => b::routes() };`
            // and matching only a direct call left every one of those routes at
            // its local path.
            nest_variable: compile(
                r#"\.nest(?:_service)?\(\s*"([^"]*)"\s*,\s*([A-Za-z_][A-Za-z0-9_]*)\s*[,)]"#,
            ),
            binding: compile(r"\blet\s+(?:mut\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*(?::[^=]*)?="),
            call: compile(r"(?:[A-Za-z_][A-Za-z0-9_]*::)*([A-Za-z_][A-Za-z0-9_]*)\s*\("),
        }
    }

    /// Attribute each line's hits to the function whose body encloses it, by
    /// tracking brace depth. `route_hits` is the existing per-line extractor, so
    /// this adds attribution without changing what counts as a route.
    pub(super) fn scan(
        &self,
        content: &str,
        route_hits: impl Fn(&str) -> Vec<RouteHit>,
    ) -> RustUnits {
        let mut units = RustUnits::default();
        // The function whose body we are inside, with the brace depth it opened
        // at. A nested block does not change attribution; leaving the body does.
        let mut current: Option<(String, i32)> = None;
        let mut pending: Option<String> = None;
        let mut binding_scope: Option<(String, String, Vec<String>)> = None;
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
            if let Some(captures) = self.binding.captures(code) {
                let name = captures[1].to_string();
                let rhs = &code[captures.get(0).map(|m| m.end()).unwrap_or(0)..];
                let called: Vec<String> = self
                    .call
                    .captures_iter(rhs)
                    .map(|call| call[1].to_string())
                    .collect();
                if !called.is_empty() {
                    binding_scope = Some((scope.to_string(), name, called));
                } else {
                    // The binding opens a block (a `match`, an `if`): keep
                    // collecting calls from the following lines until it closes.
                    binding_scope = Some((scope.to_string(), name, Vec::new()));
                }
            } else if let Some((owner, name, called)) = binding_scope.as_mut() {
                let _ = (&owner, &name);
                called.extend(
                    self.call
                        .captures_iter(code)
                        .map(|call| call[1].to_string()),
                );
            }
            // Routes inside a binding's initializer belong to the LOCAL being
            // built, not to the enclosing function. `let v1 = match plane {
            // A => Router::new().route(..), B => .. };` has no callee to
            // resolve to: the arms are the router. Attributing them to a
            // synthetic unit named for the local lets `.nest("/v1", v1)` mount
            // them like any other child.
            let owner = match &binding_scope {
                Some((binding_owner, name, _)) if binding_owner == scope => local_unit(scope, name),
                _ => scope.to_string(),
            };
            for hit in route_hits(code) {
                units.routes.entry(owner.clone()).or_default().push(hit);
            }
            let mut nested_here = Vec::new();
            for captures in self.nest.captures_iter(code) {
                nested_here.push(captures[1].to_string());
                units.mounts.push((
                    scope.to_string(),
                    captures[1].to_string(),
                    captures[2].to_string(),
                ));
            }
            for captures in self.nest_variable.captures_iter(code) {
                // A direct call already matched above; do not record it twice.
                if nested_here.contains(&captures[1].to_string()) {
                    continue;
                }
                units.variable_mounts.push((
                    scope.to_string(),
                    captures[1].to_string(),
                    captures[2].to_string(),
                ));
            }
            if code.contains(';') {
                if let Some(binding) = binding_scope.take() {
                    if !binding.2.is_empty() {
                        units.bindings.push(binding);
                    }
                }
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
) -> Resolved {
    let mut routes: BTreeMap<String, Vec<RouteHit>> = BTreeMap::new();
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
    // Resolve `.nest("/v1", v1)` through the local that built the router. A
    // binding is function-scoped, so only bindings made in the SAME function as
    // the mount are considered: two functions may each have their own `v1`.
    let mut bindings: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    for unit in units {
        for (owner, name, called) in &unit.bindings {
            bindings
                .entry((owner.clone(), name.clone()))
                .or_default()
                .extend(called.iter().cloned());
        }
    }
    for unit in units {
        for (parent, prefix, variable) in &unit.variable_mounts {
            // The local may itself hold routes (inline match arms), and may also
            // call builders. Both are children of this mount.
            let local = local_unit(parent, variable);
            if routes.contains_key(&local) || mounts.contains_key(&local) {
                mounts
                    .entry(parent.clone())
                    .or_default()
                    .push((prefix.clone(), local.clone()));
                mounted.insert(local);
            }
            let Some(called) = bindings.get(&(parent.clone(), variable.clone())) else {
                continue;
            };
            for child in called {
                // Only functions that actually declare routes or mount others:
                // a binding's right-hand side also calls constructors and
                // helpers, and mounting those would invent paths.
                if !routes.contains_key(child) && !mounts.contains_key(child) {
                    continue;
                }
                mounts
                    .entry(parent.clone())
                    .or_default()
                    .push((prefix.clone(), child.clone()));
                mounted.insert(child.clone());
            }
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
    let mut handlers: BTreeMap<(String, String), String> = BTreeMap::new();
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
        for (path, method, handler) in routes.get(&function).into_iter().flatten() {
            let full = if prefix.is_empty() {
                path.clone()
            } else {
                join(Some(&prefix), path)
            };
            match normalize(&full) {
                Some(path) => {
                    resolved.entry(path.clone()).or_default().insert(method);
                    if let Some(handler) = handler {
                        // Uppercase to match the Route convention every consumer uses.
                        handlers.insert((method.to_uppercase(), path), handler.clone());
                    }
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
    Resolved {
        routes: resolved,
        handlers,
        skipped,
    }
}

/// Resolved routes plus the handler serving each, so the type check can find
/// the code behind a declared operation.
pub(super) struct Resolved {
    pub(super) routes: BTreeMap<String, BTreeSet<&'static str>>,
    pub(super) handlers: BTreeMap<(String, String), String>,
    pub(super) skipped: usize,
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

/// The synthetic unit name for a router built into a local variable. Scoped to
/// the enclosing function, because two functions may each have their own `v1`.
pub(super) fn local_unit(scope: &str, name: &str) -> String {
    format!("{scope}::let {name}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scanner() -> RustScanner {
        RustScanner::new(|pattern| Regex::new(pattern).expect("valid pattern"))
    }

    /// Stand-in for the real per-line route extractor.
    fn hits(line: &str) -> Vec<RouteHit> {
        let route = Regex::new(r#"\.route\(\s*"([^"]+)"\s*,\s*(get|post)\("#).expect("pattern");
        route
            .captures_iter(line)
            .map(|captures| {
                let method: &'static str = if &captures[2] == "get" { "get" } else { "post" };
                (captures[1].to_string(), method, None)
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
        resolve(&units, join, |path| Some(path.to_string())).routes
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
    fn a_router_built_into_a_local_first_is_still_mounted() {
        // hey's shape: the router is conditional, so it lands in a local before
        // being nested. Matching only a direct call left all of these unprefixed.
        let routes = resolve_all(&[
            r#"
            fn app() -> Router {
                let v1 = match cfg.app_plane {
                    Plane::Regional => regional::routes(),
                    Plane::Global => global::routes(),
                };
                Router::new().nest("/v1", v1)
            }
            "#,
            r#"fn routes() -> Router { Router::new().route("/nearby", get(h)) }"#,
        ]);
        assert!(routes.contains_key("/v1/nearby"), "{routes:?}");
        assert!(
            !routes.contains_key("/nearby"),
            "the unprefixed duplicate must not also be emitted: {routes:?}"
        );
    }

    #[test]
    fn chained_routes_on_one_line_each_keep_their_own_method() {
        let routes = resolve_all(&[
            r#"fn app() -> Router { Router::new().route("/a", get(x)).route("/b", post(y)) }"#,
        ]);
        assert_eq!(routes.get("/a").map(|m| m.len()), Some(1), "{routes:?}");
        assert!(
            routes.contains_key("/b"),
            "the second route was dropped: {routes:?}"
        );
        assert!(routes["/b"].contains("post"), "{routes:?}");
        assert!(
            !routes["/a"].contains("post"),
            "methods leaked across routes: {routes:?}"
        );
    }

    #[test]
    fn a_local_built_from_inline_match_arms_is_mounted() {
        // hey's actual shape: the arms build the router inline, so there is no
        // callee to resolve to. Every route under it was staying unprefixed and
        // the drift check was calling 11 correct operations 404s.
        let routes = resolve_all(&[r#"
            fn app(cfg: &Cfg) -> Router {
                let v1 = match cfg.app_plane {
                    AppPlane::Global => Router::new().route("/auth/request-code", post(code)),
                    AppPlane::Regional => Router::new().route("/presence/update", post(pres)),
                };
                Router::new().route("/healthz", get(h)).nest("/v1", v1)
            }
            "#]);
        assert!(routes.contains_key("/v1/auth/request-code"), "{routes:?}");
        assert!(routes.contains_key("/v1/presence/update"), "{routes:?}");
        assert!(
            routes.contains_key("/healthz"),
            "the root route stays: {routes:?}"
        );
        assert!(
            !routes.contains_key("/presence/update"),
            "the unprefixed local path must not also be emitted: {routes:?}"
        );
    }

    #[test]
    fn two_functions_may_each_have_their_own_local_router() {
        let routes = resolve_all(&[
            r#"
            fn a() -> Router {
                let v1 = match x { _ => Router::new().route("/a", get(h)) };
                Router::new().nest("/one", v1)
            }
            "#,
            r#"
            fn b() -> Router {
                let v1 = match x { _ => Router::new().route("/b", get(h)) };
                Router::new().nest("/two", v1)
            }
            "#,
        ]);
        assert!(routes.contains_key("/one/a"), "{routes:?}");
        assert!(routes.contains_key("/two/b"), "{routes:?}");
        assert!(
            !routes.contains_key("/two/a"),
            "locals must not cross functions: {routes:?}"
        );
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
