use super::*;

fn codec_result(
    input: Value,
    output: Value,
    authority: Authority,
    projections: Vec<CodecProjection>,
) -> (&'static str, Option<String>) {
    let start = BackendEvent {
        sequence: 1,
        trace_id: "trace".into(),
        span_id: "span".into(),
        action_index: 1,
        parent_span_id: None,
        operation: "codec".into(),
        build: None,
        config_contract: None,
        actor: None,
        tenant: None,
        idempotency_key: None,
        selections: Vec::new(),
        event: BackendEventKind::Start { input },
    };
    let returned = BackendEvent {
        sequence: 2,
        event: BackendEventKind::Return {
            output,
            status: Some(200),
            success: true,
            effects_complete: false,
        },
        ..start.clone()
    };
    let returned_output = match &returned.event {
        BackendEventKind::Return { output, .. } => output,
        _ => unreachable!(),
    };
    let contract = OperationContract {
        id: "codec".into(),
        authority,
        input: None,
        output: None,
        outputs_by_status: BTreeMap::new(),
        success_statuses: vec![200],
        read_only: true,
        idempotent: true,
        idempotency_response_replay: IdempotencyResponseReplay::Unspecified,
        tenant_isolated: false,
        promised_effects: Vec::new(),
    };
    let contracts = BTreeMap::from([("codec", &contract)]);
    let invocations = BTreeMap::from([(
        ("trace".into(), "span".into()),
        Invocation {
            start: Some(&start),
            returned: Some(ReturnEvent {
                event: &returned,
                output: returned_output,
                status: Some(200),
                success: true,
                effects_complete: false,
            }),
            effects: Vec::new(),
            protocols: Vec::new(),
        },
    )]);
    let proof = BackendProofContract::CodecRoundTrip {
        operation: "codec".into(),
        projections,
    };
    match codec_round_trip_outcome(&proof, &contracts, &invocations) {
        ProofOutcome::Violation { oracle, reason, .. } => {
            ("violation", Some(format!("{oracle}:{reason}")))
        }
        ProofOutcome::Satisfied => ("satisfied", None),
        ProofOutcome::Abstain => ("abstain", None),
    }
}

#[test]
fn codec_projection_has_violation_satisfied_and_abstain_outcomes() {
    let projection = || CodecProjection {
        input_path: "$.typed.amount".into(),
        output_path: "$.decoded.amount".into(),
    };
    assert_eq!(
        codec_result(
            json!({"typed":{"amount":"10.25"}}),
            json!({"decoded":{"amount":"10.25"}}),
            Authority::Declared,
            vec![projection()],
        )
        .0,
        "satisfied"
    );
    let violation = codec_result(
        json!({"typed":{"amount":"10.25"}}),
        json!({"decoded":{"amount":"10.2"}}),
        Authority::Declared,
        vec![projection()],
    );
    assert_eq!(violation.0, "violation");
    assert!(violation.1.unwrap().contains("codec-round-trip"));
    assert_eq!(
        codec_result(
            json!({"typed":{"amount":"10.25"}}),
            json!({"decoded":{}}),
            Authority::Declared,
            vec![projection()],
        )
        .0,
        "abstain"
    );
    assert_eq!(
        codec_result(
            json!({"typed":{"amount":"10.25"}}),
            json!({"decoded":{"amount":"different"}}),
            Authority::Inferred,
            vec![projection()],
        )
        .0,
        "abstain"
    );
}
