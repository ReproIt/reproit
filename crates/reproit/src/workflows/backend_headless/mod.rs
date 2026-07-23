//! Schema-driven backend scan, fuzz, replay, and artifact orchestration.

use crate::domain::backend::{
    self, BackendConfig, BackendEvent, BackendEventKind, BackendInvariant, BackendViolation,
    FleetInvariant, OperationContract, ValueDomain,
};
use crate::domain::repro;
use crate::interface::cli::context::{Ctx, Exit};
use crate::runtime::project_layout as layout;
use anyhow::{bail, Context, Result};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{ExitCode, Stdio};
use std::time::Duration;
use tokio::io::AsyncWriteExt;

const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const MAX_GRAPHQL_SELECTION_DEPTH: usize = 5;
const MAX_GENERATED_VALUE_DEPTH: usize = 12;
const MAX_GENERATED_STRING_CHARS: usize = 4 * 1024;
const MAX_GENERATED_ARRAY_ITEMS: usize = 256;
const MAX_REDUCTIONS_PER_PASS: usize = 256;
const MAX_ENDPOINTS: usize = 2_048;
const MAX_ATTEMPTS_PER_OPERATION: u32 = 1_024;
const MAX_TOTAL_ATTEMPTS: usize = 100_000;

mod types;
use types::*;
mod binding;
use binding::ValueBank;
mod round_trip;
use round_trip::{probe_round_trips, record_create, CreateRecord};
fn operation_rank(method: &str) -> u8 {
    match method {
        "POST" => 0,
        "PUT" | "PATCH" => 1,
        "GET" | "HEAD" | "OPTIONS" => 2,
        "DELETE" => 3,
        _ => 4,
    }
}

pub fn looks_like_schema(path: &Path) -> bool {
    load_document(path).is_ok_and(|document| !backend::import_service_schema(&document).is_empty())
}

pub async fn run_target(
    ctx: &Ctx,
    target: &Path,
    command: &str,
    seed: u64,
    runs: u32,
) -> Result<ExitCode> {
    run_target_with_policy(
        ctx,
        target,
        command,
        seed,
        runs,
        BackendPolicy::default(),
        Vec::new(),
    )
    .await
}

pub async fn run_configured_target(
    ctx: &Ctx,
    target: &Path,
    command: &str,
    seed: u64,
    runs: u32,
    config: BackendConfig,
) -> Result<ExitCode> {
    let operations = config.operations;
    run_target_with_policy(
        ctx,
        target,
        command,
        seed,
        runs,
        BackendPolicy {
            invariants: config.invariants,
            resources: config.resources,
            proofs: config.proofs,
            fleet: config.fleet,
        },
        operations,
    )
    .await
}

async fn run_target_with_policy(
    ctx: &Ctx,
    target: &Path,
    command: &str,
    seed: u64,
    runs: u32,
    policy: BackendPolicy,
    operation_overrides: Vec<OperationContract>,
) -> Result<ExitCode> {
    let root = std::env::current_dir()?;
    let document = load_document(target)?;
    let openapi = document.get("openapi").is_some() || document.get("swagger").is_some();
    let graphql =
        document.pointer("/data/__schema").is_some() || document.get("__schema").is_some();
    let grpc = document.get("file").is_some() || document.get("files").is_some();
    if !openapi && !graphql && !grpc {
        bail!("backend schema is not OpenAPI, GraphQL, or a protobuf descriptor");
    }
    let schema_bytes = std::fs::read(target)?;
    let schema_sha256 = hex_hash(&schema_bytes);
    let schema_violations = if openapi {
        backend::validate_openapi_parameter_uniqueness(&document)
    } else {
        Vec::new()
    };
    let mut endpoints = if openapi {
        openapi_endpoints(&document)
    } else if graphql {
        graphql_endpoints(&document)
    } else {
        grpc_endpoints(&document)
    };
    for endpoint in &mut endpoints {
        if let Some(declared) = operation_overrides
            .iter()
            .find(|declared| declared.id == endpoint.contract.id)
        {
            apply_operation_override(&mut endpoint.contract, declared);
        }
        endpoint.policy = policy.clone();
        if grpc && target.extension().and_then(|value| value.to_str()) == Some("proto") {
            endpoint.schema_source = Some(target.canonicalize()?);
        }
    }
    if endpoints.is_empty() {
        bail!("the OpenAPI document contains no executable operations");
    }
    if endpoints.len() > MAX_ENDPOINTS {
        bail!(
            "backend schema has {} executable operations; safety limit is {MAX_ENDPOINTS}",
            endpoints.len()
        );
    }
    let base_url = match service_base_url(&document) {
        Ok(base_url) => base_url,
        Err(error) if !schema_violations.is_empty() => {
            let findings =
                persist_schema_findings(&root, target, &schema_sha256, schema_violations)?;
            let report = json!({
                "command": format!("backend {command}"),
                "complete": true,
                "schema": target.to_string_lossy(),
                "schemaSha256": schema_sha256,
                "baseUrl": Value::Null,
                "operations": endpoints.len(),
                "attemptsPerOperation": 0,
                "exercised": 0,
                "rejected": 0,
                "skipped": [{
                    "scope": "runtime",
                    "reason": error.to_string(),
                }],
                "executionErrors": [],
                "candidates": [],
                "findings": findings,
            });
            persist_run_report(&root, command, &report)?;
            emit_report(ctx, command, &report);
            return Ok(Exit::Regression.code());
        }
        Err(error) => return Err(error),
    };
    let fuzzing = command == "fuzz";
    if fuzzing
        && endpoints
            .iter()
            .any(|endpoint| !endpoint.contract.read_only)
    {
        let loopback = base_url
            .parse::<reqwest::Url>()
            .ok()
            .and_then(|url| url.host_str().map(str::to_string))
            .is_some_and(|host| matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1"));
        if !loopback && !ctx.confirmed() {
            bail!(
                "backend fuzz may call mutating operations on {base_url}; use a disposable target \
                 and pass --yes to confirm"
            );
        }
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::limited(3))
        .build()?;
    let attempts = if fuzzing { runs.max(1) } else { 1 };
    if attempts > MAX_ATTEMPTS_PER_OPERATION {
        bail!(
            "requested {attempts} attempts per operation; safety limit is \
             {MAX_ATTEMPTS_PER_OPERATION}"
        );
    }
    let total_attempts = endpoints
        .len()
        .checked_mul(attempts as usize)
        .context("backend attempt budget overflow")?;
    if total_attempts > MAX_TOTAL_ATTEMPTS {
        bail!(
            "backend run would execute {total_attempts} attempts; safety limit is \
             {MAX_TOTAL_ATTEMPTS}"
        );
    }
    let mut findings = Vec::new();
    let mut creates: Vec<CreateRecord> = Vec::new();
    let mut candidates = Vec::new();
    let mut exercised = 0usize;
    let mut rejected = 0usize;
    let mut skipped = Vec::new();
    let mut execution_errors = Vec::new();

    let mut ordered = endpoints.clone();
    if fuzzing {
        ordered.sort_by(|left, right| {
            operation_rank(&left.method)
                .cmp(&operation_rank(&right.method))
                .then_with(|| left.contract.id.cmp(&right.contract.id))
        });
    }
    for offset in 0..attempts {
        let mut values = ValueBank::default();
        let mut setup = Vec::<ReplayStep>::new();
        for endpoint in &ordered {
            if !fuzzing && !endpoint.contract.read_only {
                if offset == 0 {
                    skipped.push(json!({
                        "operation": endpoint.contract.id,
                        "reason": "scan executes read-only GET operations only",
                    }));
                }
                continue;
            }
            let case_seed = seed.saturating_add(u64::from(offset));
            let mut input = endpoint
                .contract
                .input
                .as_ref()
                .map(|domain| sample_domain(domain, case_seed, fuzzing, 0))
                .unwrap_or(Value::Null);
            if let Some(domain) = endpoint.contract.input.as_ref() {
                values.bind(domain, &mut input, None);
            }
            let request = match build_request(endpoint, &base_url, input) {
                Ok(request) => request,
                Err(error) => {
                    skipped.push(json!({
                        "operation": endpoint.contract.id,
                        "reason": error.to_string(),
                    }));
                    continue;
                }
            };
            let result = match invoke(&client, endpoint, request.clone()).await {
                Ok(result) => result,
                Err(error) => {
                    execution_errors.push(json!({
                        "operation": endpoint.contract.id,
                        "error": error.to_string(),
                    }));
                    continue;
                }
            };
            let accepted = (200..400).contains(&result.status);
            exercised += 1;
            if !accepted {
                rejected += 1;
            }
            let clean = accepted && result.violations.is_empty();
            if clean {
                values.harvest(&result.output);
                if endpoint.method == "POST" {
                    // Round-trip probes only ever touch resources this run
                    // created itself; remember clean creates as candidates.
                    record_create(
                        &mut creates,
                        CreateRecord {
                            endpoint: endpoint.clone(),
                            request: request.clone(),
                            output: result.output.clone(),
                        },
                    );
                }
            }
            for violation in result.violations {
                let finding = backend::finding(&violation);
                let reset_available = std::env::var_os("REPROIT_BACKEND_RESET_URL").is_some();
                if endpoint.contract.idempotent && setup.is_empty() {
                    match invoke(&client, endpoint, request.clone()).await {
                        Ok(confirmation)
                            if has_fingerprint(&confirmation, &violation.fingerprint) =>
                        {
                            findings.push((
                                endpoint.clone(),
                                request.clone(),
                                setup.clone(),
                                finding,
                            ));
                        }
                        Ok(_) => candidates.push(json!({
                            "operation": endpoint.contract.id,
                            "reason": violation.reason,
                            "confirmation": "did not reproduce exactly",
                        })),
                        Err(error) => candidates.push(json!({
                            "operation": endpoint.contract.id,
                            "reason": violation.reason,
                            "confirmation": format!("confirmation failed: {error}"),
                        })),
                    }
                } else if reset_available {
                    match replay_sequence(
                        &client,
                        &setup,
                        endpoint,
                        &request,
                        &violation.fingerprint,
                    )
                    .await
                    {
                        Ok(true) => findings.push((
                            endpoint.clone(),
                            request.clone(),
                            setup.clone(),
                            finding,
                        )),
                        Ok(false) => candidates.push(json!({
                            "operation": endpoint.contract.id,
                            "reason": violation.reason,
                            "confirmation": "clean-state replay did not reproduce exactly",
                        })),
                        Err(error) => candidates.push(json!({
                            "operation": endpoint.contract.id,
                            "reason": violation.reason,
                            "confirmation": format!("clean-state replay failed: {error}"),
                        })),
                    }
                } else {
                    candidates.push(json!({
                        "operation": endpoint.contract.id,
                        "reason": violation.reason,
                        "confirmation": concat!(
                            "stateful or non-idempotent confirmation requires ",
                            "REPROIT_BACKEND_RESET_URL"
                        ),
                    }));
                }
            }
            if !accepted {
                continue;
            }
            if clean && !endpoint.contract.read_only {
                setup.push(ReplayStep {
                    contract: endpoint.contract.clone(),
                    request,
                    policy: endpoint.policy.clone(),
                });
            }
        }
    }

    if fuzzing && !policy.resources.is_empty() {
        let lifecycle =
            exercise_resource_lifecycles(&client, &ordered, &base_url, seed, &policy).await?;
        findings.extend(lifecycle.findings);
        candidates.extend(lifecycle.candidates);
        skipped.extend(lifecycle.skipped);
        exercised += lifecycle.exercised;
        rejected += lifecycle.rejected;
    }
    if fuzzing && !creates.is_empty() {
        // DATA-LOSS round-trip probes: schema-inferred (GET, PATCH) pairs on
        // resources this run created. See round_trip.rs.
        let round = probe_round_trips(&client, &ordered, &base_url, seed, &creates).await?;
        findings.extend(round.findings);
        candidates.extend(round.candidates);
        skipped.extend(round.skipped);
        exercised += round.exercised;
        rejected += round.rejected;
    }

    let findings = shrink_findings(&client, &base_url, findings).await?;
    let mut public_findings =
        persist_schema_findings(&root, target, &schema_sha256, schema_violations)?;
    public_findings.extend(persist_findings(
        &root,
        target,
        &schema_sha256,
        seed,
        findings,
    )?);
    public_findings.sort_by(|left, right| {
        left.get("id")
            .and_then(Value::as_str)
            .cmp(&right.get("id").and_then(Value::as_str))
    });
    let complete = execution_errors.is_empty() && exercised > 0;
    let report = json!({
        "command": format!("backend {command}"),
        "complete": complete,
        "schema": target.to_string_lossy(),
        "schemaSha256": schema_sha256,
        "baseUrl": base_url,
        "operations": endpoints.len(),
        "attemptsPerOperation": attempts,
        "exercised": exercised,
        "rejected": rejected,
        "skipped": skipped,
        "executionErrors": execution_errors,
        "candidates": candidates,
        "findings": public_findings,
    });
    persist_run_report(&root, command, &report)?;
    emit_report(ctx, command, &report);
    let has_findings = report["findings"]
        .as_array()
        .is_some_and(|values| !values.is_empty());
    Ok(if complete && !has_findings {
        ExitCode::SUCCESS
    } else {
        Exit::Regression.code()
    })
}

#[derive(Default)]
struct LifecycleRun {
    findings: Vec<FindingCase>,
    candidates: Vec<Value>,
    skipped: Vec<Value>,
    exercised: usize,
    rejected: usize,
}

#[derive(Clone, Copy)]
enum LifecycleBranch<'a> {
    Read,
    Update(&'a backend::ResourceMutationContract),
    Delete(&'a backend::ResourceMutationContract),
}

async fn exercise_resource_lifecycles(
    client: &reqwest::Client,
    endpoints: &[Endpoint],
    base_url: &str,
    seed: u64,
    policy: &BackendPolicy,
) -> Result<LifecycleRun> {
    let mut run = LifecycleRun::default();
    for resource in &policy.resources {
        if resource.consistency != backend::ResourceConsistency::Strong {
            run.skipped.push(json!({
                "resource": resource.name,
                "reason": "lifecycle consistency is not explicitly strong; result is unknown",
            }));
            continue;
        }
        if resource.read.absent_statuses.is_empty() {
            run.skipped.push(json!({
                "resource": resource.name,
                "reason": "read absent statuses are not declared; result is unknown",
            }));
            continue;
        }
        if std::env::var_os("REPROIT_BACKEND_RESET_URL").is_none() {
            run.skipped.push(json!({
                "resource": resource.name,
                "reason": "lifecycle replay needs REPROIT_BACKEND_RESET_URL; result is unknown",
            }));
            continue;
        }
        let Some(create) = unique_endpoint(endpoints, &resource.create.operation) else {
            run.skipped.push(json!({
                "resource": resource.name,
                "reason": "create operation is missing or ambiguous; result is unknown",
            }));
            continue;
        };
        let Some(read) = unique_endpoint(endpoints, &resource.read.operation) else {
            run.skipped.push(json!({
                "resource": resource.name,
                "reason": "read operation is missing or ambiguous; result is unknown",
            }));
            continue;
        };
        let mut branches = vec![LifecycleBranch::Read];
        if let Some(update) = &resource.update {
            if unique_endpoint(endpoints, &update.operation).is_some() {
                branches.push(LifecycleBranch::Update(update));
            } else {
                run.skipped.push(json!({
                    "resource": resource.name,
                    "reason": concat!(
                        "update operation is missing or ambiguous; ",
                        "update lifecycle is unknown"
                    ),
                }));
            }
        }
        if let Some(delete) = &resource.delete {
            if unique_endpoint(endpoints, &delete.operation).is_some() {
                branches.push(LifecycleBranch::Delete(delete));
            } else {
                run.skipped.push(json!({
                    "resource": resource.name,
                    "reason": concat!(
                        "delete operation is missing or ambiguous; ",
                        "delete lifecycle is unknown"
                    ),
                }));
            }
        }

        for (branch_index, branch) in branches.into_iter().enumerate() {
            maybe_reset_target(client, base_url).await?;
            let create_input = create
                .contract
                .input
                .as_ref()
                .map(|domain| sample_domain(domain, seed + branch_index as u64, true, 0))
                .unwrap_or(Value::Null);
            let create_request = build_request(create, base_url, create_input)?;
            let create_result = invoke(client, create, create_request.clone()).await?;
            run.exercised += 1;
            if !(200..400).contains(&create_result.status) || !create_result.violations.is_empty() {
                run.rejected += 1;
                run.skipped.push(json!({
                    "resource": resource.name,
                    "reason": "create setup did not complete cleanly; result is unknown",
                }));
                continue;
            }
            let Some(identity) =
                json_path_value(&create_result.output, &resource.create.output_identity_path)
                    .filter(|value| is_scalar_identity(value))
                    .cloned()
            else {
                run.skipped.push(json!({
                    "resource": resource.name,
                    "reason": "create returned no unambiguous scalar identity; result is unknown",
                }));
                continue;
            };

            let mut setup = vec![ReplayStep {
                contract: create.contract.clone(),
                request: create_request,
                policy: policy.clone(),
            }];
            let mut sequence = Vec::new();
            append_sequence_events(&mut sequence, create_result.events, 0);

            let mut branch_ready = true;
            match branch {
                LifecycleBranch::Read => {}
                LifecycleBranch::Update(update) => {
                    let endpoint = unique_endpoint(endpoints, &update.operation)
                        .expect("validated lifecycle update endpoint");
                    let mut input = endpoint
                        .contract
                        .input
                        .as_ref()
                        .map(|domain| sample_domain(domain, seed + 31, true, 0))
                        .unwrap_or(Value::Null);
                    if !set_json_path(&mut input, &update.input_identity_path, identity.clone())
                        || !resource.fields.iter().any(|field| {
                            field
                                .update_input_path
                                .as_deref()
                                .and_then(|path| json_path_value(&input, path))
                                .is_some()
                        })
                    {
                        branch_ready = false;
                    } else {
                        let mut request = build_request(endpoint, base_url, input)?;
                        request.bindings.push(RequestBinding {
                            source_step: 0,
                            source_output_path: resource.create.output_identity_path.clone(),
                            input_path: update.input_identity_path.clone(),
                        });
                        let result = invoke(client, endpoint, request.clone()).await?;
                        run.exercised += 1;
                        if !(200..400).contains(&result.status) || !result.violations.is_empty() {
                            run.rejected += 1;
                            branch_ready = false;
                        } else {
                            let step = setup.len();
                            append_sequence_events(&mut sequence, result.events, step);
                            setup.push(ReplayStep {
                                contract: endpoint.contract.clone(),
                                request,
                                policy: policy.clone(),
                            });
                        }
                    }
                }
                LifecycleBranch::Delete(delete) => {
                    let endpoint = unique_endpoint(endpoints, &delete.operation)
                        .expect("validated lifecycle delete endpoint");
                    let mut input = endpoint
                        .contract
                        .input
                        .as_ref()
                        .map(|domain| sample_domain(domain, seed + 47, true, 0))
                        .unwrap_or(Value::Null);
                    if !set_json_path(&mut input, &delete.input_identity_path, identity.clone()) {
                        branch_ready = false;
                    } else {
                        let mut request = build_request(endpoint, base_url, input)?;
                        request.bindings.push(RequestBinding {
                            source_step: 0,
                            source_output_path: resource.create.output_identity_path.clone(),
                            input_path: delete.input_identity_path.clone(),
                        });
                        let result = invoke(client, endpoint, request.clone()).await?;
                        run.exercised += 1;
                        if !(200..400).contains(&result.status) || !result.violations.is_empty() {
                            run.rejected += 1;
                            branch_ready = false;
                        } else {
                            let step = setup.len();
                            append_sequence_events(&mut sequence, result.events, step);
                            setup.push(ReplayStep {
                                contract: endpoint.contract.clone(),
                                request,
                                policy: policy.clone(),
                            });
                        }
                    }
                }
            }
            if !branch_ready {
                run.skipped.push(json!({
                    "resource": resource.name,
                    "reason": concat!(
                        "lifecycle setup or identity binding was incomplete; ",
                        "result is unknown"
                    ),
                }));
                continue;
            }

            let mut read_input = read
                .contract
                .input
                .as_ref()
                .map(|domain| sample_domain(domain, seed + 63, true, 0))
                .unwrap_or(Value::Null);
            if !set_json_path(
                &mut read_input,
                &resource.read.input_identity_path,
                identity,
            ) {
                run.skipped.push(json!({
                    "resource": resource.name,
                    "reason": "read identity path could not be bound; result is unknown",
                }));
                continue;
            }
            let mut read_request = build_request(read, base_url, read_input)?;
            read_request.bindings.push(RequestBinding {
                source_step: 0,
                source_output_path: resource.create.output_identity_path.clone(),
                input_path: resource.read.input_identity_path.clone(),
            });
            let read_result = invoke(client, read, read_request.clone()).await?;
            run.exercised += 1;
            let failing_index = setup.len();
            append_sequence_events(&mut sequence, read_result.events, failing_index);
            let mut operations = setup
                .iter()
                .map(|step| step.contract.clone())
                .collect::<Vec<_>>();
            operations.push(read.contract.clone());
            let config = BackendConfig {
                enabled: true,
                operations,
                invariants: policy.invariants.clone(),
                resources: policy.resources.clone(),
                proofs: policy.proofs.clone(),
                fleet: policy.fleet.clone(),
                ..BackendConfig::default()
            };
            for violation in backend::evaluate(&config, &sequence)
                .into_iter()
                .filter(|violation| violation.oracle.starts_with("resource-"))
            {
                let finding = backend::finding(&violation);
                if replay_sequence(client, &setup, read, &read_request, &violation.fingerprint)
                    .await?
                {
                    run.findings
                        .push((read.clone(), read_request.clone(), setup.clone(), finding));
                } else {
                    run.candidates.push(json!({
                        "resource": resource.name,
                        "reason": violation.reason,
                        "confirmation": "clean-state lifecycle replay did not reproduce exactly",
                    }));
                }
            }
        }
    }
    Ok(run)
}

fn unique_endpoint<'a>(endpoints: &'a [Endpoint], operation: &str) -> Option<&'a Endpoint> {
    let mut matches = endpoints
        .iter()
        .filter(|endpoint| endpoint.contract.id == operation);
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

fn json_path_value<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    if path.is_empty() || path == "$" {
        return Some(value);
    }
    path.trim_start_matches('$')
        .trim_start_matches('.')
        .split('.')
        .filter(|part| !part.is_empty())
        .try_fold(value, |current, part| current.get(part))
}

fn set_json_path(value: &mut Value, path: &str, replacement: Value) -> bool {
    let parts = path
        .trim_start_matches('$')
        .trim_start_matches('.')
        .split('.')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    let Some((last, parents)) = parts.split_last() else {
        *value = replacement;
        return true;
    };
    let Some(parent) = parents
        .iter()
        .try_fold(value, |current, part| current.get_mut(*part))
    else {
        return false;
    };
    let Some(object) = parent.as_object_mut() else {
        return false;
    };
    if !object.contains_key(*last) {
        return false;
    }
    object.insert((*last).into(), replacement);
    true
}

fn is_scalar_identity(value: &Value) -> bool {
    matches!(value, Value::String(_) | Value::Number(_) | Value::Bool(_))
}

fn apply_operation_override(imported: &mut OperationContract, declared: &OperationContract) {
    if declared.input.is_some() {
        imported.input = declared.input.clone();
    }
    if declared.output.is_some() {
        imported.output = declared.output.clone();
    }
    if !declared.outputs_by_status.is_empty() {
        imported
            .outputs_by_status
            .extend(declared.outputs_by_status.clone());
    }
    if !declared.success_statuses.is_empty() {
        imported.success_statuses = declared.success_statuses.clone();
    }
    imported.read_only |= declared.read_only;
    imported.idempotent |= declared.idempotent;
    imported.tenant_isolated |= declared.tenant_isolated;
    if !declared.promised_effects.is_empty() {
        imported.promised_effects = declared.promised_effects.clone();
    }
}

mod schema;
use schema::*;
mod generation;
use generation::sample_domain;
mod request;
use request::build_request;
mod transport;
#[cfg(test)]
use transport::evaluate_invocation;
use transport::invoke;
mod replay;
#[cfg(test)]
use replay::apply_request_bindings;
use replay::{append_sequence_events, has_fingerprint, replay_sequence};
mod shrink;
use shrink::shrink_findings;
mod artifacts;
use artifacts::{emit_report, persist_findings, persist_run_report, persist_schema_findings};
mod replay_command;
pub use replay_command::try_replay;
use replay_command::{escape_pointer, maybe_reset_target, replay_endpoint, value_as_text};
fn percent_encode(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn hex_hash(value: &[u8]) -> String {
    crate::domain::hash::sha256_hex(value)
}

#[cfg(test)]
mod tests;
