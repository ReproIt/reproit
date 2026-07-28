use reproit_protocol::{CaptureBatch, Event, EventBatch, VERSION};

#[test]
fn sdk_fixture_is_a_valid_versioned_batch() {
    let batch: EventBatch = serde_json::from_str(include_str!("../../../sdk/event-batch-v1.json"))
        .expect("SDK fixture must deserialize through the shared protocol");

    batch
        .validate()
        .expect("SDK fixture must satisfy every shared protocol bound");
    assert_eq!(batch.version, VERSION);
    assert_eq!(batch.frames.len(), 2);
    assert!(matches!(batch.frames[0].event, Event::GraphEdge { .. }));
    assert!(matches!(batch.frames[1].event, Event::Finding { .. }));
}

#[test]
fn universal_sdk_fixture_matches_the_vendored_protocol_fixture() {
    let sdk = include_str!("../../../sdk/capture-batch-v1.json");
    let protocol = include_str!("../fixtures/capture-batch-v1.json");
    assert_eq!(sdk, protocol);
    let batch: CaptureBatch =
        serde_json::from_str(sdk).expect("universal SDK fixture must deserialize");
    batch
        .validate()
        .expect("universal SDK fixture must satisfy every protocol bound");
}
