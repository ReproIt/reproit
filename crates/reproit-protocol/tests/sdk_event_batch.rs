use reproit_protocol::{CaptureBatch, Event, EventBatch, VERSION};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::Path;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CaptureConformance {
    version: u16,
    schema_command: String,
    fixture: String,
    fixture_sha256: String,
    sdks: Vec<CaptureSdk>,
}

#[derive(Deserialize)]
struct CaptureSdk {
    id: String,
    implementation: String,
}

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

#[test]
fn every_sdk_is_registered_against_one_capture_contract() {
    let sdk_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../sdk");
    let manifest: CaptureConformance =
        serde_json::from_str(include_str!("../../../sdk/capture-conformance-v1.json"))
            .expect("capture conformance manifest must deserialize");

    assert_eq!(manifest.version, reproit_protocol::CAPTURE_BATCH_VERSION);
    assert_eq!(
        manifest.schema_command,
        "cargo run -q -p reproit-protocol --bin capture-schema"
    );
    assert_eq!(manifest.fixture, "capture-batch-v1.json");

    let fixture = include_bytes!("../../../sdk/capture-batch-v1.json");
    assert_eq!(
        hex::encode(Sha256::digest(fixture)),
        manifest.fixture_sha256
    );

    let ids = manifest
        .sdks
        .iter()
        .map(|sdk| sdk.id.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(ids.len(), manifest.sdks.len(), "SDK ids must be unique");
    assert_eq!(
        ids.len(),
        20,
        "every shipped SDK must register its contract"
    );
    for sdk in manifest.sdks {
        let implementation = sdk_root.join(&sdk.implementation);
        assert!(
            implementation.is_file(),
            "{} implementation is missing: {}",
            sdk.id,
            implementation.display()
        );
    }
}

#[test]
fn generated_capture_schema_rejects_unknown_fields() {
    let schema = schemars::schema_for!(CaptureBatch);
    let encoded = serde_json::to_value(schema).expect("schema must serialize");
    assert_eq!(
        encoded["$schema"],
        "https://json-schema.org/draft/2020-12/schema"
    );
    assert_eq!(encoded["additionalProperties"], false);
    assert_eq!(encoded["title"], "CaptureBatch");
}
