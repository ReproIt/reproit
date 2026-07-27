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
use super::rust_router::{attribute_route, routes_of_fn, Crate, Route, MAX_ROUTES};
use super::rust_types::{json_body_type, struct_fields, unit_variants, Guards};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use syn::Item;

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

pub(super) fn read(root: &Path) -> RustSource {
    let mut source = RustSource::default();
    let mut krate = Crate::default();
    let mut files = Vec::new();
    for file in super::extract::family_sources(root, super::extract::Family::Rust) {
        // Not decodable or not openable is not the same as not declaring
        // anything, and it used to be indistinguishable.
        let Ok(text) = std::fs::read_to_string(&file) else {
            source.files_unparsed += 1;
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
                    .insert(format!("{module}::{name}"), (*function.block).clone());
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
            // An attribute route belongs to no router expression, so it is a
            // root UNLESS a `.mount()` claimed its handler and already emitted
            // it at a prefix. Emitting both invents a path nobody serves.
            if name.is_none() && handler.as_deref().is_some_and(|h| mounted.contains(h)) {
                continue;
            }
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
    fn a_conditionally_nested_router_carries_its_prefix_and_leaves_no_twin() {
        // hey's exact shape. Reading the router but not the mount emitted
        // `/seed` instead of `/dev/seed`: a path the service does not serve,
        // which is worse than not reading it at all.
        let source = read_source(
            "conditional_nest",
            &[(
                "main.rs",
                r#"async fn main() {
                    let dev_routes = if dev {
                        Some(Router::new().route("/seed", post(seed)))
                    } else {
                        None
                    };
                    let mut app = Router::new().route("/health/live", get(live));
                    if let Some(dev) = dev_routes {
                        app = app.nest("/dev", dev);
                    }
                    axum::serve(listener, app).await.unwrap();
                }"#,
            )],
        );
        let paths: Vec<&String> = source.routes.keys().collect();
        assert!(source.routes.contains_key("/health/live"), "{paths:?}");
        assert!(source.routes.contains_key("/dev/seed"), "{paths:?}");
        assert!(
            !source.routes.contains_key("/seed"),
            "the unprefixed twin is a path nobody serves: {paths:?}"
        );
    }

    #[test]
    fn a_warp_filter_chain_states_its_path_and_verb() {
        let source = read_source(
            "warp_filters",
            &[(
                "main.rs",
                r#"async fn main() {
                    let status = warp::path!("status").and(warp::get()).map(|| "ok");
                    let items = warp::path!("api" / "items").and(warp::post()).map(|| "made");
                    let routes = status.or(items);
                    warp::serve(routes).run(([127, 0, 0, 1], 8080)).await;
                }"#,
            )],
        );
        let paths: Vec<&String> = source.routes.keys().collect();
        assert!(source.routes.contains_key("/status"), "{paths:?}");
        assert!(source.routes.contains_key("/api/items"), "{paths:?}");
        assert_eq!(
            source.routes["/api/items"]
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec!["post"]
        );
    }

    #[test]
    fn a_warp_path_with_an_unnamed_parameter_is_left_unread() {
        // warp parameters are positional, so there is no name for the template.
        // Inventing one produces a path that matches no declared operation,
        // which reads as a route the service does not serve.
        let source = read_source(
            "warp_param",
            &[(
                "main.rs",
                r#"async fn main() {
                    let one = warp::path!("items" / u32).and(warp::get()).map(|| "x");
                    warp::serve(one).run(([127, 0, 0, 1], 8080)).await;
                }"#,
            )],
        );
        assert!(
            source.routes.is_empty(),
            "an unnamed parameter must not be guessed: {:?}",
            source.routes.keys()
        );
    }

    #[test]
    fn a_router_wrapped_in_some_is_still_a_router() {
        // hey gates seven dev-only routes behind
        // `if !cfg.is_production() { Some(Router::new()...) }`. `Some` is a
        // wrapper, not a router builder, and not seeing through it made those
        // routes indistinguishable from routes the service does not have.
        let source = read_source(
            "some_wrapper",
            &[(
                "main.rs",
                r#"fn app() -> Option<Router> {
                    if dev {
                        Some(Router::new().route("/seed", post(seed)))
                    } else {
                        None
                    }
                }"#,
            )],
        );
        assert!(
            source.routes.contains_key("/seed"),
            "{:?}",
            source.routes.keys()
        );
    }

    #[test]
    fn an_actix_app_built_in_a_closure_with_a_scope_resolves() {
        let source = read_source(
            "actix_entry",
            &[(
                "main.rs",
                r#"async fn main() -> std::io::Result<()> {
                    HttpServer::new(|| {
                        App::new()
                            .route("/status", web::get().to(status))
                            .service(web::scope("/api").route("/items", web::post().to(create)))
                    })
                    .bind(("127.0.0.1", 8080))?
                    .run()
                    .await
                }"#,
            )],
        );
        let paths: Vec<&String> = source.routes.keys().collect();
        assert!(source.routes.contains_key("/status"), "{paths:?}");
        assert!(
            source.routes.contains_key("/api/items"),
            "a scoped service must carry its prefix: {paths:?}"
        );
        assert!(
            !source.routes.contains_key("/items"),
            "no unprefixed twin: {paths:?}"
        );
    }

    #[test]
    fn a_rocket_mount_prefixes_the_handlers_it_names() {
        // The path lives on the handler's attribute and the prefix on the
        // mount, so reading only the attribute reported every rocket route one
        // prefix short: a declared `/api/status` matched nothing in source,
        // which is the direction that advises deleting a live operation.
        let source = read_source(
            "rocket_mount",
            &[(
                "main.rs",
                r#"#[get("/status")]
                fn status() -> &'static str { "ok" }

                #[post("/items")]
                fn create() -> &'static str { "made" }

                fn rocket() -> Rocket<Build> {
                    rocket::build().mount("/api", routes![status, create])
                }"#,
            )],
        );
        let paths: Vec<&String> = source.routes.keys().collect();
        assert!(source.routes.contains_key("/api/status"), "{paths:?}");
        assert!(source.routes.contains_key("/api/items"), "{paths:?}");
        assert!(
            !source.routes.contains_key("/status") && !source.routes.contains_key("/items"),
            "a mounted handler must not also surface unprefixed: {paths:?}"
        );
    }

    #[test]
    fn a_router_longer_than_the_recursion_budget_keeps_every_route() {
        // The chain walk used to recurse per `.route()`, so a router with more
        // links than MAX_ROUTER_DEPTH returned only its tail. hey declares 24
        // routes in one chain and the reader silently dropped the first 12,
        // which reads downstream as a service that does not serve them.
        let routes: String = (0..40)
            .map(|index| format!("        .route(\"/r{index}\", get(h{index}))\n"))
            .collect();
        let source = read_source(
            "long_chain",
            &[(
                "main.rs",
                &format!("fn app() -> Router {{\n    Router::new()\n{routes}    }}\n"),
            )],
        );
        assert_eq!(source.routes.len(), 40, "{:?}", source.routes.keys());
        assert!(
            source.routes.contains_key("/r0"),
            "the FIRST link must survive"
        );
        assert!(source.routes.contains_key("/r39"));
    }

    #[test]
    fn a_router_bound_in_a_block_with_no_tail_is_still_the_service() {
        // Every test in this file used `fn app() -> Router`, where the router
        // is the function's value. A real binary binds it and hands it to
        // `serve`; `main` returns `()`, so the block's value is nothing and
        // the whole service was computed and then discarded.
        let source = read_source(
            "main_binding",
            &[(
                "main.rs",
                r#"async fn main() {
                    let app = Router::new()
                        .route("/health", get(health))
                        .route("/items", post(create));
                    axum::serve(listener, app).await.unwrap();
                }"#,
            )],
        );
        let paths: Vec<&String> = source.routes.keys().collect();
        assert!(source.routes.contains_key("/health"), "{paths:?}");
        assert!(source.routes.contains_key("/items"), "{paths:?}");
    }

    #[test]
    fn a_merged_router_contributes_at_the_same_level() {
        let source = read_source(
            "merge",
            &[(
                "main.rs",
                r#"fn app() -> Router {
                    let ops = Router::new().route("/metrics", get(metrics));
                    Router::new().route("/health", get(health)).merge(ops)
                }"#,
            )],
        );
        let paths: Vec<&String> = source.routes.keys().collect();
        assert!(source.routes.contains_key("/metrics"), "{paths:?}");
        assert!(source.routes.contains_key("/health"), "{paths:?}");
        assert_eq!(source.routes.len(), 2, "no duplicate twin: {paths:?}");
    }

    #[test]
    fn two_modules_exporting_routes_both_resolve() {
        // Every real axum workspace has several `pub fn routes()`. Keyed by
        // bare name, the second overwrote the first and one whole module's
        // routes came back as "not served by the source".
        let source = read_source(
            "same_named_routes_fns",
            &[
                (
                    "main.rs",
                    r#"fn app() -> Router {
                        Router::new()
                            .nest("/api/v1", users::routes())
                            .nest("/api/v1", posts::routes())
                    }"#,
                ),
                (
                    "users.rs",
                    r#"pub fn routes() -> Router { Router::new().route("/users", get(list)) }"#,
                ),
                (
                    "posts.rs",
                    r#"pub fn routes() -> Router { Router::new().route("/posts", post(create)) }"#,
                ),
            ],
        );
        let paths: Vec<&String> = source.routes.keys().collect();
        assert!(source.routes.contains_key("/api/v1/users"), "{paths:?}");
        assert!(source.routes.contains_key("/api/v1/posts"), "{paths:?}");
        assert!(
            !source.routes.contains_key("/posts"),
            "a mounted router must not also surface at its local path: {paths:?}"
        );
    }

    #[test]
    fn an_ambiguous_bare_call_resolves_to_nothing_rather_than_to_a_guess() {
        // `routes()` written bare with two candidates names neither. Picking
        // one would mount a real router under the wrong prefix, which reads
        // downstream as a route the service does not serve.
        let source = read_source(
            "ambiguous_bare_call",
            &[
                (
                    "main.rs",
                    r#"fn app() -> Router { Router::new().nest("/api", routes()) }"#,
                ),
                (
                    "users.rs",
                    r#"pub fn routes() -> Router { Router::new().route("/users", get(list)) }"#,
                ),
                (
                    "posts.rs",
                    r#"pub fn routes() -> Router { Router::new().route("/posts", post(create)) }"#,
                ),
            ],
        );
        let paths: Vec<&String> = source.routes.keys().collect();
        assert!(
            !source.routes.contains_key("/api/users") && !source.routes.contains_key("/api/posts"),
            "an ambiguous name must not be guessed: {paths:?}"
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
    fn serde_default_makes_a_non_option_field_optional() {
        // hey's `RegisterDeviceRequest.platform` is a bare `String` with
        // `#[serde(default)]`: omitting it yields "" rather than a rejection.
        // Reading required-ness from `Option` alone called a correct schema
        // wrong and stated a rejection that does not happen.
        let source = read_source(
            "serde_default",
            &[(
                "models.rs",
                r#"
            pub struct R {
                pub token: String,
                #[serde(default)]
                pub platform: String,
                #[serde(default = "default_kind")]
                pub kind: String,
                #[serde(skip_deserializing)]
                pub server_set: String,
            }
            pub async fn h(Json(b): Json<R>) {}
            "#,
            )],
        );
        let fields = source.bodies.get("h").expect("resolved");
        assert!(fields["token"].required, "a bare field is still required");
        assert!(!fields["platform"].required, "#[serde(default)] opts out");
        assert!(!fields["kind"].required, "default = \"path\" opts out too");
        assert!(
            !fields["server_set"].required,
            "a field never read from input cannot be required"
        );
        assert!(
            fields.contains_key("server_set"),
            "it stays in the set: declaring it is ignored, not wrong"
        );
    }

    #[test]
    fn container_default_opts_every_field_out() {
        let source = read_source(
            "container_default",
            &[(
                "models.rs",
                r#"
            #[serde(default)]
            pub struct R { pub a: String, pub b: String }
            pub async fn h(Json(b): Json<R>) {}
            "#,
            )],
        );
        let fields = source.bodies.get("h").expect("resolved");
        assert!(!fields["a"].required && !fields["b"].required, "{fields:?}");
    }

    #[test]
    fn container_rename_all_gives_the_wire_name() {
        // Comparing the Rust name against a renamed wire name reports a field
        // that is present as one the handler does not have.
        let source = read_source(
            "rename_all_fields",
            &[(
                "models.rs",
                r#"
            #[serde(rename_all = "camelCase")]
            pub struct R { pub blocked_type: String, #[serde(rename = "id")] pub blocked_id: String }
            pub async fn h(Json(b): Json<R>) {}
            "#,
            )],
        );
        let fields = source.bodies.get("h").expect("resolved");
        assert!(fields.contains_key("blockedType"), "{:?}", fields.keys());
        assert!(
            !fields.contains_key("blocked_type"),
            "the Rust name is not the wire name: {:?}",
            fields.keys()
        );
        assert!(
            fields.contains_key("id"),
            "an explicit rename beats rename_all: {:?}",
            fields.keys()
        );
    }

    #[test]
    fn a_flattened_field_makes_the_whole_type_abstain() {
        // The flattened type's fields are on the wire at this level and cannot
        // be enumerated from here. A partial set would report every one of them
        // as absent from the handler's body type.
        let source = read_source(
            "flatten",
            &[(
                "models.rs",
                r#"
            pub struct R { pub a: String, #[serde(flatten)] pub extra: Meta }
            pub async fn h(Json(b): Json<R>) {}
            "#,
            )],
        );
        assert!(
            !source.bodies.contains_key("h"),
            "an unenumerable shape must abstain: {:?}",
            source.bodies
        );
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
