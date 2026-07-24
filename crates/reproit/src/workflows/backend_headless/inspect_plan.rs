//! Source resolution and step planning for backend inspection.
//!
//! Pure planning lives here: resolving what a `reproit inspect` reference is
//! (capture file, finding artifact, capture-bearing production bucket),
//! turning it into a bounded sequence of planned steps, and attaching live
//! requests when a configured target is available. The interactive session
//! loop lives in `inspect.rs`.

use super::inspect_report::MAX_INSPECT_STEPS;
use super::*;
use crate::domain::backend::BackendProofContract;
use crate::workflows::backend_target;

pub(super) enum Source {
    Capture {
        label: String,
        artifact: capture_replay::CaptureArtifact,
    },
    Finding {
        id: String,
        artifact: Box<BackendFindingArtifact>,
    },
}

pub(super) async fn resolve_source(
    ctx: &Ctx,
    config_path: Option<&Path>,
    reference: &str,
) -> Result<Option<Source>> {
    let path = Path::new(reference);
    if path.is_file() {
        let artifact = capture_replay::parse_capture(
            &std::fs::read(path).with_context(|| format!("read {}", path.display()))?,
        )?;
        return Ok(Some(Source::Capture {
            label: format!("capture file {}", path.display()),
            artifact,
        }));
    }
    if let Some(raw_id) = repro::raw_finding_id(reference) {
        let Some(artifact_path) = replay_command::find_artifact(raw_id)? else {
            return Ok(None);
        };
        if artifact_path.file_name().and_then(|name| name.to_str()) == Some("backend-schema.json") {
            bail!(
                "{reference} is a schema finding; it replays deterministically with \
                 `reproit {reference}` and has no operation sequence to inspect"
            );
        }
        let artifact: BackendFindingArtifact =
            serde_json::from_slice(&std::fs::read(&artifact_path)?)?;
        return Ok(Some(Source::Finding {
            id: reference.to_string(),
            artifact: Box::new(artifact),
        }));
    }
    let backend_project = backend_target::resolve(config_path)?.is_some();
    if reference.starts_with("bkt_") {
        // On a UI-configured project the UI path owns bucket pulls. Backend
        // projects (and projects with no UI config) look for a production
        // capture payload inside the bucket package first.
        if !backend_project && crate::adapters::config::load(config_path).is_ok() {
            return Ok(None);
        }
        let (cloud, key) = crate::workflows::cloud::cloud_creds(None, None);
        let (_, package) = crate::workflows::triage::fetch_bucket_package(reference, cloud, key)
            .await
            .with_context(|| format!("pulling bucket {reference}"))?;
        let Some(capture) = package.pointer("/context/reproitCapture") else {
            if backend_project {
                bail!(
                    "bucket {reference} carries no backend capture payload \
                     (context.reproitCapture); nothing to inspect on a backend project"
                );
            }
            return Ok(None);
        };
        let bytes = serde_json::to_vec(capture)?;
        let artifact = capture_replay::parse_capture(&bytes)?;
        let directory = std::env::current_dir()?
            .join(".reproit/pulls")
            .join(reference);
        std::fs::create_dir_all(&directory)?;
        let saved = directory.join("capture.json");
        std::fs::write(&saved, &bytes)?;
        ctx.say(format!(
            "Pulled the production capture of {reference} to {}.",
            saved.display()
        ));
        return Ok(Some(Source::Capture {
            label: format!("production bucket {reference}"),
            artifact,
        }));
    }
    if backend_project {
        bail!(
            "backend projects inspect a finding id (fnd_...), a production bucket id \
             (bkt_...), or a captured-production payload file; `{reference}` is none of these"
        );
    }
    Ok(None)
}

pub(super) enum Expected {
    Fingerprint(String),
    CaptureTarget { operation: String, oracle: String },
}

impl Expected {
    pub(super) fn matches(&self, violation: &BackendViolation) -> bool {
        match self {
            Expected::Fingerprint(fingerprint) => violation.fingerprint == *fingerprint,
            Expected::CaptureTarget { operation, oracle } => {
                violation.operation == *operation
                    && backend::finding(violation)["oracle"].as_str() == Some(oracle)
            }
        }
    }

    pub(super) fn describe(&self) -> String {
        match self {
            Expected::Fingerprint(fingerprint) => format!("finding fingerprint {fingerprint}"),
            Expected::CaptureTarget { operation, oracle } => {
                format!("{oracle} on {operation}")
            }
        }
    }
}

pub(super) struct LiveMember {
    pub(super) endpoint: Endpoint,
    pub(super) request: RequestArtifact,
    /// Finding setup steps must replay cleanly before the failing request.
    pub(super) setup: bool,
}

pub(super) struct PlannedMember {
    pub(super) operation: String,
    /// This invocation's recorded events (empty for finding artifacts).
    pub(super) recorded: Vec<BackendEvent>,
    pub(super) live: Option<LiveMember>,
}

pub(super) struct PlannedStep {
    pub(super) label: String,
    pub(super) grouped: bool,
    /// Offline reveal boundary into the sorted recorded stream.
    pub(super) cut: usize,
    pub(super) members: Vec<PlannedMember>,
}

/// Resolve the configured schema target and attach a live request to every
/// planned member. `Ok(Err(reason))` means live mode is unavailable and the
/// caller falls back to offline stepping with that message.
pub(super) async fn attach_capture_target(
    client: &reqwest::Client,
    config_path: Option<&Path>,
    steps: &mut [PlannedStep],
) -> Result<std::result::Result<String, String>> {
    let Some((schema, declared)) = backend_target::resolve(config_path)? else {
        return Ok(Err(
            "no backend target is configured (reproit.yaml backend.schemas)".to_string(),
        ));
    };
    let document = load_document(&schema)?;
    let openapi = document.get("openapi").is_some() || document.get("swagger").is_some();
    let graphql =
        document.pointer("/data/__schema").is_some() || document.get("__schema").is_some();
    let grpc = document.get("file").is_some() || document.get("files").is_some();
    if !openapi && !graphql && !grpc {
        bail!("backend schema is not OpenAPI, GraphQL, or a protobuf descriptor");
    }
    let mut endpoints = if openapi {
        openapi_endpoints(&document)
    } else if graphql {
        graphql_endpoints(&document)
    } else {
        grpc_endpoints(&document)
    };
    if endpoints.is_empty() {
        bail!("the configured backend schema contains no executable operations");
    }
    if endpoints.len() > MAX_ENDPOINTS {
        bail!(
            "backend schema has {} executable operations; safety limit is {MAX_ENDPOINTS}",
            endpoints.len()
        );
    }
    let policy = BackendPolicy {
        invariants: declared.invariants.clone(),
        resources: declared.resources.clone(),
        proofs: declared.proofs.clone(),
        fleet: declared.fleet.clone(),
    };
    for endpoint in &mut endpoints {
        if let Some(contract) = declared
            .operations
            .iter()
            .find(|contract| contract.id == endpoint.contract.id)
        {
            apply_operation_override(&mut endpoint.contract, contract);
        }
        endpoint.policy = policy.clone();
        if grpc && schema.extension().and_then(|value| value.to_str()) == Some("proto") {
            endpoint.schema_source = Some(schema.canonicalize()?);
        }
    }
    let base_url = service_base_url(&document)?;
    let mut requests = Vec::new();
    for step in steps.iter() {
        for member in &step.members {
            let Some(endpoint) = unique_endpoint(&endpoints, &member.operation) else {
                return Ok(Err(format!(
                    "captured operation `{}` is missing or ambiguous in the configured schema",
                    member.operation
                )));
            };
            let Some(input) = recorded_input(&member.recorded) else {
                return Ok(Err(format!(
                    "captured operation `{}` has no start event to rebuild a request from",
                    member.operation
                )));
            };
            // Body-only endpoints take the request body directly, everything
            // else takes the grouped {path, query, headers, body} shape the
            // SDK middleware records.
            let mut input = sanitize_capture_input(input);
            if endpoint.body_only {
                input = input.get("body").cloned().unwrap_or(Value::Null);
            }
            match build_request(endpoint, &base_url, input) {
                Ok(request) => requests.push((endpoint.clone(), request)),
                Err(error) => {
                    return Ok(Err(format!(
                        "cannot rebuild the `{}` request from the capture: {error}",
                        member.operation
                    )))
                }
            }
        }
    }
    if !probe_target(client, &base_url).await {
        return Ok(Err(format!(
            "the configured backend target {base_url} is not reachable"
        )));
    }
    let mut requests = requests.into_iter();
    for step in steps.iter_mut() {
        for member in &mut step.members {
            let (endpoint, request) = requests.next().expect("one request per member");
            member.live = Some(LiveMember {
                endpoint,
                request,
                setup: false,
            });
        }
    }
    Ok(Ok(base_url))
}

pub(super) fn recorded_input(events: &[BackendEvent]) -> Option<&Value> {
    events.iter().find_map(|event| match &event.event {
        BackendEventKind::Start { input } => Some(input),
        _ => None,
    })
}

pub(super) fn recorded_return(events: &[BackendEvent]) -> (Option<u16>, Value) {
    events
        .iter()
        .find_map(|event| match &event.event {
            BackendEventKind::Return { output, status, .. } => Some((*status, output.clone())),
            _ => None,
        })
        .unwrap_or((None, Value::Null))
}

/// Re-sent captured requests must not replay hop-by-hop or correlation
/// headers; the inspection sets its own trace identity.
fn sanitize_capture_input(input: &Value) -> Value {
    let mut input = input.clone();
    if let Some(headers) = input.get_mut("headers").and_then(Value::as_object_mut) {
        headers.retain(|name, _| {
            let name = name.to_ascii_lowercase();
            !matches!(
                name.as_str(),
                "host" | "content-length" | "transfer-encoding" | "connection" | "accept-encoding"
            ) && !name.starts_with("x-reproit-")
        });
    }
    input
}

pub(super) fn origin_url(url: &str) -> Result<String> {
    let mut url = url.parse::<reqwest::Url>()?;
    url.set_path("/");
    url.set_query(None);
    Ok(url.to_string())
}

/// Reachability probe: any HTTP response (even an error status) proves the
/// target is up; only a transport failure reads as not bootable.
pub(super) async fn probe_target(client: &reqwest::Client, base_url: &str) -> bool {
    client
        .get(base_url)
        .timeout(Duration::from_secs(3))
        .send()
        .await
        .is_ok()
}

struct RecordedInvocation {
    operation: String,
    first: usize,
    last: usize,
    events: Vec<BackendEvent>,
}

fn recorded_invocations(events: &[BackendEvent]) -> Vec<RecordedInvocation> {
    let mut order: Vec<(String, String)> = Vec::new();
    let mut grouped: BTreeMap<(String, String), (usize, usize, Vec<BackendEvent>)> =
        BTreeMap::new();
    for (index, event) in events.iter().enumerate() {
        let key = (event.trace_id.clone(), event.span_id.clone());
        match grouped.get_mut(&key) {
            Some((_, last, members)) => {
                *last = index;
                members.push(event.clone());
            }
            None => {
                order.push(key.clone());
                grouped.insert(key, (index, index, vec![event.clone()]));
            }
        }
    }
    let mut invocations: Vec<RecordedInvocation> = order
        .into_iter()
        .map(|key| {
            let (first, last, events) = grouped.remove(&key).expect("keyed above");
            RecordedInvocation {
                operation: events[0].operation.clone(),
                first,
                last,
                events,
            }
        })
        .collect();
    invocations.sort_by_key(|invocation| invocation.last);
    invocations
}

fn concurrent_operations(proofs: &[BackendProofContract]) -> BTreeSet<&str> {
    proofs
        .iter()
        .filter_map(|proof| match proof {
            BackendProofContract::ConcurrentUpdate { operation, .. } => Some(operation.as_str()),
            _ => None,
        })
        .collect()
}

/// One planned step per recorded invocation, in return order, except that
/// overlapping invocations of an operation named by a concurrent-update proof
/// advance together as one grouped multi-actor step.
pub(super) fn plan_capture_steps(
    events: &[BackendEvent],
    proofs: &[BackendProofContract],
) -> Result<Vec<PlannedStep>> {
    let invocations = recorded_invocations(events);
    if invocations.is_empty() {
        bail!("capture has no events to step through");
    }
    let concurrent = concurrent_operations(proofs);
    let mut steps: Vec<(usize, usize, Vec<RecordedInvocation>)> = Vec::new();
    for invocation in invocations {
        if let Some((first, last, members)) = steps.last_mut() {
            let same_group = concurrent.contains(invocation.operation.as_str())
                && members
                    .iter()
                    .all(|member| member.operation == invocation.operation)
                && invocation.first <= *last;
            if same_group {
                *first = (*first).min(invocation.first);
                *last = (*last).max(invocation.last);
                members.push(invocation);
                continue;
            }
        }
        steps.push((invocation.first, invocation.last, vec![invocation]));
    }
    if steps.len() > MAX_INSPECT_STEPS {
        bail!(
            "capture contains {} invocations; the inspection step limit is {MAX_INSPECT_STEPS}",
            steps.len()
        );
    }
    Ok(steps
        .into_iter()
        .map(|(_, last, members)| {
            let grouped = members.len() > 1;
            let label = if grouped {
                format!(
                    "{} x{} (concurrent group)",
                    members[0].operation,
                    members.len()
                )
            } else {
                members[0].operation.clone()
            };
            PlannedStep {
                label,
                grouped,
                cut: last + 1,
                members: members
                    .into_iter()
                    .map(|invocation| PlannedMember {
                        operation: invocation.operation,
                        recorded: invocation.events,
                        live: None,
                    })
                    .collect(),
            }
        })
        .collect())
}

/// Setup steps then the failing request, each as one live member; consecutive
/// steps of an operation named by a concurrent-update proof group together.
pub(super) fn plan_finding_steps(artifact: &BackendFindingArtifact) -> Result<Vec<PlannedStep>> {
    let total = artifact.setup.len() + 1;
    if total > MAX_INSPECT_STEPS {
        bail!("finding replays {total} requests; the inspection step limit is {MAX_INSPECT_STEPS}");
    }
    let concurrent = concurrent_operations(&artifact.failing.policy.proofs);
    let mut steps: Vec<PlannedStep> = Vec::new();
    let replay_steps = artifact
        .setup
        .iter()
        .map(|step| (step, true))
        .chain(std::iter::once((&artifact.failing, false)));
    for (replay, setup) in replay_steps {
        let member = PlannedMember {
            operation: replay.contract.id.clone(),
            recorded: Vec::new(),
            live: Some(LiveMember {
                endpoint: replay_endpoint(replay),
                request: replay.request.clone(),
                setup,
            }),
        };
        let groupable = concurrent.contains(replay.contract.id.as_str())
            && steps.last().is_some_and(|step| {
                step.members.iter().all(|existing| {
                    existing.operation == replay.contract.id
                        && existing
                            .live
                            .as_ref()
                            .is_some_and(|live| live.setup == setup)
                })
            });
        if groupable {
            let step = steps.last_mut().expect("checked above");
            step.members.push(member);
            step.grouped = true;
            step.label = format!(
                "{} x{} (concurrent group)",
                replay.contract.id,
                step.members.len()
            );
            continue;
        }
        steps.push(PlannedStep {
            label: if setup {
                format!("{} (setup)", replay.contract.id)
            } else {
                format!("{} (failing request)", replay.contract.id)
            },
            grouped: false,
            cut: 0,
            members: vec![member],
        });
    }
    Ok(steps)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::backend::ConcurrencyPolicy;
    use crate::domain::backend::ResourceConsistency;
    use serde_json::json;

    fn wire_events(spans: &[(&str, &str, u16)]) -> Vec<BackendEvent> {
        let mut events = Vec::new();
        let mut sequence = 0u64;
        for (span, operation, status) in spans {
            sequence += 1;
            events.push(json!({
                "traceId": "cap", "spanId": span, "operation": operation,
                "sequence": sequence, "kind": "start", "input": {"body": {"item": "widget"}},
            }));
            sequence += 1;
            events.push(json!({
                "traceId": "cap", "spanId": span, "operation": operation,
                "sequence": sequence, "kind": "effect", "effect": "read",
                "resource": "inventory", "key": "widget",
            }));
            sequence += 1;
            events.push(json!({
                "traceId": "cap", "spanId": span, "operation": operation,
                "sequence": sequence, "kind": "return",
                "output": {"ok": *status < 500}, "status": status,
                "success": *status < 400, "effectsComplete": true,
            }));
        }
        serde_json::from_value(Value::Array(events)).expect("wire events")
    }

    #[test]
    fn capture_steps_follow_invocation_return_order_with_monotonic_cuts() {
        let events = wire_events(&[("a:op", "createOrder", 201), ("b:op", "createOrder", 500)]);
        let steps = plan_capture_steps(&events, &[]).expect("planned");
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].cut, 3);
        assert_eq!(steps[1].cut, 6);
        assert!(!steps[0].grouped);
        assert_eq!(steps[0].members[0].recorded.len(), 3);
    }

    #[test]
    fn overlapping_proof_operations_advance_as_one_grouped_step() {
        // Interleaved spans: a starts, b starts, a returns, b returns.
        let events: Vec<BackendEvent> = serde_json::from_value(json!([
            {"traceId": "t", "spanId": "a", "operation": "updateDoc", "sequence": 1,
             "kind": "start", "input": {}},
            {"traceId": "t", "spanId": "b", "operation": "updateDoc", "sequence": 2,
             "kind": "start", "input": {}},
            {"traceId": "t", "spanId": "a", "operation": "updateDoc", "sequence": 3,
             "kind": "return", "output": {}, "status": 200, "success": true},
            {"traceId": "t", "spanId": "b", "operation": "updateDoc", "sequence": 4,
             "kind": "return", "output": {}, "status": 200, "success": true},
        ]))
        .expect("events");
        let proofs = vec![BackendProofContract::ConcurrentUpdate {
            operation: "updateDoc".into(),
            identity_input_path: "$.id".into(),
            snapshot_input_path: "$.id".into(),
            consistency: ResourceConsistency::Strong,
            policy: ConcurrencyPolicy::OptimisticVersion {
                resource: "docs".into(),
                version_input_path: "$.version".into(),
                conflict_statuses: vec![409],
            },
        }];
        let steps = plan_capture_steps(&events, &proofs).expect("planned");
        assert_eq!(steps.len(), 1, "one grouped multi-actor step");
        assert!(steps[0].grouped);
        assert_eq!(steps[0].members.len(), 2);
        assert_eq!(steps[0].label, "updateDoc x2 (concurrent group)");
        // Without the proof the same trace is two separate steps.
        assert_eq!(plan_capture_steps(&events, &[]).expect("planned").len(), 2);
    }

    #[test]
    fn expected_violation_fires_at_the_correct_prefix_step() {
        let events = wire_events(&[("a:op", "createOrder", 201), ("b:op", "createOrder", 500)]);
        let steps = plan_capture_steps(&events, &[]).expect("planned");
        let config = BackendConfig {
            enabled: true,
            operations: vec![capture_replay::capture_contract("createOrder".into())],
            ..BackendConfig::default()
        };
        let expected = Expected::CaptureTarget {
            operation: "createOrder".into(),
            oracle: "backend-server-error".into(),
        };
        let mut fired_at = None;
        for (index, step) in steps.iter().enumerate() {
            let prefix = &events[..step.cut];
            if backend::evaluate(&config, prefix)
                .iter()
                .any(|violation| expected.matches(violation))
            {
                fired_at = Some(index + 1);
                break;
            }
        }
        assert_eq!(fired_at, Some(2), "the 500 invocation is step 2");
    }

    #[test]
    fn resent_captures_drop_transport_and_correlation_headers() {
        let input = json!({
            "body": {"item": "widget"},
            "headers": {
                "host": "prod.example.com",
                "content-length": "42",
                "x-reproit-trace": "prod-trace",
                "authorization": "Bearer token",
            },
        });
        let sanitized = sanitize_capture_input(&input);
        let headers = sanitized["headers"].as_object().expect("headers kept");
        assert_eq!(
            headers.keys().collect::<Vec<_>>(),
            vec!["authorization"],
            "only end-to-end headers survive"
        );
        assert_eq!(sanitized["body"], input["body"]);
    }
}
