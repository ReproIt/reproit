//! Rust route and type extraction over a real parse, not over text.
//!
//! The pattern-based reader could not know what it failed to match, so an
//! unreadable route and an absent one were the same observation. Every false
//! "the source does not serve this" came from that: a rustfmt-wrapped call, a
//! router built in a match arm, a type declared in two modules.
//!
//! `syn` removes the ambiguity rather than narrowing it. A file either parses,
//! in which case every route and every field in it is seen exactly, or it does
//! not, in which case we KNOW we did not read it and say so. That is the
//! difference between "no route found" and "no route exists", and it is the
//! whole reason to carry a parser.
//!
//! Types are qualified by module path, so two modules declaring the same name
//! are two types instead of one silently overwriting the other.

use super::field_facts::FieldFact;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use syn::{Expr, Item, Lit, Stmt};

/// Bound recursion through router-building functions.
const MAX_ROUTER_DEPTH: usize = 12;
/// Bound the routes one expression tree may contribute.
const MAX_ROUTES: usize = 4_096;

/// One route as the parser sees it: path, method, and the handler serving it.
type Route = (String, &'static str, Option<String>);

const METHODS: [&str; 7] = ["get", "post", "put", "patch", "delete", "head", "options"];

#[derive(Debug, Default)]
pub(super) struct RustSource {
    /// Resolved path -> methods.
    pub(super) routes: BTreeMap<String, BTreeSet<&'static str>>,
    /// (METHOD, resolved path) -> handler function name.
    pub(super) handlers: BTreeMap<(String, String), String>,
    /// handler -> request body fields.
    pub(super) bodies: BTreeMap<String, BTreeMap<String, FieldFact>>,
    pub(super) files_parsed: usize,
    /// Files `syn` could not parse. Non-zero means the reader has a blind spot
    /// and any absence it reports is unreliable, which the caller must say.
    pub(super) files_unparsed: usize,
}

/// Everything one crate's sources declare.
#[derive(Default)]
struct Crate {
    /// function name -> its body. Bodies are collected BEFORE any are
    /// evaluated: a router built by a function defined later in the walk is
    /// still a router, and evaluating in file order silently lost it.
    fn_bodies: BTreeMap<String, syn::Block>,
    /// qualified type name -> fields.
    structs: BTreeMap<String, BTreeMap<String, FieldFact>>,
    /// qualified enum name -> its unit variant values.
    enums: BTreeMap<String, Vec<String>>,
    /// handler fn -> the bare name of its `Json<T>` request body.
    handler_body: BTreeMap<String, String>,
    /// bare type name -> how many DIFFERENT declarations carry it.
    declarations: BTreeMap<String, BTreeSet<String>>,
    /// Routes declared by an attribute on the handler, which belong to no
    /// router expression and are therefore always roots.
    attribute_routes: Vec<Route>,
    /// handler -> the value guards its body enforces.
    handler_guards: BTreeMap<String, Guards>,
}

pub(super) fn read(root: &Path) -> RustSource {
    let mut source = RustSource::default();
    let mut krate = Crate::default();
    let mut files = Vec::new();
    for file in super::extract::family_sources(root, super::extract::Family::Rust) {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        match syn::parse_file(&text) {
            Ok(parsed) => {
                source.files_parsed += 1;
                files.push((module_of(root, &file), parsed));
            }
            // A file that does not parse is a KNOWN blind spot, not an empty one.
            Err(_) => source.files_unparsed += 1,
        }
    }
    for (module, file) in &files {
        collect(&file.items, module, &mut krate);
    }
    resolve(&krate, &mut source);
    source
}

/// A module path from the file's location, so `models::Request` and
/// `legacy::Request` stay distinct types.
fn module_of(root: &Path, file: &Path) -> String {
    file.strip_prefix(root)
        .unwrap_or(file)
        .with_extension("")
        .components()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("::")
}

fn collect(items: &[Item], module: &str, krate: &mut Crate) {
    for item in items {
        match item {
            Item::Mod(inner) => {
                if let Some((_, nested)) = &inner.content {
                    let module = format!("{module}::{}", inner.ident);
                    collect(nested, &module, krate);
                }
            }
            Item::Fn(function) => {
                let name = function.sig.ident.to_string();
                // actix and rocket put the route on the handler: `#[get("/x")]`.
                // The function IS the route, so it is emitted directly rather
                // than through a router expression.
                if let Some((verb, path)) = attribute_route(&function.attrs) {
                    krate
                        .attribute_routes
                        .push((path, verb, Some(name.clone())));
                }
                krate
                    .fn_bodies
                    .insert(name.clone(), (*function.block).clone());
                if let Some(body) = json_body_type(function) {
                    krate.handler_body.insert(name.clone(), body);
                    let mut guards = Guards::default();
                    syn::visit::Visit::visit_block(&mut guards, &function.block);
                    krate.handler_guards.insert(name, guards);
                }
            }
            Item::Struct(item) => {
                let qualified = format!("{module}::{}", item.ident);
                let fields = struct_fields(item);
                if !fields.is_empty() {
                    krate
                        .declarations
                        .entry(item.ident.to_string())
                        .or_default()
                        .insert(fingerprint(&fields));
                    krate.structs.insert(qualified, fields);
                }
            }
            Item::Enum(item) => {
                if let Some(values) = unit_variants(item) {
                    krate
                        .enums
                        .insert(format!("{module}::{}", item.ident), values);
                }
            }
            _ => {}
        }
    }
}

/// A structural fingerprint, so an identical re-declaration (a re-export, a cfg
/// twin) is one type while a genuinely different one is ambiguous.
fn fingerprint(fields: &BTreeMap<String, FieldFact>) -> String {
    fields
        .iter()
        .map(|(name, fact)| format!("{name}:{}:{:?}", fact.required, fact.allowed))
        .collect::<Vec<_>>()
        .join(",")
}

/// Compose the router functions into final paths.
fn resolve(krate: &Crate, source: &mut RustSource) {
    // Evaluate every function once, recording which functions were reached
    // THROUGH another one. A router someone mounts is reachable at its mount,
    // so emitting its local paths as well would invent routes the service does
    // not serve.
    let mut evaluated: BTreeMap<String, Vec<Route>> = BTreeMap::new();
    let mut mounted: BTreeSet<String> = BTreeSet::new();
    for name in krate.fn_bodies.keys() {
        let mut visiting = BTreeSet::new();
        let routes = routes_of_fn(name, krate, &mut mounted, &mut visiting, 0);
        if !routes.is_empty() {
            evaluated.insert(name.clone(), routes);
        }
    }
    for (name, routes) in evaluated
        .iter()
        .map(|(name, routes)| (Some(name), routes))
        .chain(std::iter::once((None, &krate.attribute_routes)))
    {
        if name.is_some_and(|name| mounted.contains(name)) {
            continue;
        }
        for (path, method, handler) in routes {
            if source.routes.len() >= MAX_ROUTES {
                break;
            }
            let Some(normalized) = super::extract::normalize_path(path) else {
                continue;
            };
            source
                .routes
                .entry(normalized.clone())
                .or_default()
                .insert(method);
            if let Some(handler) = handler {
                source
                    .handlers
                    .insert((method.to_uppercase(), normalized), handler.clone());
            }
        }
    }
    for (handler, body) in &krate.handler_body {
        // Ambiguous by bare name: two modules declared it differently, so which
        // type this handler takes is genuinely unknown.
        if krate
            .declarations
            .get(body)
            .is_some_and(|shapes| shapes.len() > 1)
        {
            continue;
        }
        let Some((qualified, fields)) = krate
            .structs
            .iter()
            .find(|(qualified, _)| qualified.rsplit("::").next() == Some(body.as_str()))
        else {
            continue;
        };
        let _ = qualified;
        let mut fields = fields.clone();
        // Resolve enum-typed fields now that every module has been read.
        for fact in fields.values_mut() {
            if let Some(name) = fact.evidence.as_deref().and_then(|e| e.strip_prefix('@')) {
                let values = krate
                    .enums
                    .iter()
                    .find(|(qualified, _)| qualified.rsplit("::").next() == Some(name));
                match values {
                    Some((_, values)) => {
                        fact.allowed = Some(values.clone());
                        fact.evidence = Some("a unit-only enum".to_string());
                    }
                    None => fact.evidence = None,
                }
            }
        }
        // A guard in the handler body settles what the type cannot.
        if let Some(guards) = krate.handler_guards.get(handler) {
            for (name, fact) in fields.iter_mut() {
                if fact.allowed.is_none() {
                    if let Some(values) = guards.allowed.get(name) {
                        fact.allowed = Some(values.clone());
                        fact.evidence = Some("an explicit value guard in the handler".to_string());
                    }
                }
                if fact.range.is_none() {
                    if let Some(range) = guards.ranges.get(name) {
                        fact.range = Some(*range);
                        fact.evidence = Some("an explicit range guard in the handler".to_string());
                    }
                }
            }
        }
        source.bodies.insert(handler.clone(), fields);
    }
}

/// The routes a named function evaluates to, guarding against recursion.
fn routes_of_fn(
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
    let routes = routes_of_block(&block.stmts, krate, mounted, visiting, depth);
    visiting.remove(name);
    routes
}

/// The routes a block evaluates to, following its local bindings.
fn routes_of_block(
    stmts: &[Stmt],
    krate: &Crate,
    mounted: &mut BTreeSet<String>,
    visiting: &mut BTreeSet<String>,
    depth: usize,
) -> Vec<Route> {
    let mut locals: BTreeMap<String, Vec<Route>> = BTreeMap::new();
    let mut routes = Vec::new();
    for stmt in stmts {
        match stmt {
            // `let v1 = match ... { .. };` binds a router to a name. The arms
            // are the router: there is no callee to follow, which is exactly
            // what the pattern reader could not express.
            Stmt::Local(local) => {
                let Some(init) = &local.init else { continue };
                let value = routes_of_expr(&init.expr, krate, &locals, mounted, visiting, depth);
                if !value.is_empty() {
                    if let syn::Pat::Ident(ident) = &local.pat {
                        locals.insert(ident.ident.to_string(), value);
                    }
                }
            }
            Stmt::Expr(expr, _) => routes.extend(routes_of_expr(
                expr, krate, &locals, mounted, visiting, depth,
            )),
            _ => {}
        }
    }
    routes
}

/// The routes an expression evaluates to.
fn routes_of_expr(
    expr: &Expr,
    krate: &Crate,
    locals: &BTreeMap<String, Vec<Route>>,
    mounted: &mut BTreeSet<String>,
    visiting: &mut BTreeSet<String>,
    depth: usize,
) -> Vec<Route> {
    if depth > MAX_ROUTER_DEPTH {
        return Vec::new();
    }
    match expr {
        Expr::MethodCall(call) => {
            let mut routes =
                routes_of_expr(&call.receiver, krate, locals, mounted, visiting, depth + 1);
            let method = call.method.to_string();
            let mut args = call.args.iter();
            match method.as_str() {
                "route" => {
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
                            for (path, verb, handler) in
                                routes_of_expr(inner, krate, locals, mounted, visiting, depth + 1)
                            {
                                routes.push((join(&prefix, &path), verb, handler));
                            }
                        }
                    }
                }
                // `.layer(..)`, `.with_state(..)` and friends pass the router
                // through unchanged, which the receiver recursion already did.
                _ => {}
            }
            routes
        }
        // A router built by another function.
        // A router built by another function: evaluate it, and remember that it
        // is reachable through this mount rather than on its own.
        Expr::Call(call) => match last_segment(&call.func) {
            Some(name) if krate.fn_bodies.contains_key(&name) => {
                let routes = routes_of_fn(&name, krate, mounted, visiting, depth + 1);
                if !routes.is_empty() {
                    mounted.insert(name);
                }
                routes
            }
            _ => Vec::new(),
        },
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
                    mounted,
                    visiting,
                    depth + 1,
                ));
            }
            routes
        }
        Expr::If(node) => {
            let mut routes =
                routes_of_block(&node.then_branch.stmts, krate, mounted, visiting, depth + 1);
            if let Some((_, other)) = &node.else_branch {
                routes.extend(routes_of_expr(
                    other,
                    krate,
                    locals,
                    mounted,
                    visiting,
                    depth + 1,
                ));
            }
            routes
        }
        Expr::Block(node) => {
            routes_of_block(&node.block.stmts, krate, mounted, visiting, depth + 1)
        }
        Expr::Reference(node) => {
            routes_of_expr(&node.expr, krate, locals, mounted, visiting, depth + 1)
        }
        Expr::Paren(node) => {
            routes_of_expr(&node.expr, krate, locals, mounted, visiting, depth + 1)
        }
        _ => Vec::new(),
    }
}

/// `get(list).post(create)` -> the verbs and their handlers.
fn method_router(expr: &Expr) -> Vec<(&'static str, Option<String>)> {
    match expr {
        Expr::Call(call) => match last_segment(&call.func) {
            Some(name) => METHODS
                .iter()
                .find(|verb| **verb == name)
                .map(|verb| {
                    let handler = call.args.first().and_then(last_segment);
                    vec![(*verb, handler)]
                })
                .unwrap_or_default(),
            None => Vec::new(),
        },
        Expr::MethodCall(call) => {
            let mut verbs = method_router(&call.receiver);
            let name = call.method.to_string();
            if let Some(verb) = METHODS.iter().find(|verb| **verb == name) {
                verbs.push((*verb, call.args.first().and_then(last_segment)));
            }
            verbs
        }
        _ => Vec::new(),
    }
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

fn join(prefix: &str, path: &str) -> String {
    let base = prefix.trim_end_matches('/');
    if path.is_empty() || path == "/" {
        return base.to_string();
    }
    format!("{base}{path}")
}

/// The `Json<T>` REQUEST body of a handler. A `Json<T>` RETURN type is not one.
fn json_body_type(function: &syn::ItemFn) -> Option<String> {
    function.sig.inputs.iter().find_map(|input| {
        let syn::FnArg::Typed(typed) = input else {
            return None;
        };
        inner_of(&typed.ty, "Json")
    })
}

/// The single generic argument of `Wrapper<T>`, by bare name.
fn inner_of(ty: &syn::Type, wrapper: &str) -> Option<String> {
    let syn::Type::Path(path) = ty else {
        return None;
    };
    let segment = path.path.segments.last()?;
    if segment.ident != wrapper {
        return None;
    }
    let syn::PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    args.args.iter().find_map(|arg| match arg {
        syn::GenericArgument::Type(syn::Type::Path(inner)) => {
            Some(inner.path.segments.last()?.ident.to_string())
        }
        _ => None,
    })
}

fn struct_fields(item: &syn::ItemStruct) -> BTreeMap<String, FieldFact> {
    let syn::Fields::Named(named) = &item.fields else {
        return BTreeMap::new();
    };
    let mut fields = BTreeMap::new();
    for field in &named.named {
        let Some(ident) = &field.ident else { continue };
        let name = serde_rename(&field.attrs).unwrap_or_else(|| ident.to_string());
        let optional = inner_of(&field.ty, "Option").is_some();
        let declared = inner_of(&field.ty, "Option").or_else(|| bare_type(&field.ty));
        let range = super::field_facts::attribute_range(
            &field.attrs.iter().map(quote_attr).collect::<Vec<_>>(),
        );
        fields.insert(
            name,
            FieldFact {
                required: !optional,
                // Remembered by name; resolved once every module is read.
                evidence: match &range {
                    Some(_) => Some("a validation attribute on the field".to_string()),
                    None => declared.map(|name| format!("@{name}")),
                },
                allowed: None,
                range,
            },
        );
    }
    fields
}

fn bare_type(ty: &syn::Type) -> Option<String> {
    let syn::Type::Path(path) = ty else {
        return None;
    };
    Some(path.path.segments.last()?.ident.to_string())
}

/// A unit-only enum's serde-visible values. A data-carrying variant means the
/// set is not closed, so it abstains.
fn unit_variants(item: &syn::ItemEnum) -> Option<Vec<String>> {
    let rename_all = item.attrs.iter().find_map(|attr| {
        let text = quote_attr(attr);
        text.split("rename_all")
            .nth(1)?
            .split('"')
            .nth(1)
            .map(str::to_string)
    });
    let mut values = Vec::new();
    for variant in &item.variants {
        if !matches!(variant.fields, syn::Fields::Unit) {
            return None;
        }
        values.push(match serde_rename(&variant.attrs) {
            Some(renamed) => renamed,
            None => super::field_facts::apply_rename_all(
                &variant.ident.to_string(),
                rename_all.as_deref(),
            ),
        });
    }
    (!values.is_empty()).then_some(values)
}

fn serde_rename(attrs: &[syn::Attribute]) -> Option<String> {
    attrs.iter().find_map(|attr| {
        let text = quote_attr(attr);
        if !text.contains("serde") {
            return None;
        }
        let after = text.split("rename").nth(1)?;
        if after.trim_start().starts_with("_all") {
            return None;
        }
        after.split('"').nth(1).map(str::to_string)
    })
}

/// An attribute's argument text. serde and validator take arbitrary token
/// trees, so this last mile stays textual, but it operates on tokens the parser
/// produced rather than on a line of a file.
fn quote_attr(attr: &syn::Attribute) -> String {
    let path = attr
        .path()
        .segments
        .last()
        .map(|segment| segment.ident.to_string())
        .unwrap_or_default();
    match &attr.meta {
        syn::Meta::List(list) => format!("{path} {}", list.tokens),
        _ => path,
    }
}

/// `#[get("/status")]` on a handler, as actix and rocket declare routes.
/// Rocket paths may carry a `?<query>` suffix; the path is the part before it.
fn attribute_route(attrs: &[syn::Attribute]) -> Option<(&'static str, String)> {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A unique directory per call: these run in parallel, and keying on
    /// anything shared makes them clobber each other.
    fn read_source(case: &str, files: &[(&str, &str)]) -> RustSource {
        let root = std::env::temp_dir().join(format!("reproit-ast-{}-{case}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("root");
        for (name, body) in files {
            std::fs::write(root.join(name), body).expect("write");
        }
        let source = read(&root);
        let _ = std::fs::remove_dir_all(&root);
        source
    }

    #[test]
    fn a_conditional_router_built_into_a_local_resolves_from_both_arms() {
        let source = read_source(
            "a_conditional_router_built_into_a_local_resolves_from_both_arms",
            &[(
                "main.rs",
                r#"
            fn app(cfg: &Cfg) -> Router {
                let v1 = match cfg.plane {
                    Plane::Regional => Router::new()
                        .route(
                            "/presence/update",
                            post(update_presence),
                        )
                        .route("/nearby", get(nearby)),
                    Plane::Global => Router::new().route("/profile", get(profile)),
                };
                Router::new().route("/healthz", get(health)).nest("/v1", v1)
            }
            "#,
            )],
        );
        let paths: Vec<&String> = source.routes.keys().collect();
        assert!(
            source.routes.contains_key("/v1/presence/update"),
            "{paths:?}"
        );
        assert!(source.routes.contains_key("/v1/nearby"), "{paths:?}");
        assert!(source.routes.contains_key("/v1/profile"), "{paths:?}");
        assert!(source.routes.contains_key("/healthz"), "{paths:?}");
        assert!(
            !source.routes.contains_key("/nearby"),
            "no unprefixed twin: {paths:?}"
        );
    }

    #[test]
    fn a_router_mounted_from_another_function_is_not_also_emitted_unprefixed() {
        let source = read_source(
            "mounted_fn",
            &[
                (
                    "main.rs",
                    r#"fn app() -> Router { Router::new().nest("/api", users::routes()) }"#,
                ),
                (
                    "users.rs",
                    r#"pub fn routes() -> Router { Router::new().route("/users", get(list)) }"#,
                ),
            ],
        );
        let paths: Vec<&String> = source.routes.keys().collect();
        assert!(source.routes.contains_key("/api/users"), "{paths:?}");
        assert!(
            !source.routes.contains_key("/users"),
            "a mounted router must not also surface at its local path: {paths:?}"
        );
    }

    #[test]
    fn chained_methods_on_one_route_keep_their_own_handlers() {
        let source = read_source(
            "chained_methods_on_one_route_keep_their_own_handlers",
            &[(
                "main.rs",
                r#"fn app() -> Router { Router::new().route("/users", get(list).post(create)) }"#,
            )],
        );
        assert_eq!(source.routes["/users"].len(), 2);
        assert_eq!(
            source.handlers.get(&("POST".into(), "/users".into())),
            Some(&"create".to_string())
        );
        assert_eq!(
            source.handlers.get(&("GET".into(), "/users".into())),
            Some(&"list".to_string())
        );
    }

    #[test]
    fn a_type_declared_differently_in_two_modules_is_ambiguous() {
        let source = read_source(
            "a_type_declared_differently_in_two_modules_is_ambiguous",
            &[
                (
                    "models.rs",
                    "pub struct R { pub a: String, pub b: String }\n\
                 pub async fn h(Json(body): Json<R>) {}",
                ),
                ("legacy.rs", "pub struct R { pub a: String }"),
            ],
        );
        assert!(
            !source.bodies.contains_key("h"),
            "two different types with one name is not a verdict"
        );
    }

    #[test]
    fn an_identical_type_in_two_modules_is_not_ambiguous() {
        let source = read_source(
            "an_identical_type_in_two_modules_is_not_ambiguous",
            &[
                (
                    "models.rs",
                    "pub struct R { pub a: String }\npub async fn h(Json(body): Json<R>) {}",
                ),
                ("reexport.rs", "pub struct R { pub a: String }"),
            ],
        );
        assert!(source.bodies.contains_key("h"), "identical is one type");
    }

    #[test]
    fn a_json_return_type_is_not_a_request_body() {
        let source = read_source(
            "a_json_return_type_is_not_a_request_body",
            &[(
                "main.rs",
                "pub async fn list(State(db): State<Db>) -> Json<Vec<Row>> { todo!() }",
            )],
        );
        assert!(!source.bodies.contains_key("list"));
    }

    #[test]
    fn a_file_that_does_not_parse_is_counted_not_ignored() {
        // The whole reason for a parser: a blind spot must be knowable.
        let source = read_source(
            "a_file_that_does_not_parse_is_counted_not_ignored",
            &[
                (
                    "good.rs",
                    "fn app() -> Router { Router::new().route(\"/x\", get(h)) }",
                ),
                ("bad.rs", "fn broken( { this is not rust"),
            ],
        );
        assert_eq!(source.files_parsed, 1);
        assert_eq!(source.files_unparsed, 1, "an unread file must be counted");
    }

    #[test]
    fn a_unit_only_enum_field_carries_its_values() {
        let source = read_source(
            "a_unit_only_enum_field_carries_its_values",
            &[(
                "models.rs",
                r#"
            #[serde(rename_all = "snake_case")]
            pub enum BlockedType { User, Sponsor }
            pub struct R { pub blocked_type: BlockedType, pub note: Option<String> }
            pub async fn h(Json(b): Json<R>) {}
            "#,
            )],
        );
        let fields = source.bodies.get("h").expect("resolved");
        assert_eq!(
            fields["blocked_type"].allowed.as_deref(),
            Some(["user".to_string(), "sponsor".to_string()].as_slice())
        );
        assert!(!fields["note"].required);
    }

    #[test]
    fn a_data_carrying_variant_abstains() {
        let source = read_source(
            "a_data_carrying_variant_abstains",
            &[(
                "models.rs",
                "pub enum T { User(Uuid), Everyone }\n\
             pub struct R { pub t: T }\n\
             pub async fn h(Json(b): Json<R>) {}",
            )],
        );
        assert_eq!(source.bodies.get("h").expect("resolved")["t"].allowed, None);
    }
}

/// Value guards found in a handler body, by the field they constrain.
///
/// A Rust type carries no value range: `rating: i8` says nothing, and the
/// constraint that actually rejects the request is two lines into the handler.
/// Over a parse these are exact expressions rather than a line that looked
/// right, so `matches!(body.rating, -1 | 0 | 1)` is read as the closed set it
/// is, and a guard whose alternatives are not literals is left alone.
#[derive(Default)]
struct Guards {
    /// field -> the values an explicit guard accepts.
    allowed: BTreeMap<String, Vec<String>>,
    /// field -> the bounds an explicit range guard accepts.
    ranges: BTreeMap<String, (Option<f64>, Option<f64>)>,
}

impl<'ast> syn::visit::Visit<'ast> for Guards {
    fn visit_expr(&mut self, expr: &'ast Expr) {
        match expr {
            // `matches!(body.rating, -1 | 0 | 1)`
            Expr::Macro(node) if node.mac.path.is_ident("matches") => {
                let tokens = node.mac.tokens.to_string();
                if let Some((scrutinee, arms)) = tokens.split_once(',') {
                    if let (Some(field), Some(values)) =
                        (mentioned_field(scrutinee), literal_list(arms, '|'))
                    {
                        self.allowed.entry(field).or_insert(values);
                    }
                }
            }
            // `[-1, 0, 1].contains(&body.rating)` and `(1..=5).contains(..)`
            Expr::MethodCall(call) if call.method == "contains" => {
                let Some(field) = call
                    .args
                    .first()
                    .and_then(|arg| mentioned_field(&expr_text(arg)))
                else {
                    syn::visit::visit_expr(self, expr);
                    return;
                };
                match unwrap_paren(&call.receiver) {
                    Expr::Array(array) => {
                        let items = array
                            .elems
                            .iter()
                            .map(expr_text)
                            .collect::<Vec<_>>()
                            .join(",");
                        if let Some(values) = literal_list(&items, ',') {
                            self.allowed.entry(field).or_insert(values);
                        }
                    }
                    Expr::Range(range) => {
                        let low = range.start.as_ref().and_then(|e| numeric(e));
                        let inclusive = matches!(range.limits, syn::RangeLimits::Closed(_));
                        let high = range.end.as_ref().and_then(|e| numeric(e)).map(|high| {
                            if inclusive {
                                high
                            } else {
                                high - 1.0
                            }
                        });
                        if low.is_some() || high.is_some() {
                            self.ranges.entry(field).or_insert((low, high));
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        syn::visit::visit_expr(self, expr);
    }
}

fn unwrap_paren(expr: &Expr) -> &Expr {
    match expr {
        Expr::Paren(inner) => unwrap_paren(&inner.expr),
        Expr::Reference(inner) => unwrap_paren(&inner.expr),
        other => other,
    }
}

/// The last identifier of a field access, which is the field a guard is about.
fn mentioned_field(text: &str) -> Option<String> {
    let cleaned: String = text
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '.')
        .collect();
    let last = cleaned.rsplit('.').next()?.trim().to_string();
    (!last.is_empty() && last.chars().next()?.is_alphabetic()).then_some(last)
}

fn expr_text(expr: &Expr) -> String {
    match expr {
        Expr::Lit(lit) => match &lit.lit {
            Lit::Str(text) => format!("\"{}\"", text.value()),
            Lit::Int(value) => value.to_string(),
            Lit::Float(value) => value.to_string(),
            other => format!("{other:?}"),
        },
        Expr::Unary(unary) => format!("-{}", expr_text(&unary.expr)),
        Expr::Reference(inner) => expr_text(&inner.expr),
        Expr::Field(field) => match &field.member {
            syn::Member::Named(name) => format!(".{name}"),
            syn::Member::Unnamed(_) => String::new(),
        },
        Expr::MethodCall(call) => expr_text(&call.receiver),
        Expr::Path(path) => path
            .path
            .segments
            .last()
            .map(|segment| segment.ident.to_string())
            .unwrap_or_default(),
        _ => String::new(),
    }
}

fn numeric(expr: &Expr) -> Option<f64> {
    expr_text(expr).replace(' ', "").parse().ok()
}

/// The literal items of a separated list, or None if any item is computed: a
/// list with one non-literal element states no closed set.
fn literal_list(text: &str, separator: char) -> Option<Vec<String>> {
    let mut values = Vec::new();
    for part in text.split(separator) {
        let item = part.trim().replace(' ', "");
        if item.is_empty() || item == "_" {
            return None;
        }
        let unquoted = item.trim_matches('"').to_string();
        if unquoted == item && item.parse::<f64>().is_err() {
            return None;
        }
        values.push(unquoted);
    }
    (values.len() > 1).then_some(values)
}
