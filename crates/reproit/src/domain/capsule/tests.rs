use super::*;
use serde_json::json;

fn finding() -> FindingIdentity {
    FindingIdentity {
        oracle: "crash".into(),
        invariant: "no-exception".into(),
        kind: "TypeError".into(),
        message: "cannot read property".into(),
        frame: "FeedItem.fromJson:42".into(),
        trigger: "key:feed".into(),
        boundary: Some("GET /feed".into()),
    }
}

#[test]
fn structural_bug_identity_ignores_the_replay_path_and_is_field_sensitive() {
    let identity = finding();
    assert_eq!(identity.bug_id(), identity.clone().bug_id());
    assert!(identity.bug_id().starts_with("bug_"));

    let mut other = identity.clone();
    other.trigger.push_str("-other");
    assert_ne!(identity.bug_id(), other.bug_id());
}

#[test]
fn completeness_is_derived_from_required_inputs() {
    let mut c = Capsule::new("app", finding());
    c.actions.push(Action {
        index: 0,
        actor: "a".into(),
        action: "tap:key:feed".into(),
        from_sig: None,
        to_sig: None,
    });
    c.exchanges.push(Exchange {
        id: "n1".into(),
        actor: "a".into(),
        action_index: 0,
        ordinal: 0,
        protocol: "https".into(),
        method: "GET".into(),
        url: "https://x/feed".into(),
        request_headers: BTreeMap::new(),
        request_body: None,
        status: 200,
        response_headers: BTreeMap::new(),
        response_body: Some(json!({"items":[]})),
        required: true,
    });
    c.capabilities.insert(
        "ui_actions".into(),
        Capability {
            status: CaptureStatus::Captured,
            detail: None,
        },
    );
    assert_eq!(c.missing_required_capabilities(), vec!["http"]);
    c.capabilities.insert(
        "http".into(),
        Capability {
            status: CaptureStatus::Captured,
            detail: None,
        },
    );
    assert!(c.confirmable());
}

#[test]
fn load_only_ui_finding_is_confirmable_with_a_captured_empty_schedule() {
    let mut c = Capsule::new("app", finding());
    for name in ["ui_actions", "http"] {
        c.capabilities.insert(
            name.into(),
            Capability {
                status: CaptureStatus::Captured,
                detail: None,
            },
        );
    }

    assert!(c.actions.is_empty());
    assert!(c.confirmable());
}

#[test]
fn bootstrap_backend_finding_is_confirmable_without_ui_actions() {
    let mut c = Capsule::new(
        "app",
        FindingIdentity {
            oracle: "contract".into(),
            invariant: "backend:response-shape".into(),
            kind: "response-shape".into(),
            message: "response omitted account id".into(),
            frame: "getAccount".into(),
            trigger: "bootstrap".into(),
            boundary: None,
        },
    );
    c.backend_events.push(crate::domain::backend::BackendEvent {
        sequence: 1,
        trace_id: "trace".into(),
        span_id: "span".into(),
        action_index: 0,
        parent_span_id: None,
        operation: "getAccount".into(),
        build: None,
        config_contract: None,
        actor: None,
        tenant: None,
        idempotency_key: None,
        selections: Vec::new(),
        event: crate::domain::backend::BackendEventKind::Start { input: Value::Null },
    });
    for name in ["ui_actions", "http", "backend_effects"] {
        c.capabilities.insert(
            name.into(),
            Capability {
                status: CaptureStatus::Captured,
                detail: None,
            },
        );
    }
    assert!(c.actions.is_empty());
    assert!(c.confirmable());
}

#[test]
fn redaction_is_recursive_typed_and_manifested() {
    let mut c = Capsule::new("app", finding());
    c.exchanges.push(Exchange {
        id: "n".into(),
        actor: "a".into(),
        action_index: 0,
        ordinal: 0,
        protocol: "https".into(),
        method: "POST".into(),
        url: "https://x".into(),
        request_headers: BTreeMap::from([("Authorization".into(), "Bearer raw".into())]),
        request_body: Some(json!({"profile":{"email":"a@example.com"},"count":2})),
        status: 200,
        response_headers: BTreeMap::new(),
        response_body: None,
        required: true,
    });
    c.backend_events.push(crate::domain::backend::BackendEvent {
        sequence: 1,
        trace_id: "trace".into(),
        span_id: "span".into(),
        action_index: 0,
        parent_span_id: None,
        operation: "createUser".into(),
        build: None,
        config_contract: None,
        actor: Some("a".into()),
        tenant: Some("team".into()),
        idempotency_key: Some("payment-retry-secret".into()),
        selections: Vec::new(),
        event: crate::domain::backend::BackendEventKind::Start {
            input: json!({"profile":{"email":"a@example.com"}}),
        },
    });
    redact_capsule(&mut c, &RedactionPolicy::default());
    assert_eq!(
        c.exchanges[0].request_headers["Authorization"],
        "<reproit:secret>"
    );
    assert_eq!(
        c.exchanges[0].request_body.as_ref().unwrap()["profile"]["email"],
        json!({"$reproit":{"redacted":true,"type":"string","length":13}})
    );
    assert!(c.redactions.contains(&"$request.profile.email".into()));
    let crate::domain::backend::BackendEventKind::Start { input } = &c.backend_events[0].event
    else {
        panic!("expected start event");
    };
    assert_eq!(
        input["profile"]["email"],
        json!({"$reproit":{"redacted":true,"type":"string","length":13}})
    );
    assert!(c
        .redactions
        .contains(&"$backend.input.profile.email".into()));
    assert_eq!(
        c.backend_events[0].idempotency_key.as_deref(),
        Some("sha256:c5f7b22400db7ee6d27dfbf7")
    );
}

#[test]
fn backend_findings_require_structural_replay_capability() {
    let mut backend = finding();
    backend.oracle = "backend-contract".into();
    let mut capsule = Capsule::new("app", backend);
    capsule.capabilities.insert(
        "http_replay".into(),
        Capability {
            status: CaptureStatus::Captured,
            detail: None,
        },
    );
    assert_eq!(
        capsule.missing_required_replay_capabilities(),
        vec!["backend_effects_replay"]
    );
    capsule.capabilities.insert(
        "backend_effects_replay".into(),
        Capability {
            status: CaptureStatus::Captured,
            detail: None,
        },
    );
    assert!(capsule.missing_required_replay_capabilities().is_empty());
}

#[test]
fn matching_and_reduction_are_deterministic() {
    let mut e = Exchange {
        id: "n".into(),
        actor: "bob".into(),
        action_index: 2,
        ordinal: 0,
        protocol: "https".into(),
        method: "post".into(),
        url: "https://x/p?b=2&a=1".into(),
        request_headers: BTreeMap::new(),
        request_body: Some(json!({"x":1})),
        status: 200,
        response_headers: BTreeMap::new(),
        response_body: None,
        required: true,
    };
    let a = exchange_match_key(&e);
    e.url = "https://x/p?a=1&b=2".into();
    assert_eq!(a, exchange_match_key(&e));
    let reductions = json_reductions(&json!({"items":[{"author":null,"name":"x"}],"page":1}));
    assert!(!reductions.is_empty());
    assert_eq!(
        reductions,
        json_reductions(&json!({"items":[{"author":null,"name":"x"}],"page":1}))
    );
}

#[test]
fn exchange_wire_format_is_canonical_camel_case_and_reads_legacy_snake_case() {
    let exchange = Exchange {
        id: "a-1-0".into(),
        actor: "a".into(),
        action_index: 1,
        ordinal: 0,
        protocol: "https".into(),
        method: "GET".into(),
        url: "https://x.test".into(),
        request_headers: BTreeMap::new(),
        request_body: Some(json!({"q":1})),
        status: 200,
        response_headers: BTreeMap::new(),
        response_body: Some(json!({"ok":true})),
        required: true,
    };
    let value = serde_json::to_value(&exchange).unwrap();
    assert_eq!(value["actionIndex"], 1);
    assert!(value.get("action_index").is_none());
    assert!(value.get("requestHeaders").is_some());
    assert!(value.get("responseBody").is_some());
    let legacy = json!({"id":"a-1-0","actor":"a","action_index":1,"ordinal":0,
        "protocol":"https","method":"GET","url":"https://x.test","request_headers":{},
        "request_body": null,
        "status": 200,
        "response_headers": {},
        "response_body": {"ok": true},
        "required": true
    });
    assert_eq!(
        serde_json::from_value::<Exchange>(legacy)
            .unwrap()
            .action_index,
        1
    );
}

#[test]
fn persisted_id_is_content_addressed_and_round_trips() {
    let root = std::env::temp_dir().join(format!("reproit-capsule-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let mut c = Capsule::new("app", finding());
    c.actions.push(Action {
        index: 0,
        actor: "a".into(),
        action: "tap:key:feed".into(),
        from_sig: None,
        to_sig: None,
    });
    let dir = c.persist(&root).unwrap();
    let loaded = Capsule::load(&root, &c.id).unwrap();
    assert_eq!(loaded, c);
    assert_eq!(
        dir,
        crate::runtime::project_layout::capsule_dir(&root, &c.id)
    );
    assert!(dir.join("capsule.enc").is_file());
    assert!(!dir.join("capsule.json").exists());
    let plaintext_path;
    {
        let guard = Capsule::materialize_plaintext(&root, &c.id).unwrap();
        plaintext_path = guard.path().to_path_buf();
        assert!(plaintext_path.is_file());
    }
    assert!(
        !plaintext_path.exists(),
        "plaintext scratch must delete on drop"
    );
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn retention_never_removes_referenced_or_inflight_capsules() {
    let root = std::env::temp_dir().join(format!("reproit-cap-prune-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for id in ["cap_old", "cap_pinned", "cap_current"] {
        std::fs::create_dir_all(crate::runtime::project_layout::capsule_dir(&root, id)).unwrap();
        std::fs::write(
            crate::runtime::project_layout::capsule_dir(&root, id).join("capsule.enc"),
            id,
        )
        .unwrap();
    }
    let finding = root.join(".reproit/findings/fnd");
    std::fs::create_dir_all(&finding).unwrap();
    std::fs::write(finding.join("capsule-id"), "cap_pinned").unwrap();
    assert_eq!(
        prune_unreferenced(&root, Some("cap_current"), 0, std::time::Duration::MAX).unwrap(),
        1
    );
    assert!(!crate::runtime::project_layout::capsule_dir(&root, "cap_old").exists());
    assert!(crate::runtime::project_layout::capsule_dir(&root, "cap_pinned").exists());
    assert!(crate::runtime::project_layout::capsule_dir(&root, "cap_current").exists());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn key_rotation_reencrypts_every_capsule_and_preserves_content() {
    let root = std::env::temp_dir().join(format!("reproit-cap-rotate-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let mut capsule = Capsule::new("app", finding());
    capsule.actions.push(Action {
        index: 1,
        actor: "a".into(),
        action: "tap:key:x".into(),
        from_sig: None,
        to_sig: None,
    });
    capsule.capabilities.insert(
        "ui_actions".into(),
        Capability {
            status: CaptureStatus::Captured,
            detail: None,
        },
    );
    capsule.capabilities.insert(
        "http".into(),
        Capability {
            status: CaptureStatus::Captured,
            detail: None,
        },
    );
    capsule.persist(&root).unwrap();
    let id = capsule.id.clone();
    let before_key =
        std::fs::read(crate::runtime::project_layout::capsule_key_path(&root)).unwrap();
    assert_eq!(rotate_key(&root).unwrap(), 1);
    let after_key = std::fs::read(crate::runtime::project_layout::capsule_key_path(&root)).unwrap();
    assert_ne!(before_key, after_key);
    assert_eq!(Capsule::load(&root, &id).unwrap(), capsule);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn joint_minimizer_removes_actions_exchanges_and_json_by_exact_contract() {
    let mut c = Capsule::new("app", finding());
    c.actions = vec![
        Action {
            index: 0,
            actor: "a".into(),
            action: "tap:key:noise".into(),
            from_sig: None,
            to_sig: None,
        },
        Action {
            index: 1,
            actor: "a".into(),
            action: "tap:key:feed".into(),
            from_sig: None,
            to_sig: None,
        },
    ];
    let exchange = |id: &str, action_index, body| Exchange {
        id: id.into(),
        actor: "a".into(),
        action_index,
        ordinal: 0,
        protocol: "https".into(),
        method: "GET".into(),
        url: format!("https://x/{id}"),
        request_headers: BTreeMap::new(),
        request_body: None,
        status: 200,
        response_headers: BTreeMap::new(),
        response_body: Some(body),
        required: true,
    };
    c.exchanges = vec![
        exchange("noise", 0, json!({"ok":true})),
        exchange(
            "feed",
            1,
            json!({"items":[{"author":null,"name":"Ada"}],"page":1}),
        ),
    ];
    let reproduces = |candidate: &Capsule| {
        candidate.actions.iter().any(|a| a.action == "tap:key:feed")
            && candidate.exchanges.iter().any(|e| {
                e.url.ends_with("/feed")
                    && e.response_body
                        .as_ref()
                        .is_some_and(|b| b.to_string().contains("\"author\":null"))
            })
    };
    let shrunk = minimize_exact(&c, reproduces).unwrap();
    assert_eq!(shrunk.actions.len(), 1);
    assert_eq!(shrunk.actions[0].index, 0);
    assert_eq!(shrunk.exchanges.len(), 1);
    assert_eq!(shrunk.exchanges[0].action_index, 0);
    assert_eq!(
        shrunk.exchanges[0].response_body,
        Some(json!({"items":[{"author":null}]}))
    );
}
