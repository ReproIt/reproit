use super::*;

pub(super) fn build_request(
    endpoint: &Endpoint,
    base_url: &str,
    input: Value,
) -> Result<RequestArtifact> {
    if endpoint.transport == Transport::Grpc {
        return Ok(RequestArtifact {
            operation: endpoint.contract.id.clone(),
            method: "GRPC".into(),
            url: base_url.to_string(),
            input: input.clone(),
            headers: BTreeMap::new(),
            body: Some(input),
            content_type: Some("application/json".into()),
            schema_source: endpoint.schema_source.clone(),
            client_streaming: endpoint.client_streaming,
            server_streaming: endpoint.server_streaming,
            bindings: Vec::new(),
        });
    }
    if endpoint.response_field.is_some() {
        return Ok(RequestArtifact {
            operation: endpoint.contract.id.clone(),
            method: "POST".into(),
            url: base_url.to_string(),
            input: input.clone(),
            headers: BTreeMap::new(),
            body: Some(json!({"query": endpoint.path, "variables": input})),
            content_type: Some("application/json".into()),
            schema_source: None,
            client_streaming: false,
            server_streaming: false,
            bindings: Vec::new(),
        });
    }
    let mut path = endpoint.path.clone();
    let mut headers = BTreeMap::new();
    let mut query = Vec::new();
    let mut body = None;
    if endpoint.body_only {
        if !input.is_null() {
            body = Some(input.clone());
        }
    } else if let Some(groups) = input.as_object() {
        if let Some(values) = groups.get("path").and_then(Value::as_object) {
            for (name, value) in values {
                let text = value_as_text(value).context("path parameter is not scalar")?;
                path = path.replace(&format!("{{{name}}}"), &percent_encode(&text));
            }
        }
        if let Some(values) = groups.get("query").and_then(Value::as_object) {
            for (name, value) in values {
                query.push((
                    name.clone(),
                    value_as_text(value).context("query parameter is not scalar")?,
                ));
            }
        }
        if let Some(values) = groups.get("headers").and_then(Value::as_object) {
            for (name, value) in values {
                headers.insert(
                    name.clone(),
                    value_as_text(value).context("header parameter is not scalar")?,
                );
            }
        }
        body = groups.get("body").cloned();
    }
    if path.contains('{') {
        bail!(
            "could not synthesize every path parameter for {}",
            endpoint.contract.id
        );
    }
    let mut url = format!("{base_url}/{}", path.trim_start_matches('/')).parse::<reqwest::Url>()?;
    if !query.is_empty() {
        url.query_pairs_mut().extend_pairs(query);
    }
    Ok(RequestArtifact {
        operation: endpoint.contract.id.clone(),
        method: endpoint.method.clone(),
        url: url.to_string(),
        input,
        headers,
        body,
        content_type: endpoint.content_type.clone(),
        schema_source: None,
        client_streaming: false,
        server_streaming: false,
        bindings: Vec::new(),
    })
}
