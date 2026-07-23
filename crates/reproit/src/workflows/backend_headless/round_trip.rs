//! Schema-inferred round-trip prober for the DATA-LOSS oracle: for a
//! (GET, PATCH) pair on the same identified path, drive the quad the oracle
//! recognizes (read, read, single-field patch, read) against a resource THIS
//! RUN created, then let `backend::evaluate` judge the accumulated event
//! sequence. Confirmation IS replay: `replay_sequence` re-creates a fresh
//! resource, rebinds its identity, replays the quad, and must regenerate the
//! exact fingerprint. Only findings that survive that are published.
//!
//! Safety: runs only in fuzz mode, which already gates mutation behind a
//! loopback target or an explicit `--yes`. Only resources created by this
//! run's own POSTs are ever patched (never pre-existing data), and every
//! non-probed body field is re-sent with its currently stored value.

use super::*;

const MAX_PROBES: usize = 3;
const MAX_FIELDS_PER_PAIR: usize = 3;
const MAX_CREATES: usize = 8;

/// A clean POST this run performed: the source of probe-able resources.
pub(super) struct CreateRecord {
    pub(super) endpoint: Endpoint,
    pub(super) request: RequestArtifact,
    pub(super) output: Value,
}

pub(super) struct RoundTripRun {
    pub(super) findings: Vec<FindingCase>,
    pub(super) candidates: Vec<Value>,
    pub(super) skipped: Vec<Value>,
    pub(super) exercised: usize,
    pub(super) rejected: usize,
}

pub(super) fn record_create(creates: &mut Vec<CreateRecord>, record: CreateRecord) {
    if creates.len() < MAX_CREATES && record.output.is_object() {
        creates.push(record);
    }
}

/// The single `{param}` name of an identified path like `/notes/{id}`.
fn path_param(path: &str) -> Option<&str> {
    let start = path.find('{')?;
    let end = path[start..].find('}')? + start;
    let name = &path[start + 1..end];
    // Exactly one parameter: multi-param paths are out of scope for v1.
    if name.is_empty() || path[end..].contains('{') {
        return None;
    }
    Some(name)
}

/// The scalar identity in a create output, tried at the conventional spots.
fn create_identity(output: &Value, param: &str) -> Option<(String, Value)> {
    for path in [param, "id", "data.id"] {
        if let Some(value) = json_path_value(output, path).filter(|v| is_scalar_identity(v)) {
            return Some((path.to_string(), value.clone()));
        }
    }
    None
}

/// A scalar probe value from the field's domain that differs from `current`.
fn fresh_value(domain: &ValueDomain, current: &Value, seed: u64) -> Option<Value> {
    if let Value::Bool(b) = current {
        return Some(Value::Bool(!b));
    }
    for offset in [7u64, 13, 29] {
        let candidate = sample_domain(domain, seed + offset, false, 0);
        if !candidate.is_object() && !candidate.is_array() && &candidate != current {
            return Some(candidate);
        }
    }
    None
}

pub(super) async fn probe_round_trips(
    client: &reqwest::Client,
    endpoints: &[Endpoint],
    base_url: &str,
    seed: u64,
    creates: &[CreateRecord],
) -> Result<RoundTripRun> {
    let mut run = RoundTripRun {
        findings: Vec::new(),
        candidates: Vec::new(),
        skipped: Vec::new(),
        exercised: 0,
        rejected: 0,
    };
    let pairs: Vec<(&Endpoint, &Endpoint)> = endpoints
        .iter()
        .filter(|e| e.method == "GET" && path_param(&e.path).is_some())
        .filter_map(|get| {
            endpoints
                .iter()
                .find(|p| p.method == "PATCH" && p.path == get.path)
                .map(|patch| (get, patch))
        })
        .collect();
    let mut probes = 0usize;
    for (get, patch) in pairs {
        if probes >= MAX_PROBES {
            break;
        }
        let param = path_param(&get.path).expect("filtered identified paths");
        // Only resources THIS RUN created, matched by path prefix.
        let Some((create, id_path, identity)) = creates.iter().find_map(|record| {
            let suffix = get.path.strip_prefix(&record.endpoint.path)?;
            if !(suffix.starts_with("/{") && suffix.ends_with('}')) {
                return None;
            }
            let (path, value) = create_identity(&record.output, param)?;
            Some((record, path, value))
        }) else {
            continue;
        };
        probes += 1;

        // The list no-shrink probe derives its patch field straight from the
        // PATCH schema and creates its own fresh resources, so it runs
        // independently of the getNote round-trip below (which may bail if the
        // target mutated its own collection during the main fuzz loop).
        let list_field: Option<(String, Value)> = {
            let body_domain = if patch.body_only {
                patch.contract.input.as_ref()
            } else {
                match patch.contract.input.as_ref() {
                    Some(ValueDomain::Object { properties, .. }) => properties.get("body"),
                    _ => None,
                }
            };
            match body_domain {
                Some(ValueDomain::Object { properties, .. }) => {
                    properties.iter().find_map(|(name, domain)| {
                        if name == param {
                            return None;
                        }
                        let sample = sample_domain(domain, seed + 61, false, 0);
                        if sample.is_object() || sample.is_array() {
                            None
                        } else {
                            Some((name.clone(), sample))
                        }
                    })
                }
                _ => None,
            }
        };
        // COLLECTION NO-SHRINK: create TWO fresh resources, then run a
        // list quad (list, list, patch one, list). An id the patch never
        // referenced vanishing from the listing is sibling deletion (the
        // Insomnia class). Self-contained fresh creates make the probe robust
        // to the target mutating its own collection during the main fuzz loop.
        let list_get = endpoints
            .iter()
            .find(|e| e.method == "GET" && e.path == create.endpoint.path);
        if let (Some(list), Some((field, fresh))) = (list_get, list_field.clone()) {
            let list_read = |sample_seed: u64| -> Result<RequestArtifact> {
                let input = list
                    .contract
                    .input
                    .as_ref()
                    .map(|domain| sample_domain(domain, sample_seed, false, 0))
                    .unwrap_or(Value::Null);
                build_request(list, base_url, input)
            };
            let listable = |result: &InvocationResult| {
                (200..400).contains(&result.status) && result.violations.is_empty()
            };
            // Two fresh resources so the collection has a known unreferenced
            // sibling to watch. Both created from the same POST contract.
            let c1 = invoke(client, &create.endpoint, create.request.clone()).await?;
            let c2 = invoke(client, &create.endpoint, create.request.clone()).await?;
            run.exercised += 2;
            let sibling_id =
                create_identity(&c1.output, param).filter(|_| listable(&c1) && listable(&c2));
            if let Some((sibling_id_path, sibling_identity)) = sibling_id {
                let mut setup = vec![
                    ReplayStep {
                        contract: create.endpoint.contract.clone(),
                        request: create.request.clone(),
                        policy: create.endpoint.policy.clone(),
                    },
                    ReplayStep {
                        contract: create.endpoint.contract.clone(),
                        request: create.request.clone(),
                        policy: create.endpoint.policy.clone(),
                    },
                ];
                let mut sequence = Vec::new();
                let list_step = |setup: &mut Vec<ReplayStep>, request: &RequestArtifact| {
                    let step = setup.len();
                    setup.push(ReplayStep {
                        contract: list.contract.clone(),
                        request: request.clone(),
                        policy: list.policy.clone(),
                    });
                    step
                };
                let request_a = list_read(seed + 5)?;
                let step_a = list_step(&mut setup, &request_a);
                let result_a = invoke(client, list, request_a).await?;
                let request_b = list_read(seed + 5)?;
                let step_b = list_step(&mut setup, &request_b);
                let result_b = invoke(client, list, request_b).await?;
                run.exercised += 2;
                if listable(&result_a) && listable(&result_b) {
                    append_sequence_events(&mut sequence, result_a.events, step_a);
                    append_sequence_events(&mut sequence, result_b.events, step_b);
                    let mut body_map = Map::new();
                    body_map.insert(field.clone(), fresh.clone());
                    let mut path_map = Map::new();
                    path_map.insert(param.to_string(), sibling_identity.clone());
                    let mut input_map = Map::new();
                    input_map.insert("path".into(), Value::Object(path_map));
                    input_map.insert("body".into(), Value::Object(body_map));
                    let mut patch_request =
                        build_request(patch, base_url, Value::Object(input_map))?;
                    // The patch targets the FIRST create (step 0); the second
                    // create (step 1) is the unreferenced sibling watched.
                    patch_request.bindings.push(RequestBinding {
                        source_step: 0,
                        source_output_path: sibling_id_path.clone(),
                        input_path: format!("path.{param}"),
                    });
                    let patch_step = setup.len();
                    setup.push(ReplayStep {
                        contract: patch.contract.clone(),
                        request: patch_request.clone(),
                        policy: patch.policy.clone(),
                    });
                    let patch_result = invoke(client, patch, patch_request).await?;
                    run.exercised += 1;
                    if (200..400).contains(&patch_result.status)
                        && patch_result.violations.is_empty()
                    {
                        append_sequence_events(&mut sequence, patch_result.events, patch_step);
                        let check_request = list_read(seed + 5)?;
                        let check_result = invoke(client, list, check_request.clone()).await?;
                        run.exercised += 1;
                        if listable(&check_result) {
                            append_sequence_events(&mut sequence, check_result.events, setup.len());
                            let config = BackendConfig {
                                enabled: true,
                                operations: vec![
                                    create.endpoint.contract.clone(),
                                    list.contract.clone(),
                                    patch.contract.clone(),
                                ],
                                ..BackendConfig::default()
                            };
                            for violation in backend::evaluate(&config, &sequence)
                                .into_iter()
                                .filter(|violation| violation.oracle == "data-loss")
                            {
                                let finding = backend::finding(&violation);
                                if replay_sequence(
                                    client,
                                    &setup,
                                    list,
                                    &check_request,
                                    &violation.fingerprint,
                                )
                                .await?
                                {
                                    run.findings.push((
                                        list.clone(),
                                        check_request.clone(),
                                        setup.clone(),
                                        finding,
                                    ));
                                } else {
                                    run.candidates.push(json!({
                                        "operation": patch.contract.id,
                                        "reason": violation.reason,
                                        "confirmation": "did not reproduce on fresh resources",
                                    }));
                                }
                            }
                        }
                    }
                }
            }
        }

        let bound_read = |sample_seed: u64| -> Result<RequestArtifact> {
            let mut input = get
                .contract
                .input
                .as_ref()
                .map(|domain| sample_domain(domain, sample_seed, false, 0))
                .unwrap_or_else(|| json!({}));
            if input.is_null() {
                input = json!({});
            }
            let group = input
                .as_object_mut()
                .context("read input is not an object")?
                .entry("path")
                .or_insert_with(|| json!({}));
            group
                .as_object_mut()
                .context("read path group is not an object")?
                .insert(param.to_string(), identity.clone());
            let mut request = build_request(get, base_url, input)?;
            request.bindings.push(RequestBinding {
                source_step: 0,
                source_output_path: id_path.clone(),
                input_path: format!("path.{param}"),
            });
            Ok(request)
        };

        // Baseline reads once per pair to find the stable, writable fields;
        // each field then gets its own independent quad from the shared
        // create step, so every finding's replay setup stays minimal.
        let probe_a = invoke(client, get, bound_read(seed + 3)?).await?;
        let probe_b = invoke(client, get, bound_read(seed + 3)?).await?;
        run.exercised += 2;
        let readable = |result: &InvocationResult| {
            (200..400).contains(&result.status)
                && result.violations.is_empty()
                && result.output.is_object()
        };
        if !readable(&probe_a) || !readable(&probe_b) {
            run.rejected += 1;
            run.skipped.push(json!({
                "operation": get.contract.id,
                "reason": "round-trip baseline reads did not complete cleanly",
            }));
            continue;
        }
        let stored = probe_b.output.as_object().expect("checked readable");
        let baseline = probe_a.output.as_object().expect("checked readable");
        // The writable fields live in the input's body group (or the whole
        // input for body-only transports).
        let body_domain = if patch.body_only {
            patch.contract.input.as_ref()
        } else {
            match patch.contract.input.as_ref() {
                Some(ValueDomain::Object { properties, .. }) => properties.get("body"),
                _ => None,
            }
        };
        let Some(ValueDomain::Object {
            properties,
            required,
            ..
        }) = body_domain
        else {
            continue;
        };
        let fields: Vec<(String, Value)> = properties
            .iter()
            .filter_map(|(name, domain)| {
                if name == param || !stored.contains_key(name) {
                    return None;
                }
                let current = &stored[name];
                if current.is_object() || current.is_array() || baseline.get(name) != Some(current)
                {
                    return None;
                }
                fresh_value(domain, current, seed).map(|value| (name.clone(), value))
            })
            .take(MAX_FIELDS_PER_PAIR)
            .collect();
        if fields.is_empty() {
            run.skipped.push(json!({
                "operation": patch.contract.id,
                "reason": "no stable scalar field is writable per the PATCH schema",
            }));
            continue;
        }

        for (field, fresh) in fields {
            let mut setup = vec![ReplayStep {
                contract: create.endpoint.contract.clone(),
                request: create.request.clone(),
                policy: create.endpoint.policy.clone(),
            }];
            let mut sequence = Vec::new();
            let read_step = |setup: &mut Vec<ReplayStep>, request: &RequestArtifact| -> usize {
                let step = setup.len();
                setup.push(ReplayStep {
                    contract: get.contract.clone(),
                    request: request.clone(),
                    policy: get.policy.clone(),
                });
                step
            };

            let request_a = bound_read(seed + 3)?;
            let step_a = read_step(&mut setup, &request_a);
            let result_a = invoke(client, get, request_a).await?;
            let request_b = bound_read(seed + 3)?;
            let step_b = read_step(&mut setup, &request_b);
            let result_b = invoke(client, get, request_b).await?;
            run.exercised += 2;
            if !readable(&result_a) || !readable(&result_b) {
                run.rejected += 1;
                continue;
            }
            append_sequence_events(&mut sequence, result_a.events, step_a);
            append_sequence_events(&mut sequence, result_b.events, step_b);
            let stored = result_b.output.as_object().expect("checked readable");

            // Required body fields ride along with their CURRENTLY stored
            // values, so the probe writes exactly one field's worth of change.
            let mut body_map = Map::new();
            for name in required {
                if let Some(value) = stored.get(name) {
                    body_map.insert(name.clone(), value.clone());
                }
            }
            body_map.insert(field.clone(), fresh.clone());
            let mut path_map = Map::new();
            path_map.insert(param.to_string(), identity.clone());
            let mut input_map = Map::new();
            input_map.insert("path".into(), Value::Object(path_map));
            input_map.insert("body".into(), Value::Object(body_map));
            let patch_input = Value::Object(input_map);
            let mut patch_request = build_request(patch, base_url, patch_input)?;
            patch_request.bindings.push(RequestBinding {
                source_step: 0,
                source_output_path: id_path.clone(),
                input_path: format!("path.{param}"),
            });
            let patch_step = setup.len();
            setup.push(ReplayStep {
                contract: patch.contract.clone(),
                request: patch_request.clone(),
                policy: patch.policy.clone(),
            });
            let patch_result = invoke(client, patch, patch_request).await?;
            run.exercised += 1;
            if !(200..400).contains(&patch_result.status) || !patch_result.violations.is_empty() {
                // A rejected or otherwise-flagged PATCH is not a round-trip
                // verdict; other oracles own it.
                run.rejected += 1;
                continue;
            }
            append_sequence_events(&mut sequence, patch_result.events, patch_step);

            let check_request = bound_read(seed + 3)?;
            let check_result = invoke(client, get, check_request.clone()).await?;
            run.exercised += 1;
            if !readable(&check_result) {
                run.rejected += 1;
                continue;
            }
            append_sequence_events(&mut sequence, check_result.events, setup.len());

            let config = BackendConfig {
                enabled: true,
                operations: vec![
                    create.endpoint.contract.clone(),
                    get.contract.clone(),
                    patch.contract.clone(),
                ],
                ..BackendConfig::default()
            };
            for violation in backend::evaluate(&config, &sequence)
                .into_iter()
                .filter(|violation| violation.oracle == "data-loss")
            {
                let finding = backend::finding(&violation);
                // Confirmation IS replay: a fresh resource, rebound identity,
                // the same quad, the same fingerprint.
                if replay_sequence(client, &setup, get, &check_request, &violation.fingerprint)
                    .await?
                {
                    run.findings
                        .push((get.clone(), check_request.clone(), setup.clone(), finding));
                } else {
                    run.candidates.push(json!({
                        "operation": patch.contract.id,
                        "reason": violation.reason,
                        "confirmation": "did not reproduce on a fresh resource",
                    }));
                }
            }
        }
    }
    Ok(run)
}
