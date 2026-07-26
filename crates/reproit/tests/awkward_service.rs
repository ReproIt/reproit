//! A deliberately awkward service, kept because real ones are.
//!
//! Every bug the source extractor has shipped came from a real service's
//! awkwardness rather than from anything a clean fixture would show: a router
//! built conditionally, a route rustfmt wrapped across lines, two modules
//! declaring the same type name, a sibling service in the same repo, a bench
//! target with stale endpoints. None of those are exotic; they are what a
//! service looks like after a year.
//!
//! So they live here together. A clean fixture proves the happy path works. This
//! one proves the awkward paths do not silently produce a confident wrong
//! answer, which is the failure mode that actually costs a user their afternoon.

use std::path::Path;
use std::process::Command;

fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("create dirs");
    std::fs::write(path, contents).expect("write fixture file");
}

/// The service under test, plus everything around it that has ever confused the
/// extractor.
fn awkward_repo(name: &str) -> std::path::PathBuf {
    // Per-test directory: these run in parallel and a shared one clobbers.
    let root = std::env::temp_dir().join(format!("reproit-awkward-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);

    // A workspace root with no dependencies of its own, and a SIBLING service:
    // scanning the whole root would mix their routes together.
    write(
        &root,
        "Cargo.toml",
        "[workspace]\nresolver = \"2\"\nmembers = [\"api\", \"portal\"]\n",
    );
    write(
        &root,
        "portal/Cargo.toml",
        "[package]\nname = \"portal\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
         [dependencies]\naxum = \"0.8\"\n",
    );
    write(
        &root,
        "portal/src/main.rs",
        "use axum::{routing::get, Router};\n\
         fn app() -> Router { Router::new().route(\"/dashboard.css\", get(css)) }\n",
    );

    write(
        &root,
        "api/Cargo.toml",
        "[package]\nname = \"api\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
         [dependencies]\naxum = \"0.8\"\n",
    );

    // A bench target declaring an endpoint the service does not serve.
    write(
        &root,
        "api/benches/load.rs",
        "use axum::{routing::get, Router};\n\
         fn bench_app() -> Router { Router::new().route(\"/bench-only\", get(h)) }\n",
    );

    // The router: built conditionally into a local, with rustfmt-wrapped routes.
    write(
        &root,
        "api/src/main.rs",
        r#"
use axum::{routing::{get, post}, Router};
mod models;

fn app(cfg: &Cfg) -> Router {
    let v1 = match cfg.plane {
        Plane::Regional => Router::new()
            .route(
                "/presence/update",
                post(models::update_presence),
            )
            .route("/nearby", get(models::nearby)),
        Plane::Global => Router::new().route("/profile", get(models::profile)),
    };
    Router::new()
        .route("/healthz", get(health))
        .nest("/v1", v1)
}
"#,
    );

    // The request type, plus a DIFFERENT type of the same name in another
    // module: Rust namespaces these, the extractor keys them by bare name.
    write(
        &root,
        "api/src/models.rs",
        r#"
use axum::Json;

#[derive(Deserialize)]
pub struct PresenceUpdate {
    pub visible: bool,
    pub lat: f64,
    pub interests: Option<Vec<String>>,
    pub age: Option<i64>,
}

pub async fn update_presence(Json(body): Json<PresenceUpdate>) -> impl IntoResponse {}
"#,
    );
    write(
        &root,
        "api/src/legacy.rs",
        "pub struct PresenceUpdate {\n    pub visible: bool,\n}\n",
    );
    root
}

fn reproit(root: &Path, args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_reproit"))
        .args(args)
        .current_dir(root)
        .output()
        .expect("run reproit");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn a_sibling_service_makes_the_contract_check_abstain_rather_than_guess() {
    let root = awkward_repo("sibling");
    std::fs::write(
        root.join("api.json"),
        r#"{"openapi":"3.0.3","info":{"title":"a","version":"1"},
            "servers":[{"url":"http://127.0.0.1:9/x"}],
            "paths":{"/v1/nearby":{"get":{"operationId":"nearby","responses":{}}}}}"#,
    )
    .expect("schema");
    std::fs::write(
        root.join("reproit.yaml"),
        "backend:\n  enabled: true\n  schemas: [api.json]\n  target: http://127.0.0.1:9\n",
    )
    .expect("config");

    let report = reproit(&root, &["doctor"]);
    assert!(
        report.contains("services under this root"),
        "a multi-service root must abstain, not compare against a sibling: {report}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn learn_reads_wrapped_routes_a_conditional_router_and_skips_bench_targets() {
    let root = awkward_repo("learn");
    let derived = reproit(&root.join("api"), &["init", "--learn", "--yes", "--force"]);
    assert!(derived.contains("derived"), "{derived}");
    let schema = std::fs::read_to_string(root.join("api/openapi.yaml")).expect("draft");

    // rustfmt wrapped this one across four lines.
    assert!(
        schema.contains("/v1/presence/update"),
        "a wrapped .route() must still be read, and carry its mount: {schema}"
    );
    // Both arms of the conditional router.
    assert!(schema.contains("/v1/nearby"), "{schema}");
    assert!(schema.contains("/v1/profile"), "{schema}");
    // A route on the root router keeps its own path.
    assert!(schema.contains("/healthz"), "{schema}");
    // The unprefixed local path must not also appear.
    assert!(
        !schema.contains("\"/nearby\""),
        "a mounted route must not also be emitted unprefixed: {schema}"
    );
    // A bench target is not the served surface.
    assert!(
        !schema.contains("bench-only"),
        "a non-shipping Cargo target must not become declared surface: {schema}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_type_name_declared_in_two_modules_never_produces_a_field_verdict() {
    let root = awkward_repo("ambiguous");
    // A schema naming a field only the REAL type has. The legacy same-named
    // struct lacks it, so picking a winner would report a live field as absent.
    std::fs::write(
        root.join("api/api.json"),
        r#"{"openapi":"3.0.3","info":{"title":"a","version":"1"},
            "servers":[{"url":"http://127.0.0.1:9"}],
            "paths":{"/v1/presence/update":{"post":{"operationId":"p",
              "requestBody":{"content":{"application/json":{"schema":{"type":"object",
                "properties":{"age":{"type":"integer"}}}}}},
              "responses":{}}}}}"#,
    )
    .expect("schema");
    std::fs::write(
        root.join("api/reproit.yaml"),
        "backend:\n  enabled: true\n  schemas: [api.json]\n  target: http://127.0.0.1:9\n",
    )
    .expect("config");

    let report = reproit(&root.join("api"), &["doctor"]);
    assert!(
        !report.contains("no `age`"),
        "an ambiguous type must abstain, never claim a live field is missing: {report}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_unmatched_route_is_reported_as_unconfirmed_not_as_a_deletion() {
    // The extractor matches patterns; it cannot know what it failed to match.
    // So "no route found" must never be phrased as "this does not exist".
    let root = awkward_repo("unmatched");
    std::fs::write(
        root.join("api/api.json"),
        r#"{"openapi":"3.0.3","info":{"title":"a","version":"1"},
            "servers":[{"url":"http://127.0.0.1:9"}],
            "paths":{"/v1/not-in-source":{"get":{"operationId":"x","responses":{}}}}}"#,
    )
    .expect("schema");
    std::fs::write(
        root.join("api/reproit.yaml"),
        "backend:\n  enabled: true\n  schemas: [api.json]\n  target: http://127.0.0.1:9\n",
    )
    .expect("config");

    let report = reproit(&root.join("api"), &["doctor"]);
    assert!(report.contains("no route matched in source"), "{report}");
    assert!(
        !report.contains("delete the operation"),
        "an absence found by a pattern must not advise deletion: {report}"
    );
    let _ = std::fs::remove_dir_all(&root);
}
