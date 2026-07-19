use reproit::backend_contracts::{
    evaluate, validate_http_conditional_cache, validate_http_response_media_type,
    validate_protocol_lifecycle, BackendConfig, BackendEvent, HttpExchangeEvidence,
    ProtocolLifecycleContract, ProtocolLifecycleEvidence,
};
use serde::Deserialize;
use std::{collections::BTreeSet, env, fs, process::ExitCode};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CapturedRun {
    service: String,
    mode: String,
    codec: CodecCapture,
    initial_cache: HttpExchangeEvidence,
    conditional_cache: HttpExchangeEvidence,
    media: HttpExchangeEvidence,
    lifecycle_contract: ProtocolLifecycleContract,
    lifecycle_evidence: ProtocolLifecycleEvidence,
}

#[derive(Deserialize)]
struct CodecCapture {
    input: serde_json::Value,
    output: Option<serde_json::Value>,
}

fn main() -> ExitCode {
    let Some(path) = env::args().nth(1) else {
        eprintln!("usage: reproit-real-backend-validator <capture.json>");
        return ExitCode::from(2);
    };
    let capture: CapturedRun = match fs::read(&path)
        .map_err(|error| error.to_string())
        .and_then(|bytes| serde_json::from_slice(&bytes).map_err(|error| error.to_string()))
    {
        Ok(capture) => capture,
        Err(error) => {
            eprintln!("capture error: {error}");
            return ExitCode::from(2);
        }
    };

    let config: BackendConfig = serde_json::from_value(serde_json::json!({
        "enabled": true,
        "operations": [{
            "id": "codecRoundTrip",
            "authority": "declared",
            "successStatuses": [200],
            "readOnly": true,
            "idempotent": true
        }],
        "proofs": [{
            "kind": "codec-round-trip",
            "operation": "codecRoundTrip",
            "projections": [{"inputPath": "$.typed", "outputPath": "$.decoded"}]
        }]
    }))
    .expect("the checked-in codec contract is valid");
    let start: BackendEvent = serde_json::from_value(serde_json::json!({
        "sequence": 1,
        "traceId": "real-service-codec",
        "spanId": "codec",
        "operation": "codecRoundTrip",
        "kind": "start",
        "input": {"typed": capture.codec.input}
    }))
    .expect("the checked-in start event is valid");
    let codec_output = capture.codec.output.map_or_else(
        || serde_json::json!({}),
        |value| serde_json::json!({"decoded": value}),
    );
    let returned: BackendEvent = serde_json::from_value(serde_json::json!({
        "sequence": 2,
        "traceId": "real-service-codec",
        "spanId": "codec",
        "operation": "codecRoundTrip",
        "kind": "return",
        "output": codec_output,
        "status": 200,
        "success": true,
        "effectsComplete": false
    }))
    .expect("the checked-in return event is valid");
    let codec = evaluate(&config, &[start, returned]);
    let cache = validate_http_conditional_cache(&capture.initial_cache, &capture.conditional_cache);
    let media = validate_http_response_media_type(
        &capture.media,
        &BTreeSet::from(["application/json".to_string()]),
    );
    let lifecycle =
        validate_protocol_lifecycle(&capture.lifecycle_contract, &capture.lifecycle_evidence);
    let observed = serde_json::json!({
        "codec": codec.iter().map(|finding| finding.oracle.as_str()).collect::<Vec<_>>(),
        "cache": cache.as_ref().map(|finding| finding.oracle.as_str()),
        "media": media.as_ref().map(|finding| finding.oracle.as_str()),
        "lifecycle": lifecycle.iter().map(|finding| finding.oracle.as_str()).collect::<Vec<_>>()
    });
    println!(
        "{}",
        serde_json::to_string(&serde_json::json!({
            "service": capture.service,
            "mode": capture.mode,
            "observed": observed
        }))
        .unwrap()
    );

    let violation_count =
        codec.len() + usize::from(cache.is_some()) + usize::from(media.is_some()) + lifecycle.len();
    let passed = match capture.mode.as_str() {
        "clean" | "incomplete" => violation_count == 0,
        "broken" => {
            codec
                .iter()
                .any(|finding| finding.oracle == "codec-round-trip")
                && cache
                    .as_ref()
                    .is_some_and(|finding| finding.oracle == "http-conditional-cache")
                && media
                    .as_ref()
                    .is_some_and(|finding| finding.oracle == "http-response-media-type")
                && lifecycle
                    .iter()
                    .any(|finding| finding.oracle == "lifecycle-forbid-after")
        }
        _ => false,
    };
    if passed {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
