use super::*;

mod data_loss;
mod effects;
mod invariants;
mod lifecycle;
mod proof_contracts;
mod protocol;
mod query;
mod schema_imports;

fn event(sequence: u64, span: &str, operation: &str, kind: BackendEventKind) -> BackendEvent {
    BackendEvent {
        sequence,
        trace_id: "trace-a".into(),
        span_id: span.into(),
        action_index: 1,
        parent_span_id: None,
        operation: operation.into(),
        build: None,
        config_contract: None,
        actor: Some("alice".into()),
        tenant: Some("tenant-a".into()),
        idempotency_key: None,
        selections: Vec::new(),
        at: None,
        mono_ns: None,
        event: kind,
    }
}

fn contract() -> OperationContract {
    OperationContract {
        id: "createMessage".into(),
        authority: Authority::Declared,
        input: None,
        output: Some(ValueDomain::Object {
            required: BTreeSet::from(["id".into()]),
            properties: BTreeMap::from([(
                "id".into(),
                ValueDomain::String {
                    min_length: Some(1),
                    max_length: None,
                    pattern: None,
                    format: None,
                    variants: vec![],
                },
            )]),
            additional: true,
        }),
        outputs_by_status: BTreeMap::new(),
        success_statuses: vec![201],
        read_only: false,
        idempotent: false,
        idempotency_response_replay: IdempotencyResponseReplay::Unspecified,
        tenant_isolated: true,
        promised_effects: vec![EffectPattern {
            kind: EffectKind::Write,
            resource: Some("messages".into()),
            event: None,
            at_least: 1,
            at_most: None,
        }],
    }
}

#[test]
fn hard_oracles_require_concrete_authoritative_witnesses() {
    let config = BackendConfig {
        exec: None,
        enabled: true,
        origins: vec![],
        schemas: vec![],
        target: None,
        operations: vec![contract()],
        programs: vec![],
        invariants: vec![],
        resources: vec![],
        proofs: vec![],
        fleet: FleetInvariant::default(),
        auth: None,
        reset: Default::default(),
        source: None,
    };
    let events = vec![
        event(
            1,
            "span-a",
            "createMessage",
            BackendEventKind::Start {
                input: json!({"body":"hello"}),
            },
        ),
        event(
            2,
            "span-a",
            "createMessage",
            BackendEventKind::Effect {
                effect: EffectKind::Write,
                resource: Some("messages".into()),
                key: Some("m1".into()),
                tenant: Some("tenant-b".into()),
                event: None,
                before: None,
                after: Some(json!({"id":"m1"})),
                payload: None,
                exchange: None,
            },
        ),
        event(
            3,
            "span-a",
            "createMessage",
            BackendEventKind::Return {
                output: json!({}),
                status: Some(201),
                success: true,
                effects_complete: true,
            },
        ),
    ];
    let violations = evaluate(&config, &events);
    assert_eq!(violations.len(), 2);
    assert!(violations.iter().any(|v| v.oracle == "response-shape"));
    assert!(violations.iter().any(|v| v.oracle == "tenant-isolation"));
}

#[test]
fn inferred_contracts_never_create_findings() {
    let mut inferred = contract();
    inferred.authority = Authority::Inferred;
    let config = BackendConfig {
        exec: None,
        enabled: true,
        origins: vec![],
        schemas: vec![],
        target: None,
        operations: vec![inferred],
        programs: vec![],
        invariants: vec![],
        resources: vec![],
        proofs: vec![],
        fleet: FleetInvariant::default(),
        auth: None,
        reset: Default::default(),
        source: None,
    };
    let events = vec![
        event(
            1,
            "span-a",
            "createMessage",
            BackendEventKind::Start { input: Value::Null },
        ),
        event(
            2,
            "span-a",
            "createMessage",
            BackendEventKind::Return {
                output: Value::Null,
                // Even a status outside this inferred operation's imported
                // shape is guidance only; inferred facts never become hard
                // response-status findings.
                status: Some(201),
                success: true,
                effects_complete: true,
            },
        ),
    ];
    let violations = evaluate(&config, &events);
    assert!(violations.is_empty(), "{violations:#?}");
}

#[test]
fn backend_config_rejects_flat_auth_sugar() {
    let result = serde_yaml::from_str::<BackendConfig>(
        "enabled: true\n\
         schemas: [openapi.json]\n\
         auth:\n  \
           path: /api/auth/token\n  \
           form:\n    \
             username: ${MEALIE_USER}\n    \
             password: ${MEALIE_PASS}\n  \
           tokenPath: /access_token\n",
    );
    assert!(result.is_err());
}

#[test]
fn backend_config_parses_auth_account_pool() {
    let config: BackendConfig = serde_yaml::from_str(
        "enabled: true\n\
         schemas: [openapi.json]\n\
         auth:\n  \
           accounts:\n    \
             - login: { path: /v1/auth, form: { user: a }, tokenPath: /access_token }\n      \
               headers: { Authorization: \"Bearer {token}\", x-user-id: \"{claim:sub}\" }\n    \
             - login: { url: https://global.example/quick-auth, json: { code: b }, tokenPath: /token }\n      \
               headers: { x-region-session: \"{token}\" }\n",
    )
    .unwrap();
    let accounts = config.auth.unwrap().accounts;
    assert_eq!(accounts.len(), 2, "two pooled identities");
    assert_eq!(accounts[0].headers.len(), 2, "multi-header identity");
    assert_eq!(
        accounts[0].headers.get("x-user-id").map(String::as_str),
        Some("{claim:sub}")
    );
    assert_eq!(
        accounts[1].login.url.as_deref(),
        Some("https://global.example/quick-auth"),
        "cross-host absolute login url"
    );
}
