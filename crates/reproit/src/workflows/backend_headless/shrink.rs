use super::*;

pub(super) async fn shrink_findings(
    client: &reqwest::Client,
    base_url: &str,
    findings: Vec<FindingCase>,
) -> Result<Vec<FindingCase>> {
    let mut shrunk = Vec::with_capacity(findings.len());
    for (endpoint, mut request, mut setup, finding) in findings {
        let expected = finding
            .get("fingerprint")
            .and_then(Value::as_str)
            .context("backend finding has no fingerprint")?;
        if std::env::var_os("REPROIT_BACKEND_RESET_URL").is_some() {
            let mut index = 0;
            while index < setup.len() {
                let mut candidate = setup.clone();
                candidate.remove(index);
                if replay_sequence(client, &candidate, &endpoint, &request, expected).await? {
                    setup = candidate;
                } else {
                    index += 1;
                }
            }
        }
        let safe_to_repeat =
            endpoint.contract.read_only || std::env::var_os("REPROIT_BACKEND_RESET_URL").is_some();
        if safe_to_repeat {
            loop {
                let mut accepted = None;
                for input in structural_reductions(&request.input)
                    .into_iter()
                    .take(MAX_REDUCTIONS_PER_PASS)
                {
                    let Ok(mut candidate) = build_request(&endpoint, base_url, input) else {
                        continue;
                    };
                    candidate.bindings = request.bindings.clone();
                    if replay_sequence(client, &setup, &endpoint, &candidate, expected).await? {
                        accepted = Some(candidate);
                        break;
                    }
                }
                let Some(candidate) = accepted else {
                    break;
                };
                request = candidate;
            }
        }
        if !replay_sequence(client, &setup, &endpoint, &request, expected).await? {
            bail!("shrunk backend reproduction failed final exact verification");
        }
        shrunk.push((endpoint, request, setup, finding));
    }
    Ok(shrunk)
}

fn structural_reductions(value: &Value) -> Vec<Value> {
    let mut reductions = Vec::new();
    match value {
        Value::Object(object) => {
            for key in object.keys() {
                let mut candidate = object.clone();
                candidate.remove(key);
                reductions.push(Value::Object(candidate));
            }
            for (key, child) in object {
                for reduced in structural_reductions(child) {
                    let mut candidate = object.clone();
                    candidate.insert(key.clone(), reduced);
                    reductions.push(Value::Object(candidate));
                }
            }
        }
        Value::Array(values) => {
            for index in 0..values.len() {
                let mut candidate = values.clone();
                candidate.remove(index);
                reductions.push(Value::Array(candidate));
            }
            for (index, child) in values.iter().enumerate() {
                for reduced in structural_reductions(child) {
                    let mut candidate = values.clone();
                    candidate[index] = reduced;
                    reductions.push(Value::Array(candidate));
                }
            }
        }
        Value::String(value) if !value.is_empty() => {
            reductions.push(Value::String(String::new()));
            if value.chars().count() > 1 {
                reductions.push(Value::String(
                    value.chars().take(value.chars().count() / 2).collect(),
                ));
            }
        }
        Value::Number(value) => {
            for candidate in [Value::from(0), Value::from(1)] {
                if candidate != Value::Number(value.clone()) {
                    reductions.push(candidate);
                }
            }
        }
        Value::Bool(true) => reductions.push(Value::Bool(false)),
        Value::Null | Value::Bool(false) | Value::String(_) => {}
    }
    let mut seen = BTreeSet::new();
    let original_score = structural_score(value);
    reductions.retain(|candidate| {
        structural_score(candidate) < original_score && seen.insert(canonical_value(candidate))
    });
    reductions.sort_by_key(structural_score);
    reductions
}

fn canonical_value(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

fn structural_score(value: &Value) -> (usize, usize, String) {
    fn nodes(value: &Value) -> usize {
        1 + match value {
            Value::Array(values) => values.iter().map(nodes).sum(),
            Value::Object(values) => values.values().map(nodes).sum(),
            _ => 0,
        }
    }
    let canonical = canonical_value(value);
    (nodes(value), canonical.len(), canonical)
}
