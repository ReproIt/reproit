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
            "let app = Router::new()\n    .route(\"/orders\", post(create).get(list))\n    \
             .route(\"/orders/{id}\", get(show))\n    .route(\"/health\", get(health));\n",
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
             App::new().route(\"/orders/{id}\", web::patch().to(update))\n",
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
                ("/orders/{id}", &["delete", "get", "patch"]),
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
            "Route::get('/projects', [ProjectController::class, 'index']);\n\
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
    // Unconfident shapes are rejected, not guessed.
    assert_eq!(normalize_path("/files/*path"), None);
    assert_eq!(normalize_path("http://x/a"), None);
    assert_eq!(normalize_path("/a b"), None);
    assert_eq!(normalize_path("/^orders$"), None);
    assert_eq!(path_params("/a/{id}/b/{name}"), vec!["id", "name"]);
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
        ".route(\"/orders\", post(create).get(list))\n.route(\"/orders/{id}\", get(show))\n",
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
    let dir = project(&[("src/main.rs", ".route(\"/health\", get(health))\n")]);
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
