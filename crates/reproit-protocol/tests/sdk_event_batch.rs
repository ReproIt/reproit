use reproit_protocol::{Event, EventBatch, VERSION};

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
