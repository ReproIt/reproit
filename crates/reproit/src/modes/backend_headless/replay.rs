use super::*;

pub(super) fn has_fingerprint(result: &InvocationResult, expected: &str) -> bool {
    result
        .violations
        .iter()
        .any(|violation| violation.fingerprint == expected)
}

pub(super) async fn replay_sequence(
    client: &reqwest::Client,
    setup: &[ReplayStep],
    failing_endpoint: &Endpoint,
    failing_request: &RequestArtifact,
    expected: &str,
) -> Result<bool> {
    maybe_reset_target(client, &failing_request.url).await?;
    let mut events = Vec::new();
    let mut contracts = Vec::new();
    let mut outputs = Vec::new();
    for (index, step) in setup.iter().enumerate() {
        let endpoint = replay_endpoint(step);
        let mut request = step.request.clone();
        if !apply_request_bindings(&mut request, &outputs) {
            return Ok(false);
        }
        let result = invoke(client, &endpoint, request).await?;
        if !(200..400).contains(&result.status) || !result.violations.is_empty() {
            return Ok(false);
        }
        contracts.push(step.contract.clone());
        outputs.push(result.output.clone());
        append_sequence_events(&mut events, result.events, index);
    }
    let mut failing_request = failing_request.clone();
    if !apply_request_bindings(&mut failing_request, &outputs) {
        return Ok(false);
    }
    let result = invoke(client, failing_endpoint, failing_request).await?;
    if has_fingerprint(&result, expected) {
        return Ok(true);
    }
    contracts.push(failing_endpoint.contract.clone());
    append_sequence_events(&mut events, result.events, setup.len());
    let config = BackendConfig {
        enabled: true,
        operations: contracts,
        invariants: failing_endpoint.policy.invariants.clone(),
        resources: failing_endpoint.policy.resources.clone(),
        proofs: failing_endpoint.policy.proofs.clone(),
        fleet: failing_endpoint.policy.fleet.clone(),
        ..BackendConfig::default()
    };
    Ok(backend::evaluate(&config, &events)
        .iter()
        .any(|violation| violation.fingerprint == expected))
}

pub(super) fn apply_request_bindings(request: &mut RequestArtifact, outputs: &[Value]) -> bool {
    for binding in request.bindings.clone() {
        let Some(value) = outputs
            .get(binding.source_step)
            .and_then(|output| json_path_value(output, &binding.source_output_path))
            .filter(|value| is_scalar_identity(value))
            .cloned()
        else {
            return false;
        };
        if !rebind_request_input(request, &binding.input_path, value) {
            return false;
        }
    }
    true
}

pub(super) fn rebind_request_input(
    request: &mut RequestArtifact,
    path: &str,
    replacement: Value,
) -> bool {
    let Some(previous) = json_path_value(&request.input, path).cloned() else {
        return false;
    };
    if !is_scalar_identity(&previous)
        || !set_json_path(&mut request.input, path, replacement.clone())
    {
        return false;
    }
    if request.method == "GRPC" {
        request.body = Some(request.input.clone());
        return true;
    }
    if request
        .body
        .as_ref()
        .is_some_and(|body| body.get("query").is_some())
    {
        if let Some(body) = request.body.as_mut().and_then(Value::as_object_mut) {
            body.insert("variables".into(), request.input.clone());
            return true;
        }
        return false;
    }

    if let Some(body) = request.input.get("body") {
        request.body = (!body.is_null()).then(|| body.clone());
    } else if request.body.is_some() {
        request.body = (!request.input.is_null()).then(|| request.input.clone());
    }
    if let Some(headers) = request.input.get("header").and_then(Value::as_object) {
        for (name, value) in headers {
            let Some(value) = value_as_text(value) else {
                return false;
            };
            request.headers.insert(name.clone(), value);
        }
    }

    let Ok(mut url) = request.url.parse::<reqwest::Url>() else {
        return false;
    };
    if path_contains_group(path, "path") {
        let Some(previous) = value_as_text(&previous) else {
            return false;
        };
        let Some(replacement) = value_as_text(&replacement) else {
            return false;
        };
        let segments = url
            .path_segments()
            .map(|segments| segments.map(str::to_string).collect::<Vec<_>>())
            .unwrap_or_default();
        let mut replaced = false;
        let rewritten = segments
            .into_iter()
            .map(|segment| {
                if segment == previous {
                    replaced = true;
                    replacement.clone()
                } else {
                    segment
                }
            })
            .collect::<Vec<_>>();
        if !replaced {
            return false;
        }
        let Ok(mut path) = url.path_segments_mut() else {
            return false;
        };
        path.clear();
        path.extend(rewritten);
        drop(path);
    }
    if path_contains_group(path, "query") {
        let Some(query) = request.input.get("query").and_then(Value::as_object) else {
            return false;
        };
        url.query_pairs_mut().clear().extend_pairs(
            query
                .iter()
                .filter_map(|(name, value)| value_as_text(value).map(|value| (name, value))),
        );
    }
    request.url = url.into();
    true
}

pub(super) fn path_contains_group(path: &str, group: &str) -> bool {
    path.trim_start_matches('$')
        .trim_start_matches('.')
        .split('.')
        .next()
        == Some(group)
}

pub(super) fn append_sequence_events(
    destination: &mut Vec<BackendEvent>,
    mut events: Vec<BackendEvent>,
    step: usize,
) {
    for event in &mut events {
        event.sequence = destination.len() as u64 + 1;
        event.trace_id = "reproit-lifecycle".into();
        event.span_id = format!("step-{step}");
    }
    destination.extend(events);
}
