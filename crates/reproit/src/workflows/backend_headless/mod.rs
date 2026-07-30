//! Schema-driven backend scan, fuzz, replay, and artifact orchestration.

use crate::domain::backend::{
    self, BackendAuth, BackendConfig, BackendEvent, BackendEventKind, BackendInvariant,
    BackendLogin, BackendViolation, FleetInvariant, OperationContract, ValueDomain,
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
use round_trip::{create_identity, probe_round_trips, record_create, CreateRecord};
mod history;
use history::{classify_and_record, gate_outcome};
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
        &[target.to_path_buf()],
        command,
        seed,
        runs,
        RunPolicy {
            policy: BackendPolicy::default(),
            operation_overrides: Vec::new(),
            auth: None,
            reset: Default::default(),
        },
        None,
    )
    .await
}

pub async fn run_configured_target(
    ctx: &Ctx,
    targets: &[PathBuf],
    command: &str,
    seed: u64,
    runs: u32,
    config: BackendConfig,
    root: Option<PathBuf>,
) -> Result<ExitCode> {
    let operations = config.operations;
    let auth = config.auth;
    let reset = config.reset;
    run_target_with_policy(
        ctx,
        targets,
        command,
        seed,
        runs,
        RunPolicy {
            policy: BackendPolicy {
                invariants: config.invariants,
                resources: config.resources,
                proofs: config.proofs,
                fleet: config.fleet,
            },
            operation_overrides: operations,
            auth: auth.as_ref(),
            reset,
        },
        root,
    )
    .await
}

async fn run_target_with_policy(
    ctx: &Ctx,
    targets: &[PathBuf],
    command: &str,
    seed: u64,
    runs: u32,
    declared: RunPolicy<'_>,
    root: Option<PathBuf>,
) -> Result<ExitCode> {
    let RunPolicy {
        policy,
        operation_overrides,
        auth,
        reset,
    } = declared;
    // The project root, not the process cwd: a repo-level gate runs several
    // services from one working directory, and each must own its own .reproit/
    // store or their finding histories collide.
    let root = match root {
        Some(root) => root,
        None => std::env::current_dir()?,
    };
    // A backend contract may be split across several schema files describing ONE
    // service. Aggregate every declared schema's operations (not just the first,
    // which silently dropped the rest); the first document supplies the base URL
    // fallback (service_base_url prefers REPROIT_BACKEND_URL regardless).
    let ServiceSchemas {
        mut endpoints,
        sha256: schema_sha256,
        violations: schema_violations,
        primary: primary_document,
        duplicates: duplicate_operations,
    } = aggregate_service_endpoints(targets)?;
    let primary = &targets[0];
    // Report schemas relative to the working directory when possible: the
    // resolved paths are absolute (joined from the canonicalized project root),
    // and a full path per schema makes the report labels noisy.
    let schema_labels: Vec<String> = targets
        .iter()
        .map(|path| {
            path.strip_prefix(&root)
                .unwrap_or(path.as_path())
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    for endpoint in &mut endpoints {
        if let Some(declared) = operation_overrides
            .iter()
            .find(|declared| declared.id == endpoint.contract.id)
        {
            apply_operation_override(&mut endpoint.contract, declared);
        }
        endpoint.policy = policy.clone();
    }
    if endpoints.is_empty() {
        bail!("the backend schema(s) contain no executable operations");
    }
    if endpoints.len() > MAX_ENDPOINTS {
        bail!(
            "backend schema has {} executable operations; safety limit is {MAX_ENDPOINTS}",
            endpoints.len()
        );
    }
    let base_url = match service_base_url(&primary_document) {
        Ok(base_url) => base_url,
        Err(error) if !schema_violations.is_empty() => {
            let findings =
                persist_schema_findings(&root, primary, &schema_sha256, schema_violations)?;
            let report = json!({
                "command": format!("backend {command}"),
                "complete": true,
                "schema": schema_labels.join(", "),
                "schemas": schema_labels,
                "schemaSha256": schema_sha256,
                "duplicateOperations": duplicate_operations.clone(),
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
    // Authenticated scan/fuzz: log in every configured identity first and install
    // the rotating pool, so the run reaches operations behind the auth boundary
    // and a per-user rate limit throttles one identity, not the whole run. Fails
    // closed (a bad login aborts the run).
    if let Some(auth) = auth {
        let pool = build_identity_pool(&client, &base_url, auth).await?;
        install_identity_pool(pool);
        let count = identity_count();
        ctx.say(format!(
            "Authenticated {count} identit{} (rotating per request)",
            if count == 1 { "y" } else { "ies" }
        ));
    }
    // Return the service to a known state before the sweep, so findings are
    // reproducible from a declared starting point rather than from whatever the
    // last run left behind. Fails closed on a required step.
    reset::run_reset(ctx, &reset, &root).await?;
    let attempts = if fuzzing { runs.max(1) } else { 1 };
    if attempts > MAX_ATTEMPTS_PER_OPERATION {
        bail!(
            "requested {attempts} attempts per operation; safety limit is \
             {MAX_ATTEMPTS_PER_OPERATION}"
        );
    }
    let probe_attempts = if fuzzing {
        // Wrong-typed input probes plus their one-shot confirmations.
        endpoints
            .len()
            .checked_mul(MAX_INVALID_PROBES_PER_OPERATION * 2)
            .context("backend probe budget overflow")?
    } else {
        0
    };
    let total_attempts = endpoints
        .len()
        .checked_mul(attempts as usize)
        .and_then(|total| total.checked_add(probe_attempts))
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
    // Operations actually re-exercised this run: the finding-lifecycle uses this
    // so a previously-active finding is only called `fixed` when its operation
    // was genuinely retried (scan hits GETs only, so it must not "fix" a mutation).
    let mut exercised_ops = BTreeSet::new();
    // Operations that returned only 429s this run: the server refused to process
    // them, so the oracle learned nothing. These are INCONCLUSIVE, not clean, and
    // must never render as a pass (a gate that goes blind under rate limiting and
    // then passes a still-broken PR is worse than no gate). `evaluated_ops` clears
    // the flag as soon as any non-429 response for that operation is seen.
    let mut rate_limited_ops = BTreeSet::new();
    let mut evaluated_ops = BTreeSet::new();
    let mut coverage = Coverage::new(&endpoints);

    let mut ordered = endpoints.clone();
    if fuzzing {
        ordered.sort_by(|left, right| {
            operation_rank(&left.method)
                .cmp(&operation_rank(&right.method))
                .then_with(|| left.contract.id.cmp(&right.contract.id))
        });
    }
    for offset in 0..attempts {
        // Each fuzz round starts clean too: without this, round N inherits the
        // resources round N-1 created and a shrink can chase state that no
        // longer exists.
        if offset > 0 {
            reset::run_reset(ctx, &reset, &root).await?;
        }
        let mut values = ValueBank::default();
        let mut setup = Vec::<ReplayStep>::new();
        for endpoint in &ordered {
            if !fuzzing && !endpoint.contract.read_only {
                if offset == 0 {
                    let reason = "scan executes read-only GET operations only";
                    coverage.not_sent(&endpoint.contract.id, reason);
                    skipped.push(json!({
                        "operation": endpoint.contract.id,
                        "reason": reason,
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
                    coverage.not_sent(&endpoint.contract.id, &error.to_string());
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
                    coverage.transport_error(&endpoint.contract.id);
                    execution_errors.push(json!({
                        "operation": endpoint.contract.id,
                        "error": error.to_string(),
                    }));
                    continue;
                }
            };
            let accepted = (200..400).contains(&result.status);
            coverage.record(&endpoint.contract.id, result.status, &result.output);
            exercised += 1;
            exercised_ops.insert(endpoint.contract.id.clone());
            if result.status == 429 {
                rate_limited_ops.insert(endpoint.contract.id.clone());
            } else {
                evaluated_ops.insert(endpoint.contract.id.clone());
            }
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
                } else if let Some(outcome) =
                    chaining::confirm_dependent(&client, endpoint, &request, &creates, &violation)
                        .await
                {
                    match outcome {
                        Ok(chained) => findings.push((
                            endpoint.clone(),
                            chained.request,
                            chained.setup,
                            finding,
                        )),
                        Err(reason) => candidates.push(json!({
                            "operation": endpoint.contract.id,
                            "reason": violation.reason,
                            "confirmation": reason,
                        })),
                    }
                } else if reset::reset_capability_available() {
                    match replay_sequence(
                        &client,
                        &setup,
                        endpoint,
                        &request,
                        &violation.fingerprint,
                        None,
                        None,
                    )
                    .await
                    {
                        Ok(ReplayVerdict::Reproduced) => findings.push((
                            endpoint.clone(),
                            request.clone(),
                            setup.clone(),
                            finding,
                        )),
                        Ok(_) => candidates.push(json!({
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
    if fuzzing {
        // Wrong-typed input probes: crash-instead-of-reject and
        // accept-instead-of-reject both surface here.
        let probes = probe_invalid_inputs(&client, &ordered, &base_url, seed).await;
        findings.extend(probes.findings);
        candidates.extend(probes.candidates);
        skipped.extend(probes.skipped);
        execution_errors.extend(probes.execution_errors);
        exercised += probes.exercised;
        rejected += probes.rejected;
    }

    let findings = shrink_findings(&client, &base_url, findings).await?;
    let mut public_findings =
        persist_schema_findings(&root, primary, &schema_sha256, schema_violations)?;
    public_findings.extend(persist_findings(
        &root,
        primary,
        &schema_sha256,
        seed,
        findings,
        &reset,
    )?);
    public_findings.sort_by(|left, right| {
        left.get("id")
            .and_then(Value::as_str)
            .cmp(&right.get("id").and_then(Value::as_str))
    });
    // Operations the server refused to evaluate (only ever 429 this run). They are
    // inconclusive, so a run that contains any is NOT complete: it cannot certify
    // "clean", record a baseline, or classify a finding as fixed. Fail closed.
    let inconclusive_ops: Vec<String> = rate_limited_ops
        .difference(&evaluated_ops)
        .cloned()
        .collect();
    let inconclusive = inconclusive_ops.len();
    let complete = execution_errors.is_empty() && exercised > 0 && inconclusive_ops.is_empty();
    // Finding lifecycle: classify this run's findings against the per-project
    // history (new / persisting / regressed / fixed) so a CI gate can block on
    // new-or-regressed. Only a complete run records history, and `fixed` is
    // guarded on the operation being re-exercised, so a partial run never
    // manufactures a fix.
    // A normal run advances the baseline; a CI gate classifies read-only against
    // it and records only when explicitly re-baselining.
    let gate = std::env::var_os("REPROIT_GATE").is_some();
    let record = !gate || std::env::var_os("REPROIT_GATE_BASELINE").is_some();
    let lifecycle = if complete {
        classify_and_record(&root, &public_findings, &exercised_ops, record)?
    } else {
        Value::Null
    };
    if let Some(counts) = lifecycle.get("counts") {
        ctx.say(format!(
            "lifecycle: {} new, {} regressed, {} persisting, {} fixed",
            counts["new"], counts["regressed"], counts["persisting"], counts["fixed"]
        ));
    }
    let report = json!({
        "command": format!("backend {command}"),
        "complete": complete,
        "schema": schema_labels.join(", "),
        "schemas": schema_labels,
        "schemaSha256": schema_sha256,
        "duplicateOperations": duplicate_operations,
        "baseUrl": base_url,
        "operations": endpoints.len(),
        "operationsEvaluated": coverage.evaluated_count(),
        "coverage": coverage.report(),
        "attemptsPerOperation": attempts,
        "exercised": exercised,
        "rejected": rejected,
        "inconclusive": inconclusive_ops,
        "skipped": skipped,
        "executionErrors": execution_errors,
        "candidates": candidates,
        "lifecycle": lifecycle,
        "findings": public_findings,
    });
    persist_run_report(&root, command, &report)?;
    emit_report(ctx, command, &report);
    if inconclusive > 0 {
        ctx.say(format!(
            "{inconclusive} operation(s) inconclusive (rate-limited); not treated as clean"
        ));
    }
    // CI gate mode (REPROIT_GATE): block on NEW or REGRESSED findings, never on
    // persisting/accepted ones, so a gate on every PR fails on a freshly introduced
    // reproducible bug yet not forever on a known one (zero-false-positive findings).
    // It ALSO fails closed on inconclusive (rate-limited) operations: "could not
    // evaluate" must never render as "clean", or a retried CI job passes a still
    // broken PR.
    if std::env::var_os("REPROIT_GATE").is_some() {
        return Ok(gate_outcome(ctx, &lifecycle, complete, inconclusive, &root));
    }
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
struct PassRun {
    findings: Vec<FindingCase>,
    candidates: Vec<Value>,
    skipped: Vec<Value>,
    execution_errors: Vec<Value>,
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
) -> Result<PassRun> {
    let mut run = PassRun::default();
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
        if !reset::reset_capability_available() {
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
            maybe_reset_target(client, base_url, None).await?;
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
                json_path(&create_result.output, &resource.create.output_identity_path)
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
                                .and_then(|path| json_path(&input, path))
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
                if replay_sequence(
                    client,
                    &setup,
                    read,
                    &read_request,
                    &violation.fingerprint,
                    None,
                    None,
                )
                .await?
                    == ReplayVerdict::Reproduced
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

mod schema;
pub(crate) use schema::schema_surface;
use schema::*;
mod generation;
#[cfg(test)]
use generation::invalid_probes;
use generation::{probe_invalid_inputs, sample_domain, MAX_INVALID_PROBES_PER_OPERATION};
mod request;
use request::build_request;
mod transport;
#[cfg(test)]
use transport::evaluate_invocation;
use transport::{build_identity_pool, identity_count, install_identity_pool, invoke};
mod replay;
#[cfg(test)]
use replay::apply_request_bindings;
use replay::{append_sequence_events, has_fingerprint, replay_sequence, ReplayVerdict};
mod shrink;
use shrink::shrink_findings;
mod artifacts;
use artifacts::{emit_report, persist_findings, persist_run_report, persist_schema_findings};
pub(super) mod coverage;
use coverage::Coverage;
mod accept;
mod chaining;
pub use accept::run as backend_accept;
mod reset;
mod retraction;
use retraction::{ArtifactVerdict, ContractStatus, CurrentContracts};
mod replay_command;
pub use replay_command::{replay_kept_guards, try_replay};
mod verify;
pub use verify::run as backend_verify;
mod capture_replay;
pub use capture_replay::{check_capture, is_capture_file, replay_capture};
mod inspect;
mod inspect_plan;
mod inspect_report;
pub use inspect::try_inspect;
use replay_command::{
    escape_pointer, find_artifact, maybe_reset_target, replay_endpoint, value_as_text,
};
mod encoding;
use crate::domain::json_path::{is_scalar_identity, json_path, set_json_path};
use encoding::{hex_hash, percent_encode};

#[cfg(test)]
mod tests;
