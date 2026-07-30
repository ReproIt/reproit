//! Live-probe and draft-emission tests: what one bounded synthesized
//! request per operation observes, and how it lands in the draft schema with
//! its provenance. Split from `tests.rs` at the emission/probing boundary
//! when that file hit the reviewability line cap.

use super::extract::derive;
use super::tests::project;
use super::{emit, enrich, probe_plan};
use std::collections::BTreeMap;
use std::io::{Read as _, Write as _};

#[test]
fn draft_yaml_round_trips_through_the_schema_importer() {
    let dir = project(&[(
        "src/main.rs",
        "fn app() -> Router {\n    Router::new()\n        .route(\"/orders\", \
         post(create).get(list))\n        .route(\"/orders/{id}\", get(show))\n}\n",
    )]);
    let derived = derive(&dir, "axum").unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    let plan = probe_plan::ProbePlan::default();
    let yaml = emit::draft_yaml("fixture", "axum", &derived, &plan, &BTreeMap::new()).unwrap();
    assert!(yaml.contains("x-reproit-derived: true"));
    assert!(yaml.starts_with("# DRAFT schema derived by `reproit init`"));
    assert!(yaml.contains("operationId: get_orders_id"));
    // Everything read from source is marked with its provenance, and init
    // never writes the one value only a user may state.
    assert!(yaml.contains("x-reproit-provenance: inferred"));
    assert!(!yaml.contains("x-reproit-provenance: confirmed"), "{yaml}");
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
    let dir = project(&[(
        "src/main.rs",
        "fn app() -> Router { Router::new().route(\"/health\", get(health)) }\n",
    )]);
    let derived = derive(&dir, "axum").unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    let plan = probe_plan::plan(&derived, false, enrich::MAX_PROBED_ROUTES);
    let outcome = enrich::probe(&base, &plan.probes).await;
    let requests = handle.join().unwrap();
    assert!(requests[0].starts_with("GET /health HTTP/1.1"));
    assert!(requests[0].to_lowercase().contains("x-reproit-trace"));
    assert!(outcome.adapter);
    let observed = &outcome.observations[&("get".to_string(), "/health".to_string())];
    assert_eq!(observed.status, 200);
    assert_eq!(observed.effects, vec!["read(inventory)".to_string()]);
    let shape = observed.body.as_ref().unwrap();
    assert_eq!(shape["ok"], serde_json::json!(true));
    // The observation lands in the draft as a recorded response + comment,
    // marked with the `observed` provenance.
    let yaml = emit::draft_yaml("fixture", "axum", &derived, &plan, &outcome.observations).unwrap();
    let note = "# observed live during init: HTTP 200; adapter effects: read(inventory)";
    assert!(yaml.contains(note));
    assert!(yaml.contains("\"200\":"));
    assert!(yaml.contains("x-reproit-provenance: observed"));
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
    let routes: Vec<(String, Vec<&'static str>)> = (0..40)
        .map(|index| (format!("/r{index:02}"), vec!["get"]))
        .collect();
    let mut derived = super::extract::Derived::default();
    for (path, methods) in &routes {
        let entry = derived.routes.entry(path.clone()).or_default();
        for method in methods {
            entry.insert(method);
        }
    }
    let plan = probe_plan::plan(&derived, false, enrich::MAX_PROBED_ROUTES);
    assert_eq!(plan.probes.len(), enrich::MAX_PROBED_ROUTES);
    // A closed port: every probe fails soft and nothing is recorded.
    let dead = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", listener.local_addr().unwrap())
    };
    let outcome = enrich::probe(&dead, &plan.probes).await;
    assert!(outcome.attempted <= enrich::MAX_PROBED_ROUTES);
    assert!(outcome.observations.is_empty());
    assert!(!outcome.adapter);
}

/// The dynamic-language contract path end to end: an express-style source
/// with an inline POST handler yields parsed body fields, the plan
/// synthesizes exactly one POST with those fields (only because the server
/// is init-booted), and the observed status and shape land in the draft as
/// an `observed` baseline while the request itself is recorded.
#[tokio::test]
async fn a_booted_target_gets_one_synthesized_post_recorded_as_observed() {
    let dir = project(&[(
        "server.js",
        "const express = require('express');\nconst app = express();\n\
         app.post('/items', (req, res) => {\n  const { name, price } = req.body;\n\
         \x20 res.status(201).json({ id: 1, name: name.trim(), price });\n});\n",
    )]);
    let derived = derive(&dir, "express").unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    let plan = probe_plan::plan(&derived, true, enrich::MAX_PROBED_ROUTES);
    assert_eq!(plan.probes.len(), 1);
    assert_eq!(
        plan.probes[0].body,
        Some(serde_json::json!({"name": "reproit", "price": "reproit"}))
    );
    let body = r#"{"id":1,"name":"reproit","price":"reproit"}"#;
    let response = format!(
        "HTTP/1.1 201 Created\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\
         connection: close\r\n\r\n{body}",
        body.len()
    );
    let response: &'static str = Box::leak(response.into_boxed_str());
    let (base, handle) = stub_server(response, 1);
    let outcome = enrich::probe(&base, &plan.probes).await;
    let requests = handle.join().unwrap();
    assert!(
        requests[0].starts_with("POST /items HTTP/1.1"),
        "{}",
        requests[0]
    );
    assert!(
        requests[0].contains("\"name\":\"reproit\""),
        "{}",
        requests[0]
    );
    let yaml =
        emit::draft_yaml("fixture", "express", &derived, &plan, &outcome.observations).unwrap();
    assert!(yaml.contains("\"201\":"), "{yaml}");
    assert!(yaml.contains("x-reproit-provenance: observed"), "{yaml}");
    assert!(
        yaml.contains("request body synthesized from parsed source fields"),
        "{yaml}"
    );
    // The parsed field names are stated (untyped) in the request body, so a
    // missing-field 500 is expressible against this draft.
    assert!(yaml.contains("\"name\": {}"), "{yaml}");
}

/// Against a server init did not boot, the same source plans NO mutating
/// probe, and the draft says exactly why the POST has no observation.
#[test]
fn an_unbooted_target_keeps_every_mutating_probe_out_of_the_draft_honestly() {
    let dir = project(&[(
        "server.js",
        "const express = require('express');\nconst app = express();\n\
         app.get('/items', (req, res) => res.json([]));\n\
         app.post('/items', (req, res) => {\n  const { name } = req.body;\n\
         \x20 res.status(201).json({ name });\n});\n",
    )]);
    let derived = derive(&dir, "express").unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    let plan = probe_plan::plan(&derived, false, enrich::MAX_PROBED_ROUTES);
    assert!(plan.probes.iter().all(|probe| probe.method == "get"));
    let yaml = emit::draft_yaml("fixture", "express", &derived, &plan, &BTreeMap::new()).unwrap();
    assert!(
        yaml.contains(&format!(
            "# not probed during init: {}",
            probe_plan::SKIP_FOREIGN_SERVER
        )),
        "{yaml}"
    );
}

#[test]
fn malformed_adapter_trails_note_nothing() {
    assert!(enrich::decode_effects("not base64url !!!").is_empty());
    use base64::Engine as _;
    let not_events = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b"{\"nope\":1}");
    assert!(enrich::decode_effects(&not_events).is_empty());
}
