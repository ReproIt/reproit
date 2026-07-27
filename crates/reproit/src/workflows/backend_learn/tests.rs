use super::extract::{derive, family_for, normalize_path, path_params};
use super::{emit, enrich};
use std::collections::BTreeMap;
use std::io::{Read as _, Write as _};
use std::path::PathBuf;

fn project(files: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "reproit-learn-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    for (name, content) in files {
        let path = dir.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    dir
}

fn routes(framework: &str, file: &str, content: &str) -> Vec<(String, Vec<&'static str>)> {
    let dir = project(&[(file, content)]);
    let derived = derive(&dir, framework).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    derived
        .routes
        .into_iter()
        .map(|(path, methods)| (path, methods.into_iter().collect()))
        .collect()
}

/// (framework, file, source snippet, expected path -> methods).
type ExtractionCase = (
    &'static str,
    &'static str,
    &'static str,
    &'static [(&'static str, &'static [&'static str])],
);

#[test]
fn extraction_table_covers_every_framework() {
    // One fixture snippet per framework family and dialect, normalized params.
    let cases: &[ExtractionCase] = &[
        (
            "express",
            "server.js",
            "const app = express();\napp.get('/orders', list);\napp.post('/orders', create);\n\
             app.get('/orders/:id', show);\n",
            &[("/orders", &["get", "post"]), ("/orders/{id}", &["get"])],
        ),
        (
            "koa",
            "app.js",
            "router.get('/items', list);\nrouter.delete('/items/:itemId', remove);\n",
            &[("/items", &["get"]), ("/items/{itemId}", &["delete"])],
        ),
        (
            "fastify",
            "routes.js",
            "fastify.route({\n  method: 'PUT',\n  url: '/users/:id',\n  handler,\n});\n\
             fastify.get('/users', list);\n",
            &[("/users", &["get"]), ("/users/{id}", &["put"])],
        ),
        (
            "fastapi",
            "main.py",
            "@app.get(\"/orders\")\ndef list_orders(): ...\n\
             @app.post(\"/orders\")\ndef create(): ...\n\
             @router.get(\"/orders/{order_id}\")\ndef show(order_id: int): ...\n",
            &[
                ("/orders", &["get", "post"]),
                ("/orders/{order_id}", &["get"]),
            ],
        ),
        (
            "flask",
            "app.py",
            "@app.route(\"/things\", methods=[\"GET\", \"POST\"])\ndef things(): ...\n\
             @app.route(\"/things/<int:thing_id>\")\ndef show(thing_id): ...\n",
            &[
                ("/things", &["get", "post"]),
                ("/things/{thing_id}", &["get"]),
            ],
        ),
        (
            "django",
            "app/urls.py",
            "urlpatterns = [\n    path(\"orders/\", views.orders),\n    \
             path(\"orders/<int:pk>/\", views.detail),\n]\n",
            &[("/orders", &["get"]), ("/orders/{pk}", &["get"])],
        ),
        (
            "axum",
            "src/main.rs",
            "fn app() -> Router {\n    Router::new()\n        .route(\"/orders\", \
             post(create).get(list))\n        .route(\"/orders/{id}\", get(show))\n        \
             .route(\"/health\", get(health))\n}\n",
            &[
                ("/health", &["get"]),
                ("/orders", &["get", "post"]),
                ("/orders/{id}", &["get"]),
            ],
        ),
        (
            "actix-web",
            "src/main.rs",
            "#[get(\"/status\")]\nasync fn status() {}\n\
             fn app() -> App { App::new().route(\"/orders/{id}\", web::patch().to(update)) }\n",
            &[("/orders/{id}", &["patch"]), ("/status", &["get"])],
        ),
        (
            "gin",
            "main.go",
            "r.GET(\"/ping\", ping)\nr.POST(\"/orders\", create)\n\
             r.GET(\"/orders/:id\", show)\n",
            &[
                ("/orders", &["post"]),
                ("/orders/{id}", &["get"]),
                ("/ping", &["get"]),
            ],
        ),
        (
            "echo",
            "main.go",
            "e.GET(\"/users/:id\", getUser)\ne.PUT(\"/users/:id\", updateUser)\n",
            &[("/users/{id}", &["get", "put"])],
        ),
        (
            "chi",
            "main.go",
            "r.Get(\"/articles/{articleID}\", getArticle)\n\
             r.Delete(\"/articles/{articleID}\", rm)\n",
            &[("/articles/{articleID}", &["delete", "get"])],
        ),
        (
            "fiber",
            "main.go",
            "app.Get(\"/api/list\", list)\napp.Post(\"/api/items\", create)\n",
            &[("/api/items", &["post"]), ("/api/list", &["get"])],
        ),
        (
            "net/http",
            "main.go",
            "mux.HandleFunc(\"GET /health\", health)\nmux.HandleFunc(\"POST /orders\", create)\n",
            &[("/health", &["get"]), ("/orders", &["post"])],
        ),
        (
            "rails",
            "config/routes.rb",
            "Rails.application.routes.draw do\n  get '/status', to: 'status#show'\n  \
             resources :orders\nend\n",
            &[
                ("/orders", &["get", "post"]),
                ("/orders/new", &["get"]),
                ("/orders/{id}", &["delete", "get", "patch", "put"]),
                ("/orders/{id}/edit", &["get"]),
                ("/status", &["get"]),
            ],
        ),
        (
            "spring",
            "src/OrderController.java",
            "@RequestMapping(\"/api/orders\")\npublic class OrderController {\n  \
             @GetMapping\n  public List<Order> list() {}\n  \
             @PostMapping\n  public Order create() {}\n  \
             @GetMapping(\"/{id}\")\n  public Order show() {}\n}\n",
            &[
                ("/api/orders", &["get", "post"]),
                ("/api/orders/{id}", &["get"]),
            ],
        ),
        (
            "laravel",
            "routes/api.php",
            "<?php\n\
             Route::get('/projects', [ProjectController::class, 'index']);\n\
             Route::post('/projects', [ProjectController::class, 'store']);\n\
             Route::get('/projects/{project}', [ProjectController::class, 'show']);\n",
            &[
                ("/projects", &["get", "post"]),
                ("/projects/{project}", &["get"]),
            ],
        ),
    ];
    for (framework, file, content, expected) in cases {
        let found = routes(framework, file, content);
        let expected: Vec<(String, Vec<&str>)> = expected
            .iter()
            .map(|(path, methods)| (path.to_string(), methods.to_vec()))
            .collect();
        assert_eq!(found, expected, "framework {framework}");
    }
}

/// A whole program per family, in the shape a service is actually written in.
///
/// The table above feeds each reader a SNIPPET, which is how a total extractor
/// failure shipped: every axum case declared its router as `fn app() -> Router`,
/// where the router is the function's value. A real binary binds it in `main`
/// and hands it to `serve`, `main` returns `()`, and the reader that passed
/// nine unit tests extracted zero routes from it.
///
/// These fixtures are entry points, not fragments. A family that stops
/// extracting fails here even when its own unit tests still pass.
#[test]
fn every_family_extracts_from_an_entry_point_not_just_a_snippet() {
    let cases: &[ExtractionCase] = &[
        (
            "axum",
            "src/main.rs",
            "use axum::routing::{get, post};\nuse axum::Router;\n\n\
             async fn health() -> &'static str { \"ok\" }\n\
             async fn create() -> &'static str { \"made\" }\n\n\
             #[tokio::main]\nasync fn main() {\n    let app = Router::new()\n        \
             .route(\"/health\", get(health))\n        .route(\"/items\", post(create));\n    \
             let listener = tokio::net::TcpListener::bind(\"0.0.0.0:3000\").await.unwrap();\n    \
             axum::serve(listener, app).await.unwrap();\n}\n",
            &[("/health", &["get"]), ("/items", &["post"])],
        ),
        (
            "gin",
            "main.go",
            "package main\n\nimport \"github.com/gin-gonic/gin\"\n\n\
             func main() {\n\tr := gin.Default()\n\tr.GET(\"/health\", health)\n\t\
             r.POST(\"/items\", create)\n\tr.Run(\":3000\")\n}\n",
            &[("/health", &["get"]), ("/items", &["post"])],
        ),
        (
            "express",
            "server.js",
            "const express = require('express');\nconst app = express();\n\
             app.get('/health', health);\napp.post('/items', create);\n\
             app.listen(3000);\n",
            &[("/health", &["get"]), ("/items", &["post"])],
        ),
        (
            "fastapi",
            "main.py",
            "from fastapi import FastAPI\n\napp = FastAPI()\n\n\
             @app.get(\"/health\")\nasync def health():\n    return {}\n\n\
             @app.post(\"/items\")\nasync def create():\n    return {}\n",
            &[("/health", &["get"]), ("/items", &["post"])],
        ),
        (
            "rails",
            "config/routes.rb",
            "Rails.application.routes.draw do\n  get '/health', to: 'health#show'\n  \
             post '/items', to: 'items#create'\nend\n",
            &[("/health", &["get"]), ("/items", &["post"])],
        ),
        (
            "laravel",
            "routes/api.php",
            "<?php\n\nuse Illuminate\\Support\\Facades\\Route;\n\n\
             Route::get('/health', [HealthController::class, 'show']);\n\
             Route::post('/items', [ItemController::class, 'store']);\n",
            &[("/health", &["get"]), ("/items", &["post"])],
        ),
        (
            "aspnet",
            "Program.cs",
            "var builder = WebApplication.CreateBuilder(args);\n\
             var app = builder.Build();\n\
             app.MapGet(\"/health\", () => \"ok\");\n\
             app.MapPost(\"/items\", () => Results.Ok());\n\
             app.Run();\n",
            &[("/health", &["get"]), ("/items", &["post"])],
        ),
        (
            "nestjs",
            "src/items.controller.ts",
            "import { Controller, Get, Post } from '@nestjs/common';\n\
             @Controller()\n\
             export class ItemsController {\n\
             \x20 @Get('health')\n  health(): string { return 'ok'; }\n\
             \x20 @Post('items')\n  create(): string { return 'made'; }\n\
             }\n",
            &[("/health", &["get"]), ("/items", &["post"])],
        ),
        (
            "spring",
            "src/ItemController.java",
            "package com.example;\n\nimport org.springframework.web.bind.annotation.*;\n\n\
             @RestController\npublic class ItemController {\n    \
             @GetMapping(\"/health\")\n    public String health() { return \"ok\"; }\n\n    \
             @PostMapping(\"/items\")\n    public String create() { return \"made\"; }\n}\n",
            &[("/health", &["get"]), ("/items", &["post"])],
        ),
    ];
    for (framework, file, content, expected) in cases {
        let found = routes(framework, file, content);
        assert!(
            !found.is_empty(),
            "{framework} extracted NOTHING from an entry point; the reader is inert \
             for every real service in this family"
        );
        let expected: Vec<(String, Vec<&str>)> = expected
            .iter()
            .map(|(path, methods)| (path.to_string(), methods.to_vec()))
            .collect();
        assert_eq!(found, expected, "framework {framework}");
    }
}

#[test]
fn nested_router_mount_prefixes_are_resolved() {
    // Flask blueprint url_prefix applies to every route on that blueprint; a
    // route on the bare app is left unprefixed.
    let mut flask = routes(
        "flask",
        "app.py",
        "bp = Blueprint('users', __name__, url_prefix='/api/v1')\n\
         @bp.route('/users', methods=['GET','POST'])\n\
         def users(): pass\n\
         @bp.get('/users/<int:id>')\n\
         def one(id): pass\n\
         @app.get('/healthz')\n\
         def health(): pass\n",
    );
    flask.sort();
    assert_eq!(
        flask,
        vec![
            ("/api/v1/users".to_string(), vec!["get", "post"]),
            ("/api/v1/users/{id}".to_string(), vec!["get"]),
            ("/healthz".to_string(), vec!["get"]),
        ]
    );

    // FastAPI APIRouter(prefix=) composes with include_router(prefix=): the
    // include prefix wraps the constructor prefix.
    let mut fastapi = routes(
        "fastapi",
        "main.py",
        "router = APIRouter(prefix='/users')\n\
         @router.get('/{id}')\n\
         def one(id): ...\n\
         app.include_router(router, prefix='/api')\n",
    );
    fastapi.sort();
    assert_eq!(fastapi, vec![("/api/users/{id}".to_string(), vec!["get"])]);

    // Express Router() mounted with app.use('/prefix', router) prefixes its
    // routes; the app's own routes are unprefixed.
    let mut express = routes(
        "express",
        "server.js",
        "const router = express.Router();\n\
         router.get('/items', list);\n\
         router.post('/items/:id', edit);\n\
         app.use('/api', router);\n\
         app.get('/status', status);\n",
    );
    express.sort();
    assert_eq!(
        express,
        vec![
            ("/api/items".to_string(), vec!["get"]),
            ("/api/items/{id}".to_string(), vec!["post"]),
            ("/status".to_string(), vec!["get"]),
        ]
    );
}

#[test]
fn every_detectable_backend_framework_has_a_family_or_is_php_symfony() {
    // The backend_detect names --learn must route; symfony is the one
    // detectable framework without patterns yet (falls to the guided error).
    for name in [
        "axum",
        "actix-web",
        "rocket",
        "warp",
        "express",
        "fastify",
        "koa",
        "hapi",
        "fastapi",
        "django",
        "flask",
        "spring",
        "java",
        "rails",
        "sinatra",
        "laravel",
        "gin",
        "echo",
        "fiber",
        "chi",
        "net/http",
    ] {
        assert!(
            family_for(name).is_some(),
            "no extraction family for {name}"
        );
    }
    assert!(family_for("symfony").is_none());
}

#[test]
fn path_normalization_maps_every_param_style_to_openapi() {
    assert_eq!(normalize_path("/a/:id/b"), Some("/a/{id}/b".into()));
    assert_eq!(normalize_path("/a/<id>"), Some("/a/{id}".into()));
    assert_eq!(normalize_path("/a/<int:id>"), Some("/a/{id}".into()));
    assert_eq!(normalize_path("/a/{id:[0-9]+}"), Some("/a/{id}".into()));
    assert_eq!(normalize_path("orders/"), Some("/orders".into()));
    assert_eq!(normalize_path("/"), Some("/".into()));
    // A catch-all is a real part of the surface. Dropping the whole route lost
    // `/swagger/*any` and every static-file mount, which is an absence the
    // source does not support; OpenAPI has no wildcard, so it becomes a named
    // template parameter a generator can actually exercise.
    assert_eq!(normalize_path("/files/*path"), Some("/files/{path}".into()));
    assert_eq!(
        normalize_path("/static/*"),
        Some("/static/{wildcard}".into())
    );
    assert_eq!(normalize_path("/hello/мир"), Some("/hello/мир".into()));
    // Unconfident shapes are still rejected, not guessed.
    assert_eq!(normalize_path("http://x/a"), None);
    assert_eq!(normalize_path("/a b"), None);
    assert_eq!(normalize_path("/^orders$"), None);
    assert_eq!(path_params("/a/{id}/b/{name}"), vec!["id", "name"]);
}

#[test]
fn axum_current_catch_all_syntax_survives_a_whole_entry_point() {
    let axum = routes(
        "axum",
        "src/main.rs",
        "use axum::{routing::get, Router};\n\
         #[tokio::main]\n\
         async fn main() {\n\
         \x20   let app = Router::new()\n\
         \x20       .route(\"/{*rest}\", get(fallback))\n\
         \x20       .route(\"/static/{*path}\", get(static_file));\n\
         \x20   axum::serve(listener, app).await.unwrap();\n\
         }\n",
    );
    assert_eq!(
        axum,
        vec![
            ("/static/{path}".to_string(), vec!["get"]),
            ("/{rest}".to_string(), vec!["get"]),
        ]
    );
}

#[test]
fn optional_parameters_survive_whole_dotnet_and_fiber_entry_points() {
    let aspnet = routes(
        "aspnet",
        "Program.cs",
        "var builder = WebApplication.CreateBuilder(args);\n\
         var app = builder.Build();\n\
         app.MapGet(\"/catalog/{brandId?}\", () => Results.Ok());\n\
         app.Run();\n",
    );
    assert_eq!(
        aspnet,
        vec![("/catalog/{brandId}".to_string(), vec!["get"])]
    );

    let fiber = routes(
        "fiber",
        "main.go",
        "package main\n\
         func main() {\n\
         \tapp := fiber.New()\n\
         \tapp.Get(\"/geo/:ip?\", geo)\n\
         \tapp.Listen(\":3000\")\n\
         }\n",
    );
    assert_eq!(fiber, vec![("/geo/{ip}".to_string(), vec!["get"])]);
}

#[test]
fn fiber_suffixed_catch_all_survives_a_whole_entry_point() {
    let fiber = routes(
        "fiber",
        "main.go",
        "package main\n\
         func main() {\n\
         \tapp := fiber.New()\n\
         \tapp.Get(\"/web*\", spa)\n\
         \tapp.Listen(\":3000\")\n\
         }\n",
    );
    assert_eq!(fiber, vec![("/web{wildcard}".to_string(), vec!["get"])]);
}

#[test]
fn zero_derived_routes_fails_closed_without_writing_config() {
    let dir = project(&[
        ("Cargo.toml", "[dependencies]\naxum = \"0.8\"\n"),
        (
            "src/main.rs",
            "fn main() { println!(\"no routes here\"); }\n",
        ),
    ]);
    let ctx = crate::interface::cli::context::Ctx::default();
    let error = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(super::run(&ctx, &dir, None, false))
        .unwrap_err();
    assert!(error.to_string().contains("no routes could be derived"));
    assert!(!dir.join("reproit.yaml").exists());
    assert!(!dir.join("openapi.yaml").exists());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn draft_yaml_round_trips_through_the_schema_importer() {
    let dir = project(&[(
        "src/main.rs",
        "fn app() -> Router {\n    Router::new()\n        .route(\"/orders\", \
         post(create).get(list))\n        .route(\"/orders/{id}\", get(show))\n}\n",
    )]);
    let derived = derive(&dir, "axum").unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    let yaml = emit::draft_yaml("fixture", "axum", &derived, &BTreeMap::new()).unwrap();
    assert!(yaml.contains("x-reproit-derived: true"));
    assert!(yaml.starts_with("# DRAFT schema derived by `reproit init --learn`"));
    assert!(yaml.contains("operationId: get_orders_id"));
    // Path params are typed string; mutating routes get a free-form body.
    assert!(yaml.contains("in: path"));
    assert!(yaml.contains("requestBody"));
    // No responses claimed without live observation: no invented statuses.
    assert!(!yaml.contains("responses"));
    let document: serde_json::Value = serde_yaml::from_str(&yaml).unwrap();
    assert_eq!(
        crate::domain::backend::import_service_schema(&document).len(),
        3
    );
}

/// A one-shot HTTP/1.1 stub: accepts connections until dropped, answering each
/// with the given response bytes, and returns the requests it saw.
fn stub_server(
    response: &'static str,
    connections: usize,
) -> (String, std::thread::JoinHandle<Vec<String>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let handle = std::thread::spawn(move || {
        let mut seen = Vec::new();
        for _ in 0..connections {
            let Ok((mut stream, _)) = listener.accept() else {
                break;
            };
            let mut buffer = [0u8; 4096];
            let read = stream.read(&mut buffer).unwrap_or(0);
            seen.push(String::from_utf8_lossy(&buffer[..read]).into_owned());
            let _ = stream.write_all(response.as_bytes());
        }
        seen
    });
    (base, handle)
}

#[tokio::test]
async fn live_enrichment_records_status_shape_and_effects() {
    use base64::Engine as _;
    let events = serde_json::json!([{
        "sequence": 1, "traceId": "t", "spanId": "s", "operation": "health",
        "kind": "effect", "effect": "read", "resource": "inventory"
    }]);
    let trail = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(serde_json::to_vec(&events).unwrap());
    let body = r#"{"ok":true,"items":[{"id":1}],"note":null}"#;
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nx-reproit-events: {trail}\r\n\
         content-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let response: &'static str = Box::leak(response.into_boxed_str());
    let (base, handle) = stub_server(response, 1);
    let outcome = enrich::probe(&base, &["/health".to_string()]).await;
    let requests = handle.join().unwrap();
    assert!(requests[0].starts_with("GET /health HTTP/1.1"));
    assert!(requests[0].to_lowercase().contains("x-reproit-trace"));
    assert!(outcome.adapter);
    let observed = &outcome.observations["/health"];
    assert_eq!(observed.status, 200);
    assert_eq!(observed.effects, vec!["read(inventory)".to_string()]);
    let shape = observed.body.as_ref().unwrap();
    assert_eq!(shape["ok"], serde_json::json!(true));
    // The observation lands in the draft as a recorded response + comment.
    let dir = project(&[(
        "src/main.rs",
        "fn app() -> Router { Router::new().route(\"/health\", get(health)) }\n",
    )]);
    let derived = derive(&dir, "axum").unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    let yaml = emit::draft_yaml("fixture", "axum", &derived, &outcome.observations).unwrap();
    let note = "# observed live by --learn: HTTP 200; adapter effects: read(inventory)";
    assert!(yaml.contains(note));
    assert!(yaml.contains("\"200\":"));
    assert!(yaml.contains("type: boolean"));
    let document: serde_json::Value = serde_yaml::from_str(&yaml).unwrap();
    assert_eq!(
        crate::domain::backend::import_service_schema(&document).len(),
        1
    );
}

#[tokio::test]
async fn probe_bounds_cap_routes_and_survive_a_dead_target() {
    // More derived routes than the probe cap: only the cap is attempted.
    let paths: Vec<String> = (0..40).map(|index| format!("/r{index}")).collect();
    assert!(paths.len() > enrich::MAX_PROBED_ROUTES);
    // A closed port: every probe fails soft and nothing is recorded.
    let dead = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", listener.local_addr().unwrap())
    };
    let outcome = enrich::probe(&dead, &paths).await;
    assert!(outcome.attempted <= enrich::MAX_PROBED_ROUTES);
    assert!(outcome.observations.is_empty());
    assert!(!outcome.adapter);
}

#[test]
fn malformed_adapter_trails_note_nothing() {
    assert!(enrich::decode_effects("not base64url !!!").is_empty());
    use base64::Engine as _;
    let not_events = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b"{\"nope\":1}");
    assert!(enrich::decode_effects(&not_events).is_empty());
}

/// The report exists so it can be run on someone else's repo. If it writes
/// anything at all, that promise is void, so the guarantee is a test rather
/// than a comment.
#[test]
fn the_report_writes_nothing() {
    let dir = project(&[
        ("Cargo.toml", "[dependencies]\naxum = \"0.8\"\n"),
        (
            "src/main.rs",
            "async fn main() {\n    let app = Router::new().route(\"/health\", get(h));\n\
             \x20   axum::serve(listener, app).await.unwrap();\n}\n",
        ),
        // A schema already present is one of the two cases `--learn` refuses.
        (
            "openapi.yaml",
            "openapi: 3.1.0\ninfo: { title: t, version: \"1\" }\npaths: {}\n",
        ),
    ]);
    let before = snapshot(&dir);
    let ctx = crate::interface::cli::context::Ctx::default();
    let code = super::surface(&ctx, &dir);
    let after = snapshot(&dir);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        code.is_ok(),
        "the report must not fail on a repo with a schema"
    );
    assert_eq!(before, after, "the report wrote to the project");
}

#[test]
fn surface_descends_into_one_nested_service() {
    let dir = project(&[
        (
            "redis/Cargo.toml",
            "[package]\nname = \"redis-service\"\nversion = \"0.1.0\"\n\
             edition = \"2021\"\n[dependencies]\naxum = \"0.8\"\n",
        ),
        (
            "redis/src/main.rs",
            "async fn main() {\n    let app = Router::new().route(\"/health\", get(health));\n\
             \x20   axum::serve(listener, app).await.unwrap();\n}\n",
        ),
    ]);
    let before = snapshot(&dir);
    let ctx = crate::interface::cli::context::Ctx::default();
    let result = super::surface(&ctx, &dir);
    let after = snapshot(&dir);
    assert!(
        result.is_ok(),
        "one nested service must be discovered: {result:?}"
    );
    assert_eq!(before, after, "surface changed the nested service");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn surface_discovers_services_under_non_shipping_directory_names() {
    let manifest = "[package]\nname = \"service\"\nversion = \"0.1.0\"\n\
                    edition = \"2021\"\n[dependencies]\naxum = \"0.8\"\n";
    let source = "async fn main() {\n\
                  \x20   let app = Router::new().route(\"/health\", get(health));\n\
                  \x20   axum::serve(listener, app).await.unwrap();\n}\n";
    let dir = project(&[
        (
            "examples/Cargo.toml",
            "[workspace]\nresolver = \"2\"\nmembers = [\"one\"]\n",
        ),
        ("examples/one/Cargo.toml", manifest),
        ("examples/one/src/main.rs", source),
        ("benches/two/Cargo.toml", manifest),
        ("benches/two/src/main.rs", source),
        ("tests/three/Cargo.toml", manifest),
        ("tests/three/src/main.rs", source),
        ("spec/four/Cargo.toml", manifest),
        ("spec/four/src/main.rs", source),
    ]);
    let before = snapshot(&dir);
    let ctx = crate::interface::cli::context::Ctx::default();
    let result = super::surface(&ctx, &dir);
    let after = snapshot(&dir);
    assert!(
        result.is_ok(),
        "deployable services must not inherit source-file skip rules: {result:?}"
    );
    assert_eq!(before, after, "surface changed a discovered service");
    let super::drift::SourceRoot::Ambiguous(services) = super::drift::source_root(&dir, None)
    else {
        panic!("four nested services must remain independently discoverable");
    };
    assert!(
        services.contains(&"examples/one".to_string())
            && !services.contains(&"examples".to_string()),
        "a workspace-only manifest is an aggregator, not a service leaf: {services:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Every file under a root, with its bytes, so a rewrite is caught as well as
/// a create or a delete.
fn snapshot(root: &std::path::Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if let Ok(bytes) = std::fs::read(&path) {
                out.push((path, bytes));
            }
        }
    }
    out.sort();
    out
}

/// The decision `init` makes with no flags at all.
///
/// Deriving from source is what `init` should DO when a backend has no schema,
/// not something to ask for. It used to dead-end with advice to hand-write an
/// OpenAPI document, without even naming the flag that would have derived one.
///
/// The rule is tested directly rather than through `init`, which reads the
/// process working directory and so cannot be pointed at a fixture without
/// racing every other test in the binary.
#[test]
fn init_derives_only_for_an_unset_up_backend() {
    let server = (
        "server.js",
        "const app = require('express')();\napp.get('/health', h);\n",
    );
    let manifest = (
        "package.json",
        "{\"dependencies\":{\"express\":\"^4.19.0\"}}",
    );

    let bare = project(&[manifest, server]);
    assert!(
        crate::workflows::init_command::needs_derivation(&bare),
        "a backend with no schema and no config is the case that should just work"
    );

    // A conventional schema: there is a contract, so nothing is derived.
    let with_schema = project(&[
        manifest,
        server,
        (
            "openapi.yaml",
            "openapi: 3.1.0\ninfo: { title: t, version: \"1\" }\npaths: {}\n",
        ),
    ]);
    assert!(!crate::workflows::init_command::needs_derivation(
        &with_schema
    ));

    // Configured under a name no conventional lookup finds. Deriving here would
    // replace a real contract with a draft.
    let configured = project(&[
        manifest,
        server,
        (
            "reproit.yaml",
            "backend:\n  enabled: true\n  schemas: [service.yaml]\n",
        ),
        (
            "service.yaml",
            "openapi: 3.1.0\ninfo: { title: t, version: \"1\" }\npaths: {}\n",
        ),
    ]);
    assert!(
        !crate::workflows::init_command::needs_derivation(&configured),
        "an initialized project must never have its contract re-derived"
    );

    // A frontend is owned by the web workflow.
    let frontend = project(&[("package.json", "{\"dependencies\":{\"react\":\"^18\"}}")]);
    assert!(!crate::workflows::init_command::needs_derivation(&frontend));

    for dir in [bare, with_schema, configured, frontend] {
        let _ = std::fs::remove_dir_all(dir);
    }
}
