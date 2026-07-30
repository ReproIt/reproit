//! Evaluating a Rust router expression down to the paths it serves.
//!
//! Split from the declaration reader because the two answer different
//! questions: that one records what each file declares, this one composes
//! those declarations into the paths a request can actually reach. Mounts,
//! prefixes, conditional branches and method chains all live here.
//!
//! The rule throughout is that an expression this does not understand yields
//! NO routes rather than a guess. A wrong path is worse than a missing one: it
//! reads downstream as a route the service does not serve.

use super::field_facts::FieldFact;
use super::route_path::join_mount as join;
use super::rust_types::Guards;
use std::collections::{BTreeMap, BTreeSet};
use syn::{Expr, Lit, Stmt};

/// Bound recursion through router-building functions.
const MAX_ROUTER_DEPTH: usize = 12;
/// Bound the routes one expression tree may contribute.
pub(super) const MAX_ROUTES: usize = 4_096;

/// One route as the parser sees it: path, method, and the handler serving it.
pub(super) type Route = (String, &'static str, Option<String>);

const METHODS: [&str; 7] = ["get", "post", "put", "patch", "delete", "head", "options"];

#[derive(Default)]
pub(super) struct Crate {
    /// QUALIFIED function name -> its body. Bodies are collected BEFORE any
    /// are evaluated: a router built by a function defined later in the walk is
    /// still a router, and evaluating in file order silently lost it.
    ///
    /// The key carries the module because two modules routinely export the same
    /// `routes()`, and a bare key let the second overwrite the first: one
    /// `.nest()` resolved and the other reported no routes at all.
    pub(super) fn_bodies: BTreeMap<String, syn::Block>,
    /// qualified type name -> fields.
    pub(super) structs: BTreeMap<String, BTreeMap<String, FieldFact>>,
    /// qualified enum name -> its unit variant values.
    pub(super) enums: BTreeMap<String, Vec<String>>,
    /// handler fn -> the bare name of its `Json<T>` request body.
    pub(super) handler_body: BTreeMap<String, String>,
    /// bare type name -> how many DIFFERENT declarations carry it.
    pub(super) declarations: BTreeMap<String, BTreeSet<String>>,
    /// Routes declared by an attribute on the handler, which belong to no
    /// router expression and are therefore always roots.
    pub(super) attribute_routes: Vec<Route>,
    /// handler -> the value guards its body enforces.
    pub(super) handler_guards: BTreeMap<String, Guards>,
    /// handler -> the response statuses and bodies its code states, with the
    /// bare names that stated conflicting facts.
    pub(super) handler_responses: BTreeMap<String, super::response_facts::ResponseFact>,
    pub(super) responses_ambiguous: BTreeSet<String>,
    /// bare serializer type name -> its wire fields, with the names declared
    /// differently in two modules (which resolve to neither).
    pub(super) serializers: super::response_facts::Serializers,
    pub(super) serializers_ambiguous: BTreeSet<String>,
}

/// The routes a named function evaluates to, guarding against recursion.
pub(super) fn routes_of_fn(
    name: &str,
    krate: &Crate,
    mounted: &mut BTreeSet<String>,
    visiting: &mut BTreeSet<String>,
    depth: usize,
) -> Vec<Route> {
    if depth > MAX_ROUTER_DEPTH || !visiting.insert(name.to_string()) {
        return Vec::new();
    }
    let Some(block) = krate.fn_bodies.get(name) else {
        return Vec::new();
    };
    let mut nested = BTreeSet::new();
    let routes = routes_of_block(
        &block.stmts,
        krate,
        &BTreeMap::new(),
        &mut nested,
        mounted,
        visiting,
        depth,
    );
    visiting.remove(name);
    routes
}

/// The routes a block evaluates to, following its local bindings.
///
/// A block yields its tail expression AND any local router it never mounted
/// into another one. Yielding only the tail was wrong for the shape almost
/// every real service uses:
///
/// ```ignore
/// async fn main() {
///     let app = Router::new().route("/health", get(health));
///     axum::serve(listener, app).await
/// }
/// ```
///
/// `main` returns `()`, so the block's value is nothing and every route in the
/// service was computed and then discarded. Each test written for this file
/// used `fn app() -> Router { Router::new()... }`, where the router IS the tail,
/// so nine of them passed over a reader that extracted nothing from a binary.
///
/// The block INHERITS the enclosing scope's bindings and shares its `nested`
/// set. An inner block that mounts an outer local has to suppress it in the
/// outer scope too: hey nests its dev router inside
/// `if let Some(dev) = dev_routes { app = app.nest("/dev", dev) }`, and a
/// per-block set left the outer scope free to emit those same routes
/// unprefixed, at paths the service does not serve.
fn routes_of_block(
    stmts: &[Stmt],
    krate: &Crate,
    inherited: &BTreeMap<String, Vec<Route>>,
    nested: &mut BTreeSet<String>,
    mounted: &mut BTreeSet<String>,
    visiting: &mut BTreeSet<String>,
    depth: usize,
) -> Vec<Route> {
    let mut locals: BTreeMap<String, Vec<Route>> = inherited.clone();
    // Names THIS block binds. An inherited local belongs to the scope that
    // introduced it and is emitted there; re-emitting it here multiplied one
    // router across every enclosing block.
    let mut own: BTreeSet<String> = BTreeSet::new();
    let mut routes = Vec::new();
    for stmt in stmts {
        match stmt {
            // `let v1 = match ... { .. };` binds a router to a name. The arms
            // are the router: there is no callee to follow, which is exactly
            // what the pattern reader could not express.
            Stmt::Local(local) => {
                let Some(init) = &local.init else { continue };
                let value =
                    routes_of_expr(&init.expr, krate, &locals, nested, mounted, visiting, depth);
                if !value.is_empty() {
                    if let syn::Pat::Ident(ident) = &local.pat {
                        own.insert(ident.ident.to_string());
                        locals.insert(ident.ident.to_string(), value);
                    }
                }
            }
            // `app = app.nest("/dev", dev);` REBINDS the router rather than
            // producing a value. Ignoring assignment dropped the mount, so the
            // nested routes surfaced without their prefix.
            Stmt::Expr(Expr::Assign(assign), _) => {
                let value = routes_of_expr(
                    &assign.right,
                    krate,
                    &locals,
                    nested,
                    mounted,
                    visiting,
                    depth,
                );
                if !value.is_empty() {
                    if let Some(name) = last_segment(&assign.left) {
                        own.insert(name.clone());
                        locals.insert(name, value);
                    }
                }
            }
            Stmt::Expr(expr, _) => routes.extend(routes_of_expr(
                expr, krate, &locals, nested, mounted, visiting, depth,
            )),
            _ => {}
        }
    }
    // A local is emitted only when this block bound it and nothing mounted it.
    // `let app = ...` handed to `serve` is the service; `let v1 = ...` nested
    // under `/v1` is not.
    for name in own {
        if nested.contains(&name) {
            continue;
        }
        if let Some(value) = locals.get(&name) {
            routes.extend(value.iter().cloned());
        }
    }
    routes.sort();
    routes.dedup();
    routes
}

/// The routes an expression evaluates to.
fn routes_of_expr(
    expr: &Expr,
    krate: &Crate,
    locals: &BTreeMap<String, Vec<Route>>,
    nested: &mut BTreeSet<String>,
    mounted: &mut BTreeSet<String>,
    visiting: &mut BTreeSet<String>,
    depth: usize,
) -> Vec<Route> {
    if depth > MAX_ROUTER_DEPTH {
        return Vec::new();
    }
    match expr {
        // A method chain is ITERATION, not nesting. Recursing into each
        // receiver charged every `.route()` against the recursion budget, so a
        // router declaring more routes than MAX_ROUTER_DEPTH in one chain lost
        // everything before the last few links. hey's regional plane declares
        // 24 routes that way and the reader returned the tail 12, which is far
        // worse than returning none: a silently truncated router reads as a
        // service that does not serve the routes it dropped.
        Expr::MethodCall(_) => {
            let mut chain = Vec::new();
            let mut base = expr;
            while let Expr::MethodCall(call) = base {
                if chain.len() >= MAX_ROUTES {
                    break;
                }
                chain.push(call);
                base = &call.receiver;
            }
            let mut routes =
                routes_of_expr(base, krate, locals, nested, mounted, visiting, depth + 1);
            // actix `web::scope("/api")` prefixes every route built on it, and
            // it is the CHAIN's base rather than a link in it.
            let scope = scope_prefix(base);
            // actix `web::resource("/items")` states the path once, then each
            // `.route(web::get().to(handler))` contributes a method.
            let resource = resource_path(base);
            // warp states the whole path in a macro at the base of the filter
            // chain and the verb in an `.and(warp::get())` link further along.
            if let Some(path) = warp_path(base) {
                let verb = chain
                    .iter()
                    .find_map(|call| warp_method(call))
                    .unwrap_or("get");
                routes.push((path, verb, None));
            }
            // Innermost first, so a `.nest()` sees the routes built before it.
            for call in chain.iter().rev() {
                let method = call.method.to_string();
                let mut args = call.args.iter();
                match method.as_str() {
                    "route" => {
                        if call.args.len() == 1 {
                            if let (Some(path), Some(handlers)) =
                                (resource.as_ref(), call.args.first())
                            {
                                for (verb, handler) in method_router(handlers) {
                                    routes.push((path.clone(), verb, handler));
                                }
                                continue;
                            }
                        }
                        if let (Some(path), Some(handlers)) = (args.next(), args.next()) {
                            if let Some(path) = string_of(path) {
                                for (verb, handler) in method_router(handlers) {
                                    routes.push((path.clone(), verb, handler));
                                }
                            }
                        }
                    }
                    "nest" | "nest_service" => {
                        if let (Some(prefix), Some(inner)) = (args.next(), args.next()) {
                            if let Some(prefix) = string_of(prefix) {
                                // The mounted router is reachable at this
                                // prefix, so it must not also surface at its
                                // local path.
                                if let Some(name) = last_segment(inner) {
                                    nested.insert(name);
                                }
                                for (path, verb, handler) in routes_of_expr(
                                    inner,
                                    krate,
                                    locals,
                                    nested,
                                    mounted,
                                    visiting,
                                    depth + 1,
                                ) {
                                    routes.push((join(&prefix, &path), verb, handler));
                                }
                            }
                        }
                    }
                    // rocket: `.mount("/api", routes![status, create])`. The
                    // paths live on the handlers' own attributes, so the mount
                    // contributes the PREFIX for the handlers it names. Without
                    // this every rocket route was reported one prefix short,
                    // which is the dangerous direction: a declared `/api/status`
                    // matched nothing in source.
                    "mount" => {
                        if let (Some(prefix), Some(inner)) = (args.next(), args.next()) {
                            if let Some(prefix) = string_of(prefix) {
                                for handler in mounted_handlers(inner) {
                                    for (path, verb, owner) in &krate.attribute_routes {
                                        if owner.as_deref() != Some(handler.as_str()) {
                                            continue;
                                        }
                                        routes.push((join(&prefix, path), verb, owner.clone()));
                                        // The handler is reachable at the mount,
                                        // so its bare attribute path must not
                                        // also surface as a root.
                                        mounted.insert(handler.clone());
                                    }
                                }
                            }
                        }
                    }
                    // actix: `.service(inner)` composes at the SAME level, like
                    // `.merge`, and the inner may itself be a scoped router.
                    "service" | "configure" => {
                        if let Some(inner) = args.next() {
                            if let Some(name) = last_segment(inner) {
                                nested.insert(name);
                            }
                            routes.extend(routes_of_expr(
                                inner,
                                krate,
                                locals,
                                nested,
                                mounted,
                                visiting,
                                depth + 1,
                            ));
                        }
                    }
                    // warp `.or(other)` composes two filters at the SAME level,
                    // exactly like `.merge`.
                    // `.merge(other)` composes a router at the SAME level, so
                    // its routes are this router's routes, unprefixed.
                    "merge" | "or" => {
                        if let Some(inner) = args.next() {
                            if let Some(name) = last_segment(inner) {
                                nested.insert(name);
                            }
                            routes.extend(routes_of_expr(
                                inner,
                                krate,
                                locals,
                                nested,
                                mounted,
                                visiting,
                                depth + 1,
                            ));
                        }
                    }
                    // `.layer(..)`, `.with_state(..)` and friends pass the
                    // router through unchanged.
                    _ => {}
                }
            }
            match scope {
                Some(prefix) => routes
                    .into_iter()
                    .map(|(path, verb, handler)| (join(&prefix, &path), verb, handler))
                    .collect(),
                None => routes,
            }
        }
        // A router built by another function.
        // A router built by another function: evaluate it, and remember that it
        // is reachable through this mount rather than on its own.
        Expr::Call(call) => {
            let written = written_path(&call.func).unwrap_or_default();
            // `Some(router)` / `Ok(router)` wrap a router without changing it,
            // and actix builds its app inside `HttpServer::new(|| ...)`. Not
            // seeing through these lost whole routers: hey declares seven
            // dev-only routes inside `Some(Router::new()...)` and the reader
            // returned nothing for them, which is indistinguishable from the
            // service not having them.
            if matches!(written.as_str(), "Some" | "Ok" | "new" | "HttpServer::new") {
                let mut routes = Vec::new();
                for argument in &call.args {
                    routes.extend(routes_of_expr(
                        argument,
                        krate,
                        locals,
                        nested,
                        mounted,
                        visiting,
                        depth + 1,
                    ));
                }
                return routes;
            }
            if let Some(name) = resolve_fn(krate, &written) {
                let routes = routes_of_fn(&name, krate, mounted, visiting, depth + 1);
                if !routes.is_empty() {
                    mounted.insert(name);
                }
                return routes;
            }
            // An unresolved call may still BUILD a router in a closure argument:
            // `AdHoc::on_ignite("q", |rocket| async { rocket.mount("/queue", ..) })`.
            // Only closure and async arguments are evaluated. Evaluating every
            // argument would re-emit a router merely PASSED to a function, at
            // its unprefixed local path.
            let mut routes = Vec::new();
            for argument in &call.args {
                if !matches!(argument, Expr::Closure(_) | Expr::Async(_)) {
                    continue;
                }
                routes.extend(routes_of_expr(
                    argument,
                    krate,
                    locals,
                    nested,
                    mounted,
                    visiting,
                    depth + 1,
                ));
            }
            routes
        }
        // `HttpServer::new(|| App::new().route(..))`: the closure body IS the app.
        Expr::Closure(node) => routes_of_expr(
            &node.body,
            krate,
            locals,
            nested,
            mounted,
            visiting,
            depth + 1,
        ),
        // Forms that wrap a value without changing it.
        Expr::Try(node) => routes_of_expr(
            &node.expr,
            krate,
            locals,
            nested,
            mounted,
            visiting,
            depth + 1,
        ),
        Expr::Await(node) => routes_of_expr(
            &node.base,
            krate,
            locals,
            nested,
            mounted,
            visiting,
            depth + 1,
        ),
        Expr::Group(node) => routes_of_expr(
            &node.expr,
            krate,
            locals,
            nested,
            mounted,
            visiting,
            depth + 1,
        ),
        // A fairing builds its routes inside `async move { rocket.mount(..) }`.
        Expr::Async(node) => routes_of_block(
            &node.block.stmts,
            krate,
            locals,
            nested,
            mounted,
            visiting,
            depth + 1,
        ),
        Expr::Unsafe(node) => routes_of_block(
            &node.block.stmts,
            krate,
            locals,
            nested,
            mounted,
            visiting,
            depth + 1,
        ),
        // A local holding a router.
        Expr::Path(path) => path
            .path
            .segments
            .last()
            .and_then(|segment| locals.get(&segment.ident.to_string()))
            .cloned()
            .unwrap_or_default(),
        // Every branch can be the router, so every branch contributes.
        Expr::Match(node) => {
            let mut routes = Vec::new();
            for arm in &node.arms {
                routes.extend(routes_of_expr(
                    &arm.body,
                    krate,
                    locals,
                    nested,
                    mounted,
                    visiting,
                    depth + 1,
                ));
            }
            routes
        }
        Expr::If(node) => {
            // `if let Some(dev) = dev_routes` binds the inner name to the outer
            // router, and consumes the outer one: it is reachable through this
            // branch's mount, not on its own.
            let mut scope = locals.clone();
            let mut aliases: Vec<(String, String)> = Vec::new();
            if let Expr::Let(binding) = &*node.cond {
                if let Some(source) = last_segment(&binding.expr) {
                    if let Some(value) = locals.get(&source).cloned() {
                        for name in pattern_names(&binding.pat) {
                            scope.insert(name.clone(), value.clone());
                            aliases.push((name, source.clone()));
                        }
                    }
                }
            }
            let mut routes = routes_of_block(
                &node.then_branch.stmts,
                krate,
                &scope,
                nested,
                mounted,
                visiting,
                depth + 1,
            );
            // The branch mounted the binding, so the local it aliased is
            // reachable at that mount and must not also surface on its own.
            for (bound, source) in aliases {
                if nested.contains(&bound) {
                    nested.insert(source);
                }
            }
            if let Some((_, other)) = &node.else_branch {
                routes.extend(routes_of_expr(
                    other,
                    krate,
                    locals,
                    nested,
                    mounted,
                    visiting,
                    depth + 1,
                ));
            }
            routes
        }
        Expr::Block(node) => routes_of_block(
            &node.block.stmts,
            krate,
            locals,
            nested,
            mounted,
            visiting,
            depth + 1,
        ),
        Expr::Reference(node) => routes_of_expr(
            &node.expr,
            krate,
            locals,
            nested,
            mounted,
            visiting,
            depth + 1,
        ),
        Expr::Paren(node) => routes_of_expr(
            &node.expr,
            krate,
            locals,
            nested,
            mounted,
            visiting,
            depth + 1,
        ),
        _ => Vec::new(),
    }
}

/// `get(list).post(create)` -> the verbs and their handlers.
fn method_router(expr: &Expr) -> Vec<(&'static str, Option<String>)> {
    match expr {
        Expr::Call(call) => match last_segment(&call.func).as_deref().and_then(verb_of) {
            Some(verb) => vec![(verb, call.args.first().and_then(last_segment))],
            None => Vec::new(),
        },
        Expr::MethodCall(call) => {
            let mut verbs = method_router(&call.receiver);
            if let Some(verb) = verb_of(&call.method.to_string()) {
                verbs.push((verb, call.args.first().and_then(last_segment)));
            }
            verbs
        }
        _ => Vec::new(),
    }
}

/// The verb a method-router constructor names.
///
/// `get_service`/`post_service` mount a Service for the same verb, and `any`
/// answers every verb at once. A draft can exercise one, and GET is the one
/// that cannot mutate anything if it is the wrong guess.
fn verb_of(name: &str) -> Option<&'static str> {
    if name == "any" || name == "any_service" {
        return Some("get");
    }
    let bare = name.strip_suffix("_service").unwrap_or(name);
    METHODS.iter().copied().find(|verb| *verb == bare)
}

fn string_of(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Lit(lit) => match &lit.lit {
            Lit::Str(text) => Some(text.value()),
            _ => None,
        },
        _ => None,
    }
}

/// A call's callee as written, e.g. `users::routes` or `routes`. `crate` and
/// `self` prefixes are dropped: they say where the caller is, not which module
/// declares the callee.
fn written_path(expr: &Expr) -> Option<String> {
    let Expr::Path(path) = expr else {
        return None;
    };
    let segments: Vec<String> = path
        .path
        .segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .filter(|segment| segment != "crate" && segment != "self" && segment != "super")
        .collect();
    (!segments.is_empty()).then(|| segments.join("::"))
}

/// The one qualified function a written path names, or None.
///
/// None covers both "no such function" and "more than one module declares it",
/// and the two must stay indistinguishable HERE: resolving an ambiguous name by
/// picking one would attach a real router to the wrong mount, which reads as a
/// route the service does not serve.
fn resolve_fn(krate: &Crate, written: &str) -> Option<String> {
    let suffix = format!("::{written}");
    let mut matches = krate
        .fn_bodies
        .keys()
        .filter(|qualified| *qualified == written || qualified.ends_with(&suffix));
    let first = matches.next()?;
    matches.next().is_none().then(|| first.clone())
}

fn last_segment(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Path(path) => path
            .path
            .segments
            .last()
            .map(|segment| segment.ident.to_string()),
        _ => None,
    }
}

/// `warp::path!("api" / "items")` -> `/api/items`.
///
/// Literal segments only. warp's parameters are positional and unnamed
/// (`path!("items" / u32)`), so there is no name to put in a path template;
/// inventing one would produce a path that matches no declared operation,
/// which reads as a route the service does not serve. Those stay unread.
fn warp_path(base: &Expr) -> Option<String> {
    let Expr::Macro(node) = base else {
        return None;
    };
    let name = node.mac.path.segments.last()?.ident.to_string();
    if name != "path" {
        return None;
    }
    let tokens = node.mac.tokens.to_string();
    let mut segments = Vec::new();
    for part in tokens.split('/') {
        let part = part.trim();
        let literal = part
            .strip_prefix('"')
            .and_then(|rest| rest.strip_suffix('"'))?;
        if literal.is_empty() {
            continue;
        }
        segments.push(literal.to_string());
    }
    (!segments.is_empty()).then(|| format!("/{}", segments.join("/")))
}

/// The verb an `.and(warp::post())` link states.
fn warp_method(call: &syn::ExprMethodCall) -> Option<&'static str> {
    if call.method != "and" {
        return None;
    }
    let Some(Expr::Call(inner)) = call.args.first() else {
        return None;
    };
    let written = written_path(&inner.func)?;
    let verb = written.rsplit("::").next()?.to_ascii_lowercase();
    METHODS.into_iter().find(|known| *known == verb)
}

/// `web::scope("/api")` -> `/api`, the prefix a chain built on it carries.
fn scope_prefix(base: &Expr) -> Option<String> {
    let Expr::Call(call) = base else {
        return None;
    };
    let written = written_path(&call.func)?;
    if written != "scope" && !written.ends_with("::scope") {
        return None;
    }
    string_of(call.args.first()?)
}

/// `web::resource("/items")` -> the path its `.route(..)` links serve.
fn resource_path(base: &Expr) -> Option<String> {
    let Expr::Call(call) = base else {
        return None;
    };
    let written = written_path(&call.func)?;
    if written != "resource" && !written.ends_with("::resource") {
        return None;
    }
    string_of(call.args.first()?)
}

/// The handler names a `routes![a, b]` macro lists.
///
/// The macro body is tokens, not an expression tree, so the idents are read
/// directly. Anything that is not a bare ident is skipped: a generated or
/// re-exported name resolved to a guess would attach a real path to the wrong
/// mount.
fn mounted_handlers(expr: &Expr) -> Vec<String> {
    let Expr::Macro(node) = expr else {
        return Vec::new();
    };
    node.mac
        .tokens
        .to_string()
        .split(',')
        .filter_map(|part| {
            let name = part.trim();
            let bare = !name.is_empty()
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && !name.starts_with(|c: char| c.is_ascii_digit());
            bare.then(|| name.to_string())
        })
        .collect()
}

/// The identifiers a pattern binds. Only the simple forms are read: a pattern
/// this cannot decompose binds nothing rather than binding the wrong name.
fn pattern_names(pat: &syn::Pat) -> Vec<String> {
    match pat {
        syn::Pat::Ident(ident) => vec![ident.ident.to_string()],
        syn::Pat::TupleStruct(inner) => inner.elems.iter().flat_map(pattern_names).collect(),
        syn::Pat::Tuple(inner) => inner.elems.iter().flat_map(pattern_names).collect(),
        syn::Pat::Reference(inner) => pattern_names(&inner.pat),
        syn::Pat::Paren(inner) => pattern_names(&inner.pat),
        _ => Vec::new(),
    }
}

/// `#[get("/status")]` on a handler, as actix and rocket declare routes.
/// Rocket paths may carry a `?<query>` suffix; the path is the part before it.
pub(super) fn attribute_route(attrs: &[syn::Attribute]) -> Option<(&'static str, String)> {
    attrs.iter().find_map(|attr| {
        let name = attr.path().segments.last()?.ident.to_string();
        let verb = METHODS.iter().find(|method| **method == name)?;
        let syn::Meta::List(list) = &attr.meta else {
            return None;
        };
        let tokens = list.tokens.to_string();
        let path = tokens.split('"').nth(1)?;
        Some((*verb, path.split('?').next()?.to_string()))
    })
}
