use super::*;
use serde_json::json;

fn op(id: &str, read_only: bool) -> OperationContract {
    let mut base = contract();
    base.id = id.into();
    base.read_only = read_only;
    base.output = None;
    base.promised_effects = Vec::new();
    base.success_statuses = vec![200];
    base
}

fn config() -> BackendConfig {
    BackendConfig {
        enabled: true,
        operations: vec![op("getNote", true), op("patchNote", false)],
        ..BackendConfig::default()
    }
}

fn call(seq: u64, span: &str, operation: &str, input: Value, output: Value) -> Vec<BackendEvent> {
    vec![
        event(seq, span, operation, BackendEventKind::Start { input }),
        event(
            seq + 1,
            span,
            operation,
            BackendEventKind::Return {
                output,
                status: Some(200),
                success: true,
                effects_complete: false,
            },
        ),
    ]
}

/// The round-trip quad: GET, GET (baseline), PATCH, GET (check).
fn quad(before: Value, patch_body: Value, after: Value) -> Vec<BackendEvent> {
    let read_input = json!({"id": "n1"});
    let mut events = call(1, "ra", "getNote", read_input.clone(), before.clone());
    events.extend(call(3, "rb", "getNote", read_input.clone(), before));
    events.extend(call(5, "m", "patchNote", patch_body, json!({})));
    events.extend(call(7, "rc", "getNote", read_input, after));
    events
}

fn data_loss_reasons(events: &[BackendEvent]) -> Vec<String> {
    evaluate(&config(), events)
        .into_iter()
        .filter(|violation| violation.oracle == "data-loss")
        .map(|violation| violation.reason)
        .collect()
}

#[test]
fn silent_write_loss_fires_when_an_accepted_write_reads_back_unchanged() {
    let reasons = data_loss_reasons(&quad(
        json!({"id": "n1", "title": "old", "body": "text"}),
        json!({"id": "n1", "title": "new"}),
        json!({"id": "n1", "title": "old", "body": "text"}),
    ));
    assert_eq!(reasons.len(), 1, "{reasons:?}");
    assert!(reasons[0].contains("`title`") && reasons[0].contains("silently dropped"));
}

#[test]
fn sibling_corruption_fires_when_an_untouched_field_changes() {
    let reasons = data_loss_reasons(&quad(
        json!({"id": "n1", "title": "old", "tags": ["a", "b"]}),
        json!({"id": "n1", "title": "new"}),
        json!({"id": "n1", "title": "new", "tags": []}),
    ));
    assert_eq!(reasons.len(), 1, "{reasons:?}");
    assert!(reasons[0].contains("`tags`") && reasons[0].contains("sibling"));
}

#[test]
fn data_loss_abstains_on_every_legitimate_shape() {
    // The write landed and nothing else moved: clean.
    assert!(data_loss_reasons(&quad(
        json!({"id": "n1", "title": "old", "body": "text"}),
        json!({"id": "n1", "title": "new"}),
        json!({"id": "n1", "title": "new", "body": "text"}),
    ))
    .is_empty());

    // Canonicalization: the server stored a THIRD value (trimmed), abstain.
    assert!(data_loss_reasons(&quad(
        json!({"id": "n1", "title": "old"}),
        json!({"id": "n1", "title": "  new  "}),
        json!({"id": "n1", "title": "new"}),
    ))
    .is_empty());

    // A volatile field (differs between the two baseline reads) is excluded
    // even when it changes again after the write.
    let read_input = json!({"id": "n1"});
    let mut events = call(
        1,
        "ra",
        "getNote",
        read_input.clone(),
        json!({"id": "n1", "title": "old", "updatedAt": 1}),
    );
    events.extend(call(
        3,
        "rb",
        "getNote",
        read_input.clone(),
        json!({"id": "n1", "title": "old", "updatedAt": 2}),
    ));
    events.extend(call(
        5,
        "m",
        "patchNote",
        json!({"id": "n1", "title": "new"}),
        json!({}),
    ));
    events.extend(call(
        7,
        "rc",
        "getNote",
        read_input,
        json!({"id": "n1", "title": "new", "updatedAt": 9}),
    ));
    assert!(data_loss_reasons(&events).is_empty());

    // No identity shared between the read and the mutation: unrelated
    // resources, abstain.
    let mut events = call(1, "ra", "getNote", json!({"id": "n1"}), json!({"x": 1}));
    events.extend(call(
        3,
        "rb",
        "getNote",
        json!({"id": "n1"}),
        json!({"x": 1}),
    ));
    events.extend(call(
        5,
        "m",
        "patchNote",
        json!({"id": "OTHER", "x": 2}),
        json!({}),
    ));
    events.extend(call(
        7,
        "rc",
        "getNote",
        json!({"id": "n1"}),
        json!({"x": 2}),
    ));
    assert!(data_loss_reasons(&events).is_empty());

    // A second mutation between baseline and check explains any change:
    // abstain (nothing is attributable).
    let read_input = json!({"id": "n1"});
    let mut events = call(
        1,
        "ra",
        "getNote",
        read_input.clone(),
        json!({"a": 1, "id": "n1"}),
    );
    events.extend(call(
        3,
        "rb",
        "getNote",
        read_input.clone(),
        json!({"a": 1, "id": "n1"}),
    ));
    events.extend(call(
        5,
        "m1",
        "patchNote",
        json!({"id": "n1", "a": 2}),
        json!({}),
    ));
    events.extend(call(
        7,
        "m2",
        "patchNote",
        json!({"id": "n1", "a": 3}),
        json!({}),
    ));
    events.extend(call(
        9,
        "rc",
        "getNote",
        read_input,
        json!({"a": 1, "id": "n1"}),
    ));
    assert!(data_loss_reasons(&events).is_empty());
}
