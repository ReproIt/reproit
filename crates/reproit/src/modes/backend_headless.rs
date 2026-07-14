use crate::backend::{
    self, BackendConfig, BackendEvent, BackendEventKind, BackendInvariant, BackendViolation,
    FleetInvariant, OperationContract, ValueDomain,
};
use crate::{repro, Ctx, Exit};
use anyhow::{bail, Context, Result};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{ExitCode, Stdio};
use std::time::Duration;
use tokio::io::AsyncWriteExt;

const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone)]
struct Endpoint {
    contract: OperationContract,
    method: String,
    path: String,
    body_only: bool,
    content_type: Option<String>,
    response_field: Option<String>,
    policy: BackendPolicy,
    transport: Transport,
    schema_source: Option<PathBuf>,
    client_streaming: bool,
    server_streaming: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Transport {
    #[default]
    Http,
    Grpc,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BackendPolicy {
    #[serde(default)]
    invariants: Vec<BackendInvariant>,
    #[serde(default)]
    fleet: FleetInvariant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RequestArtifact {
    operation: String,
    method: String,
    url: String,
    input: Value,
    #[serde(default)]
    headers: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    body: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    content_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    schema_source: Option<PathBuf>,
    #[serde(default)]
    client_streaming: bool,
    #[serde(default)]
    server_streaming: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BackendFindingArtifact {
    format: String,
    version: u32,
    schema: String,
    schema_sha256: String,
    #[serde(default)]
    setup: Vec<ReplayStep>,
    failing: ReplayStep,
    finding: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReplayStep {
    contract: OperationContract,
    request: RequestArtifact,
    #[serde(default)]
    policy: BackendPolicy,
}

type FindingCase = (Endpoint, RequestArtifact, Vec<ReplayStep>, Value);

#[derive(Debug)]
struct InvocationResult {
    status: u16,
    output: Value,
    violations: Vec<BackendViolation>,
}

#[derive(Default)]
struct ValueBank {
    values: BTreeMap<String, Vec<Value>>,
}

impl ValueBank {
    fn harvest(&mut self, value: &Value) {
        self.harvest_named(value, None);
    }

    fn harvest_named(&mut self, value: &Value, name: Option<&str>) {
        match value {
            Value::Object(object) => {
                for (key, value) in object {
                    self.harvest_named(value, Some(key));
                }
            }
            Value::Array(values) => {
                for value in values {
                    self.harvest_named(value, name);
                }
            }
            Value::String(_) | Value::Number(_) | Value::Bool(_) => {
                let Some(name) = name else {
                    return;
                };
                let normalized = normalized_name(name);
                if !is_bindable_name(&normalized) {
                    return;
                }
                self.values
                    .entry(normalized.clone())
                    .or_default()
                    .push(value.clone());
                if normalized != "id" && normalized.ends_with("id") {
                    self.values
                        .entry("id".into())
                        .or_default()
                        .push(value.clone());
                }
            }
            Value::Null => {}
        }
    }

    fn bind(&self, domain: &ValueDomain, value: &mut Value, name: Option<&str>) {
        match (domain, value) {
            (ValueDomain::Object { properties, .. }, Value::Object(current)) => {
                for (property, property_domain) in properties {
                    if let Some(property_value) = current.get_mut(property) {
                        self.bind(property_domain, property_value, Some(property));
                    }
                }
            }
            (ValueDomain::Array { items, .. }, Value::Array(current)) => {
                for item in current {
                    self.bind(items, item, name);
                }
            }
            (ValueDomain::OneOf { variants }, current) => {
                if let Some(variant) = variants
                    .iter()
                    .find(|variant| variant.mismatch(current, "$candidate").is_none())
                {
                    self.bind(variant, current, name);
                }
            }
            (domain, current) => {
                let Some(name) = name else {
                    return;
                };
                let normalized = normalized_name(name);
                if !is_bindable_name(&normalized) {
                    return;
                }
                let candidates = self.values.get(&normalized).or_else(|| {
                    normalized
                        .ends_with("id")
                        .then(|| self.values.get("id"))
                        .flatten()
                });
                if let Some(candidate) = candidates
                    .into_iter()
                    .flatten()
                    .find(|candidate| domain.mismatch(candidate, "$candidate").is_none())
                {
                    *current = candidate.clone();
                }
            }
        }
    }
}

fn normalized_name(name: &str) -> String {
    name.chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn is_bindable_name(name: &str) -> bool {
    name == "id" || name.ends_with("id") || matches!(name, "slug" | "code")
}

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
    let base_url = service_base_url(&document)?;
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
                "backend fuzz may call mutating operations on {base_url}; use a disposable target and pass --yes to confirm"
            );
        }
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::limited(3))
        .build()?;
    let attempts = if fuzzing { runs.max(1) } else { 1 };
    let mut findings = Vec::new();
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
            if !(200..400).contains(&result.status) {
                rejected += 1;
                continue;
            }
            exercised += 1;
            let clean = result.violations.is_empty();
            if clean {
                values.harvest(&result.output);
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
                        "confirmation": "stateful or non-idempotent confirmation requires REPROIT_BACKEND_RESET_URL",
                    }));
                }
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

    let findings = shrink_findings(&client, &base_url, findings).await?;
    let mut public_findings = persist_findings(&root, target, &schema_sha256, seed, findings)?;
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

fn load_document(path: &Path) -> Result<Value> {
    backend::load_service_document(path)
}

fn service_base_url(document: &Value) -> Result<String> {
    if let Ok(override_url) = std::env::var("REPROIT_BACKEND_URL") {
        validate_base_url(&override_url)?;
        return Ok(override_url.trim_end_matches('/').to_string());
    }
    let server = document
        .get("servers")
        .and_then(Value::as_array)
        .and_then(|servers| servers.first())
        .and_then(|server| server.get("url").and_then(Value::as_str))
        .context(
            "the schema has no absolute server URL; set REPROIT_BACKEND_URL to the disposable service",
        )?;
    let mut resolved = server.to_string();
    if let Some(variables) = document
        .pointer("/servers/0/variables")
        .and_then(Value::as_object)
    {
        for (name, variable) in variables {
            if let Some(default) = variable.get("default").and_then(value_as_text) {
                resolved = resolved.replace(&format!("{{{name}}}"), &default);
            }
        }
    }
    validate_base_url(&resolved)?;
    Ok(resolved.trim_end_matches('/').to_string())
}

fn validate_base_url(value: &str) -> Result<()> {
    let url = value
        .parse::<reqwest::Url>()
        .with_context(|| format!("invalid backend service URL {value:?}"))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        bail!("backend service URL must be absolute HTTP or HTTPS: {value}");
    }
    Ok(())
}

fn openapi_endpoints(document: &Value) -> Vec<Endpoint> {
    let contracts = backend::import_openapi(document)
        .into_iter()
        .map(|contract| (contract.id.clone(), contract))
        .collect::<BTreeMap<_, _>>();
    let mut endpoints = Vec::new();
    let Some(paths) = document.get("paths").and_then(Value::as_object) else {
        return endpoints;
    };
    for (path, item) in paths {
        let Some(methods) = item.as_object() else {
            continue;
        };
        for (method, operation) in methods {
            if !["get", "post", "put", "patch", "delete", "head", "options"]
                .contains(&method.as_str())
            {
                continue;
            }
            let id = operation
                .get("operationId")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| format!("{} {}", method.to_ascii_uppercase(), path));
            let Some(contract) = contracts.get(&id).cloned() else {
                continue;
            };
            let request_content = operation
                .pointer("/requestBody/content")
                .and_then(Value::as_object);
            let content_type = request_content.and_then(preferred_request_content_type);
            let has_parameters = item
                .get("parameters")
                .and_then(Value::as_array)
                .is_some_and(|values| !values.is_empty())
                || operation
                    .get("parameters")
                    .and_then(Value::as_array)
                    .is_some_and(|values| !values.is_empty());
            endpoints.push(Endpoint {
                contract,
                method: method.to_ascii_uppercase(),
                path: path.clone(),
                body_only: request_content.is_some() && !has_parameters,
                content_type,
                response_field: None,
                policy: BackendPolicy::default(),
                transport: Transport::Http,
                schema_source: None,
                client_streaming: false,
                server_streaming: false,
            });
        }
    }
    endpoints.sort_by(|left, right| left.contract.id.cmp(&right.contract.id));
    endpoints
}

fn graphql_endpoints(document: &Value) -> Vec<Endpoint> {
    let Some(schema) = document
        .pointer("/data/__schema")
        .or_else(|| document.get("__schema"))
    else {
        return Vec::new();
    };
    let types = schema
        .get("types")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|value| Some((value.get("name")?.as_str()?, value)))
        .collect::<BTreeMap<_, _>>();
    let contracts = backend::import_service_schema(document)
        .into_iter()
        .map(|contract| (contract.id.clone(), contract))
        .collect::<BTreeMap<_, _>>();
    let mut endpoints = Vec::new();
    for (root_key, keyword) in [("queryType", "query"), ("mutationType", "mutation")] {
        let Some(root_name) = schema
            .pointer(&format!("/{root_key}/name"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        let Some(root) = types.get(root_name) else {
            continue;
        };
        for field in root
            .get("fields")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(name) = field.get("name").and_then(Value::as_str) else {
                continue;
            };
            let Some(contract) = contracts.get(name).cloned() else {
                continue;
            };
            let arguments = field
                .get("args")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|argument| {
                    Some((
                        argument.get("name")?.as_str()?,
                        graphql_type_name(argument.get("type")?)?,
                    ))
                })
                .collect::<Vec<_>>();
            let declarations = arguments
                .iter()
                .map(|(name, ty)| format!("${name}: {ty}"))
                .collect::<Vec<_>>()
                .join(", ");
            let calls = arguments
                .iter()
                .map(|(name, _)| format!("{name}: ${name}"))
                .collect::<Vec<_>>()
                .join(", ");
            let selection = contract
                .output
                .as_ref()
                .map(|domain| graphql_selection(domain, 0))
                .unwrap_or_default();
            let declaration = if declarations.is_empty() {
                String::new()
            } else {
                format!("({declarations})")
            };
            let call = if calls.is_empty() {
                String::new()
            } else {
                format!("({calls})")
            };
            let selection = if selection.is_empty() {
                String::new()
            } else {
                format!(" {{ {selection} }}")
            };
            let query = format!("{keyword} Reproit{declaration} {{ {name}{call}{selection} }}");
            endpoints.push(Endpoint {
                contract,
                method: "POST".into(),
                path: String::new(),
                body_only: true,
                content_type: Some("application/json".into()),
                response_field: Some(name.into()),
                policy: BackendPolicy::default(),
                transport: Transport::Http,
                schema_source: None,
                client_streaming: false,
                server_streaming: false,
            });
            if let Some(endpoint) = endpoints.last_mut() {
                endpoint.path = query;
            }
        }
    }
    endpoints
}

fn graphql_type_name(reference: &Value) -> Option<String> {
    match reference.get("kind")?.as_str()? {
        "NON_NULL" => Some(format!("{}!", graphql_type_name(reference.get("ofType")?)?)),
        "LIST" => Some(format!(
            "[{}]",
            graphql_type_name(reference.get("ofType")?)?
        )),
        _ => reference.get("name")?.as_str().map(str::to_string),
    }
}

fn graphql_selection(domain: &ValueDomain, depth: usize) -> String {
    if depth > 5 {
        return "__typename".into();
    }
    match domain {
        ValueDomain::Object { properties, .. } => properties
            .iter()
            .map(|(name, domain)| {
                let nested = graphql_selection(domain, depth + 1);
                if nested.is_empty() {
                    name.clone()
                } else {
                    format!("{name} {{ {nested} }}")
                }
            })
            .collect::<Vec<_>>()
            .join(" "),
        ValueDomain::Array { items, .. } => graphql_selection(items, depth + 1),
        ValueDomain::OneOf { variants } => variants
            .iter()
            .map(|variant| graphql_selection(variant, depth + 1))
            .find(|selection| !selection.is_empty())
            .unwrap_or_default(),
        ValueDomain::GraphqlAbstract { variants } => {
            let fragments = variants
                .iter()
                .map(|(name, domain)| {
                    format!(
                        "... on {name} {{ {} }}",
                        graphql_selection(domain, depth + 1)
                    )
                })
                .collect::<Vec<_>>()
                .join(" ");
            format!("__typename {fragments}")
        }
        _ => String::new(),
    }
}

fn grpc_endpoints(document: &Value) -> Vec<Endpoint> {
    let streaming = grpc_streaming_modes(document);
    backend::import_service_schema(document)
        .into_iter()
        .map(|mut contract| {
            let (client_streaming, server_streaming) =
                streaming.get(&contract.id).copied().unwrap_or_default();
            if client_streaming {
                contract.input = contract.input.take().map(|input| ValueDomain::Array {
                    items: Box::new(input),
                    min_items: Some(1),
                    max_items: Some(3),
                    unique: false,
                });
            }
            if server_streaming {
                contract.output = contract.output.take().map(|output| ValueDomain::Array {
                    items: Box::new(output),
                    min_items: None,
                    max_items: None,
                    unique: false,
                });
            }
            Endpoint {
                path: contract.id.clone(),
                contract,
                method: "GRPC".into(),
                body_only: true,
                content_type: Some("application/json".into()),
                response_field: None,
                policy: BackendPolicy::default(),
                transport: Transport::Grpc,
                schema_source: None,
                client_streaming,
                server_streaming,
            }
        })
        .collect()
}

fn grpc_streaming_modes(document: &Value) -> BTreeMap<String, (bool, bool)> {
    let mut modes = BTreeMap::new();
    for file in document
        .get("file")
        .or_else(|| document.get("files"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let package = file.get("package").and_then(Value::as_str).unwrap_or("");
        for service in file
            .get("service")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(service_name) = service.get("name").and_then(Value::as_str) else {
                continue;
            };
            let prefix = if package.is_empty() {
                service_name.to_string()
            } else {
                format!("{package}.{service_name}")
            };
            for method in service
                .get("method")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let Some(name) = method.get("name").and_then(Value::as_str) else {
                    continue;
                };
                modes.insert(
                    format!("{prefix}/{name}"),
                    (
                        method
                            .get("clientStreaming")
                            .or_else(|| method.get("client_streaming"))
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                        method
                            .get("serverStreaming")
                            .or_else(|| method.get("server_streaming"))
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                    ),
                );
            }
        }
    }
    modes
}

fn preferred_request_content_type(content: &Map<String, Value>) -> Option<String> {
    content
        .keys()
        .find(|media| {
            let normalized = media.to_ascii_lowercase();
            normalized == "application/json" || normalized.ends_with("+json")
        })
        .cloned()
        .or_else(|| {
            content
                .contains_key("application/x-www-form-urlencoded")
                .then(|| "application/x-www-form-urlencoded".to_string())
        })
}

fn sample_domain(domain: &ValueDomain, seed: u64, include_optional: bool, depth: usize) -> Value {
    if depth > 12 {
        return Value::Null;
    }
    match domain {
        ValueDomain::Any => Value::Null,
        ValueDomain::Null => Value::Null,
        ValueDomain::Boolean => Value::Bool(seed.is_multiple_of(2)),
        ValueDomain::Integer { min, max } => {
            let value = min.unwrap_or(1).max(0);
            Value::from(max.map_or(value, |maximum| value.min(maximum)))
        }
        ValueDomain::ProtoInteger64 { .. } => Value::String((seed.max(1)).to_string()),
        ValueDomain::Number => Value::from(seed.max(1) as f64),
        ValueDomain::String {
            min_length,
            max_length,
            format,
            variants,
            ..
        } => {
            if let Some(value) = variants.first() {
                return Value::String(value.clone());
            }
            let base = match format.as_deref() {
                Some("date-time") => "2026-01-01T00:00:00Z".to_string(),
                Some("date") => "2026-01-01".to_string(),
                Some("uuid") => format!("00000000-0000-4000-8000-{seed:012x}"),
                Some("email") => format!("reproit-{seed}@example.test"),
                Some("uri" | "url") => format!("https://example.test/{seed}"),
                _ => format!("reproit-{seed}"),
            };
            let minimum = min_length.unwrap_or(0);
            let maximum = max_length.unwrap_or(usize::MAX);
            let mut value = base;
            while value.chars().count() < minimum {
                value.push('x');
            }
            if value.chars().count() > maximum {
                value = value.chars().take(maximum).collect();
            }
            Value::String(value)
        }
        ValueDomain::Array {
            items,
            min_items,
            max_items,
            ..
        } => {
            let desired = if include_optional {
                min_items.unwrap_or(1).max(1)
            } else {
                min_items.unwrap_or(0)
            };
            let count = max_items.map_or(desired, |maximum| desired.min(maximum));
            Value::Array(
                (0..count)
                    .map(|index| {
                        sample_domain(
                            items,
                            seed.saturating_add(index as u64),
                            include_optional,
                            depth + 1,
                        )
                    })
                    .collect(),
            )
        }
        ValueDomain::Object {
            required,
            properties,
            ..
        } => Value::Object(
            properties
                .iter()
                .filter(|(name, _)| include_optional || required.contains(*name))
                .map(|(name, property)| {
                    (
                        name.clone(),
                        sample_domain(property, seed, include_optional, depth + 1),
                    )
                })
                .collect(),
        ),
        ValueDomain::OneOf { variants } => variants
            .first()
            .map(|variant| sample_domain(variant, seed, include_optional, depth + 1))
            .unwrap_or(Value::Null),
        ValueDomain::GraphqlAbstract { variants } => variants
            .values()
            .next()
            .map(|variant| sample_domain(variant, seed, include_optional, depth + 1))
            .unwrap_or(Value::Null),
        ValueDomain::Literal { value } => value.clone(),
        ValueDomain::Resource { .. } => Value::String(format!("reproit-{seed}")),
    }
}

fn build_request(endpoint: &Endpoint, base_url: &str, input: Value) -> Result<RequestArtifact> {
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
    })
}

async fn invoke(
    client: &reqwest::Client,
    endpoint: &Endpoint,
    artifact: RequestArtifact,
) -> Result<InvocationResult> {
    if endpoint.transport == Transport::Grpc {
        let output = invoke_grpc(&artifact).await?;
        return Ok(evaluate_invocation(endpoint, &artifact, 200, output));
    }
    let method = artifact.method.parse::<reqwest::Method>()?;
    let mut request = client.request(method, &artifact.url);
    let mut headers = HeaderMap::new();
    for (name, value) in &artifact.headers {
        headers.insert(
            HeaderName::from_bytes(name.as_bytes())?,
            HeaderValue::from_str(value)?,
        );
    }
    for (name, value) in extra_headers()?.iter() {
        headers.insert(name.clone(), value.clone());
    }
    request = request.headers(headers);
    if let Some(body) = &artifact.body {
        if artifact.content_type.as_deref() == Some("application/x-www-form-urlencoded") {
            let object = body
                .as_object()
                .context("form-urlencoded request body must be an object")?;
            let form = object
                .iter()
                .map(|(name, value)| {
                    Ok((
                        name.clone(),
                        value_as_text(value).context("form value is not scalar")?,
                    ))
                })
                .collect::<Result<Vec<_>>>()?;
            let encoded = form
                .iter()
                .map(|(name, value)| format!("{}={}", percent_encode(name), percent_encode(value)))
                .collect::<Vec<_>>()
                .join("&");
            request = request
                .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(encoded);
        } else {
            request = request.json(body);
        }
    }
    let mut response = request
        .send()
        .await
        .with_context(|| format!("calling {} {}", artifact.method, artifact.url))?;
    let status = response.status().as_u16();
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        bail!("response exceeded the {MAX_RESPONSE_BYTES} byte evidence limit");
    }
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            bail!("response exceeded the {MAX_RESPONSE_BYTES} byte evidence limit");
        }
        bytes.extend_from_slice(&chunk);
    }
    let raw_output = if bytes.is_empty() {
        Value::Null
    } else if content_type.contains("json") {
        serde_json::from_slice(&bytes).context("response declared JSON but was invalid")?
    } else if let Ok(value) = serde_json::from_slice(&bytes) {
        value
    } else {
        Value::String(String::from_utf8_lossy(&bytes).into_owned())
    };
    let output = endpoint
        .response_field
        .as_ref()
        .and_then(|field| raw_output.pointer(&format!("/data/{}", escape_pointer(field))))
        .cloned()
        .unwrap_or(raw_output);
    Ok(evaluate_invocation(endpoint, &artifact, status, output))
}

fn evaluate_invocation(
    endpoint: &Endpoint,
    artifact: &RequestArtifact,
    status: u16,
    output: Value,
) -> InvocationResult {
    let trace =
        hex_hash(format!("{}:{}", artifact.operation, artifact.url).as_bytes())[..16].to_string();
    let events = vec![
        BackendEvent {
            sequence: 1,
            trace_id: trace.clone(),
            span_id: "request".into(),
            action_index: 1,
            parent_span_id: None,
            operation: artifact.operation.clone(),
            build: None,
            config_contract: None,
            actor: None,
            tenant: None,
            idempotency_key: None,
            selections: Vec::new(),
            event: BackendEventKind::Start {
                input: artifact.input.clone(),
            },
        },
        BackendEvent {
            sequence: 2,
            trace_id: trace,
            span_id: "request".into(),
            action_index: 1,
            parent_span_id: None,
            operation: artifact.operation.clone(),
            build: None,
            config_contract: None,
            actor: None,
            tenant: None,
            idempotency_key: None,
            selections: Vec::new(),
            event: BackendEventKind::Return {
                output: output.clone(),
                status: Some(status),
                success: (200..400).contains(&status),
                effects_complete: false,
            },
        },
    ];
    let config = BackendConfig {
        enabled: true,
        operations: vec![endpoint.contract.clone()],
        invariants: endpoint.policy.invariants.clone(),
        fleet: endpoint.policy.fleet.clone(),
        ..BackendConfig::default()
    };
    let violations = backend::evaluate(&config, &events);
    InvocationResult {
        status,
        output,
        violations,
    }
}

async fn invoke_grpc(artifact: &RequestArtifact) -> Result<Value> {
    let tool = ensure_grpcurl().await?;
    let url = artifact.url.parse::<reqwest::Url>()?;
    let host = url.host_str().context("gRPC target has no host")?;
    let address = format!(
        "{host}:{}",
        url.port_or_known_default()
            .context("gRPC target has no port")?
    );
    let mut command = tokio::process::Command::new(tool);
    if url.scheme() == "http" {
        command.arg("-plaintext");
    }
    let proto = artifact
        .schema_source
        .clone()
        .or_else(|| std::env::var("REPROIT_GRPC_PROTO").ok().map(PathBuf::from));
    if let Some(proto) = proto {
        let proto = proto.canonicalize()?;
        command
            .arg("-import-path")
            .arg(proto.parent().unwrap_or_else(|| Path::new(".")))
            .arg("-proto")
            .arg(&proto);
    }
    let metadata = extra_headers()?;
    if !metadata.is_empty() {
        command.arg("-expand-headers");
    }
    for (index, (name, value)) in metadata.iter().enumerate() {
        let variable = format!("REPROIT_GRPC_METADATA_{index}");
        command.env(
            &variable,
            value.to_str().context("gRPC metadata is not text")?,
        );
        command
            .arg("-H")
            .arg(format!("{}: ${{{variable}}}", name.as_str(),));
    }
    command
        .arg("-d")
        .arg("@")
        .arg(address)
        .arg(&artifact.operation)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let mut stdin = child
        .stdin
        .take()
        .context("gRPC request stdin unavailable")?;
    let body = artifact.body.as_ref().unwrap_or(&Value::Null);
    if artifact.client_streaming {
        for message in body.as_array().into_iter().flatten() {
            stdin.write_all(&serde_json::to_vec(message)?).await?;
            stdin.write_all(b"\n").await?;
        }
    } else {
        stdin.write_all(&serde_json::to_vec(body)?).await?;
        stdin.write_all(b"\n").await?;
    }
    drop(stdin);
    let output = child.wait_with_output().await?;
    if !output.status.success() {
        bail!(
            "gRPC operation {} failed: {}",
            artifact.operation,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let messages = serde_json::Deserializer::from_slice(&output.stdout)
        .into_iter::<Value>()
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| {
            format!(
                "gRPC operation {} returned non-JSON output",
                artifact.operation
            )
        })?;
    if artifact.server_streaming {
        Ok(Value::Array(messages))
    } else {
        messages
            .into_iter()
            .next()
            .context("gRPC operation returned no JSON response")
    }
}

async fn ensure_grpcurl() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("REPROIT_GRPCURL") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
        bail!("REPROIT_GRPCURL does not point to a file");
    }
    if let Ok(path) = which_tool("grpcurl") {
        return Ok(path);
    }
    let (asset, expected) = grpcurl_asset()?;
    let directory = std::env::current_dir()?.join(".reproit/tools/grpcurl-1.9.3");
    let executable = directory.join(if cfg!(windows) {
        "grpcurl.exe"
    } else {
        "grpcurl"
    });
    if executable.is_file() {
        return Ok(executable);
    }
    std::fs::create_dir_all(&directory)?;
    let url = format!("https://github.com/fullstorydev/grpcurl/releases/download/v1.9.3/{asset}");
    let bytes = reqwest::get(url).await?.error_for_status()?.bytes().await?;
    if hex_hash(&bytes) != expected {
        bail!("downloaded grpcurl archive failed its pinned SHA-256 check");
    }
    if asset.ends_with(".zip") {
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;
        let mut found = false;
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index)?;
            if entry.name().ends_with("grpcurl.exe") {
                let mut output = std::fs::File::create(&executable)?;
                std::io::copy(&mut entry, &mut output)?;
                found = true;
                break;
            }
        }
        if !found {
            bail!("grpcurl archive contained no executable");
        }
    } else {
        let decoder = flate2::read::GzDecoder::new(std::io::Cursor::new(bytes));
        let mut archive = tar::Archive::new(decoder);
        let mut found = false;
        for entry in archive.entries()? {
            let mut entry = entry?;
            if entry.path()?.ends_with("grpcurl") {
                entry.unpack(&executable)?;
                found = true;
                break;
            }
        }
        if !found {
            bail!("grpcurl archive contained no executable");
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(executable)
}

fn which_tool(name: &str) -> Result<PathBuf> {
    let path = std::env::var_os("PATH").context("PATH is unset")?;
    for directory in std::env::split_paths(&path) {
        let candidate = directory.join(if cfg!(windows) {
            format!("{name}.exe")
        } else {
            name.into()
        });
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    bail!("{name} is not installed")
}

fn grpcurl_asset() -> Result<(&'static str, &'static str)> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok((
            "grpcurl_1.9.3_osx_arm64.tar.gz",
            "d8391485e99a728a3a4e82af3fd621f9fdea0c417a74e5122803ad20b207b623",
        )),
        ("macos", "x86_64") => Ok((
            "grpcurl_1.9.3_osx_x86_64.tar.gz",
            "246a6669e58c282dcaf0e9dcb06dd1c8681833d59df24eb83d3123ec64c2d2e5",
        )),
        ("linux", "aarch64") => Ok((
            "grpcurl_1.9.3_linux_arm64.tar.gz",
            "b20a00c1cb82ab81ec32696766d4076e99b4cb5ca0823a71767ba64dbea0f263",
        )),
        ("linux", "x86_64") => Ok((
            "grpcurl_1.9.3_linux_x86_64.tar.gz",
            "a926b62a85787ccf73ef8736b3ae554f1242e39d92bb8767a79d6dd23b11d1d5",
        )),
        ("windows", "x86_64") => Ok((
            "grpcurl_1.9.3_windows_x86_64.zip",
            "895335dfa7be74803eeb5acf3ec5d3b06c1e9483fdda3c7622bdef9ad388f32a",
        )),
        (os, arch) => bail!("grpcurl is not provisioned for {os}/{arch}"),
    }
}

fn extra_headers() -> Result<HeaderMap> {
    let Some(raw) = std::env::var_os("REPROIT_EXTRA_HEADERS") else {
        return Ok(HeaderMap::new());
    };
    let values: BTreeMap<String, String> = serde_json::from_str(&raw.to_string_lossy())
        .context("REPROIT_EXTRA_HEADERS must be a JSON object of strings")?;
    let mut headers = HeaderMap::new();
    for (name, value) in values {
        headers.insert(
            HeaderName::from_bytes(name.as_bytes())?,
            HeaderValue::from_str(&value)?,
        );
    }
    Ok(headers)
}

fn has_fingerprint(result: &InvocationResult, expected: &str) -> bool {
    result
        .violations
        .iter()
        .any(|violation| violation.fingerprint == expected)
}

async fn replay_sequence(
    client: &reqwest::Client,
    setup: &[ReplayStep],
    failing_endpoint: &Endpoint,
    failing_request: &RequestArtifact,
    expected: &str,
) -> Result<bool> {
    maybe_reset_target(client, &failing_request.url).await?;
    for step in setup {
        let endpoint = replay_endpoint(step);
        let result = invoke(client, &endpoint, step.request.clone()).await?;
        if !(200..400).contains(&result.status) || !result.violations.is_empty() {
            return Ok(false);
        }
    }
    let result = invoke(client, failing_endpoint, failing_request.clone()).await?;
    Ok(has_fingerprint(&result, expected))
}

async fn shrink_findings(
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
                for input in structural_reductions(&request.input).into_iter().take(256) {
                    let Ok(candidate) = build_request(&endpoint, base_url, input) else {
                        continue;
                    };
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

fn persist_findings(
    root: &Path,
    schema: &Path,
    schema_sha256: &str,
    seed: u64,
    findings: Vec<FindingCase>,
) -> Result<Vec<Value>> {
    let mut persisted = Vec::new();
    let mut seen = BTreeSet::new();
    for (endpoint, request, setup, mut finding) in findings {
        let fingerprint = finding
            .get("fingerprint")
            .and_then(Value::as_str)
            .context("backend finding has no fingerprint")?;
        if !seen.insert(fingerprint.to_string()) {
            continue;
        }
        let raw_id = repro::finding_id(
            schema_sha256,
            fingerprint,
            seed,
            &[format!("{} {}", request.method, request.url)],
        );
        let public_id = repro::display_finding_id(&raw_id);
        finding["id"] = Value::String(public_id.clone());
        finding["setupSteps"] = Value::from(setup.len());
        let directory = root.join(".reproit/findings").join(&raw_id);
        std::fs::create_dir_all(&directory)?;
        let artifact = BackendFindingArtifact {
            format: "reproit-backend-finding".into(),
            version: 2,
            schema: schema.to_string_lossy().into_owned(),
            schema_sha256: schema_sha256.into(),
            setup,
            failing: ReplayStep {
                contract: endpoint.contract,
                request,
                policy: endpoint.policy,
            },
            finding: finding.clone(),
        };
        std::fs::write(
            directory.join("backend.json"),
            serde_json::to_vec_pretty(&artifact)?,
        )?;
        std::fs::write(
            directory.join("fuzz.md"),
            format!(
                "# Backend finding (seed {seed})\n\n<!-- finding-id: {raw_id} -->\n\n## confirmed repro (0 actions)\n\n```\n```\n\nReplay: `reproit {public_id}`\n"
            ),
        )?;
        persisted.push(finding);
    }
    Ok(persisted)
}

fn persist_run_report(root: &Path, command: &str, report: &Value) -> Result<()> {
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ");
    let directory = root
        .join(".reproit/runs")
        .join(format!("backend-{command}-{stamp}"));
    std::fs::create_dir_all(&directory)?;
    std::fs::write(
        directory.join("backend-report.json"),
        serde_json::to_vec_pretty(report)?,
    )?;
    Ok(())
}

fn emit_report(ctx: &Ctx, command: &str, report: &Value) {
    if ctx.json {
        ctx.emit(report);
        return;
    }
    let findings = report["findings"].as_array().map_or(0, Vec::len);
    let candidates = report["candidates"].as_array().map_or(0, Vec::len);
    let errors = report["executionErrors"].as_array().map_or(0, Vec::len);
    ctx.say(format!(
        "backend {command}: {} operation(s) exercised, {findings} confirmed finding(s), {candidates} candidate(s), {errors} execution error(s)",
        report["exercised"].as_u64().unwrap_or(0)
    ));
    if let Some(values) = report["findings"].as_array() {
        for finding in values {
            ctx.say(format!(
                "  {}  {}: {}",
                finding
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("fnd_unknown"),
                finding
                    .get("operation")
                    .and_then(Value::as_str)
                    .unwrap_or("operation"),
                finding.get("message").and_then(Value::as_str).unwrap_or("")
            ));
        }
    }
}

pub async fn try_replay(ctx: &Ctx, id: &str) -> Result<Option<ExitCode>> {
    let Some(raw_id) = repro::raw_finding_id(id) else {
        return Ok(None);
    };
    let Some(artifact_path) = find_artifact(raw_id)? else {
        return Ok(None);
    };
    let artifact: BackendFindingArtifact = serde_json::from_slice(&std::fs::read(&artifact_path)?)?;
    let expected = artifact
        .finding
        .get("fingerprint")
        .and_then(Value::as_str)
        .context("backend artifact has no finding fingerprint")?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::limited(3))
        .build()?;
    maybe_reset_target(&client, &artifact.failing.request.url).await?;
    for step in artifact.setup {
        let endpoint = replay_endpoint(&step);
        let result = invoke(&client, &endpoint, step.request).await?;
        if !(200..400).contains(&result.status) {
            bail!(
                "backend replay setup operation {} returned {}",
                endpoint.contract.id,
                result.status
            );
        }
    }
    let endpoint = replay_endpoint(&artifact.failing);
    let result = invoke(&client, &endpoint, artifact.failing.request).await?;
    let reproduced = result
        .violations
        .iter()
        .any(|violation| violation.fingerprint == expected);
    let report = json!({
        "command": "backend replay",
        "id": id,
        "reproduced": reproduced,
        "status": result.status,
        "findings": result.violations.iter().map(backend::finding).collect::<Vec<_>>(),
    });
    if ctx.json {
        ctx.emit(&report);
    } else if reproduced {
        ctx.say(format!("{id}: reproduced exactly"));
    } else {
        ctx.say(format!("{id}: no longer reproduces"));
    }
    Ok(Some(if reproduced {
        Exit::Regression.code()
    } else {
        ExitCode::SUCCESS
    }))
}

fn replay_endpoint(step: &ReplayStep) -> Endpoint {
    let graphql = step
        .request
        .body
        .as_ref()
        .is_some_and(|body| body.get("query").is_some());
    Endpoint {
        method: step.request.method.clone(),
        path: String::new(),
        body_only: step.request.body.is_some(),
        content_type: step.request.content_type.clone(),
        response_field: graphql.then(|| step.contract.id.clone()),
        policy: step.policy.clone(),
        transport: if step.request.method == "GRPC" {
            Transport::Grpc
        } else {
            Transport::Http
        },
        schema_source: step.request.schema_source.clone(),
        client_streaming: step.request.client_streaming,
        server_streaming: step.request.server_streaming,
        contract: step.contract.clone(),
    }
}

fn escape_pointer(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}

async fn maybe_reset_target(client: &reqwest::Client, failing_url: &str) -> Result<()> {
    let Some(reset) = std::env::var_os("REPROIT_BACKEND_RESET_URL") else {
        return Ok(());
    };
    let reset = reset.to_string_lossy();
    validate_base_url(&reset).context("REPROIT_BACKEND_RESET_URL")?;
    let failing = failing_url.parse::<reqwest::Url>()?;
    let reset_url = reset.parse::<reqwest::Url>()?;
    if failing.origin() != reset_url.origin() {
        bail!("REPROIT_BACKEND_RESET_URL must use the same origin as the replay target");
    }
    let response = client.post(reset_url).send().await?;
    if !response.status().is_success() {
        bail!("backend reset returned {}", response.status());
    }
    Ok(())
}

fn find_artifact(raw_id: &str) -> Result<Option<PathBuf>> {
    let cwd = std::env::current_dir()?;
    for root in cwd.ancestors() {
        let artifact = root
            .join(".reproit/findings")
            .join(raw_id)
            .join("backend.json");
        if artifact.is_file() {
            return Ok(Some(artifact));
        }
    }
    Ok(None)
}

fn value_as_text(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

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
    Sha256::digest(value)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document() -> Value {
        serde_json::from_str(
            r#"{
              "openapi":"3.0.3",
              "servers":[{"url":"http://127.0.0.1:9999"}],
              "paths":{"/users/{id}":{"get":{
                "operationId":"getUser",
                "parameters":[{"name":"id","in":"path","required":true,"schema":{"type":"integer","minimum":1}}],
                "responses":{"200":{"content":{"application/json":{"schema":{"type":"object","required":["id"],"properties":{"id":{"type":"integer"}}}}}}}
              }}}
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn detects_and_builds_a_structural_openapi_request() {
        let document = document();
        let endpoint = openapi_endpoints(&document).pop().unwrap();
        let input = sample_domain(endpoint.contract.input.as_ref().unwrap(), 7, false, 0);
        let request = build_request(&endpoint, "http://127.0.0.1:9999", input).unwrap();
        assert_eq!(request.method, "GET");
        assert_eq!(request.url, "http://127.0.0.1:9999/users/1");
    }

    #[test]
    fn sample_values_satisfy_their_domains() {
        for domain in [
            ValueDomain::String {
                min_length: Some(12),
                max_length: Some(40),
                pattern: None,
                format: Some("email".into()),
                variants: Vec::new(),
            },
            ValueDomain::Array {
                items: Box::new(ValueDomain::Integer {
                    min: Some(2),
                    max: Some(8),
                }),
                min_items: Some(2),
                max_items: Some(3),
                unique: false,
            },
        ] {
            let sample = sample_domain(&domain, 3, true, 0);
            assert_eq!(domain.mismatch(&sample, "$"), None, "{sample}");
        }
    }

    #[test]
    fn builds_graphql_queries_from_introspection_without_framework_knowledge() {
        let document = json!({"data":{"__schema":{
            "queryType":{"name":"Query"},
            "mutationType":null,
            "subscriptionType":null,
            "types":[
                {"kind":"OBJECT","name":"Query","fields":[{
                    "name":"user",
                    "args":[{"name":"id","type":{"kind":"NON_NULL","name":null,"ofType":{"kind":"SCALAR","name":"ID","ofType":null}}}],
                    "type":{"kind":"OBJECT","name":"User","ofType":null}
                }]},
                {"kind":"OBJECT","name":"User","fields":[
                    {"name":"id","args":[],"type":{"kind":"NON_NULL","name":null,"ofType":{"kind":"SCALAR","name":"ID","ofType":null}}},
                    {"name":"name","args":[],"type":{"kind":"SCALAR","name":"String","ofType":null}}
                ]}
            ]
        }}});
        let endpoint = graphql_endpoints(&document).pop().unwrap();
        let input = sample_domain(endpoint.contract.input.as_ref().unwrap(), 4, false, 0);
        let request = build_request(&endpoint, "http://127.0.0.1:9999/graphql", input).unwrap();
        let query = request.body.unwrap()["query"].as_str().unwrap().to_string();
        assert!(query.contains("query Reproit($id: ID!)"));
        assert!(query.contains("user(id: $id)"));
        assert!(query.contains("id"));
        assert!(query.contains("name"));
    }

    #[test]
    fn imports_grpc_streaming_as_structural_message_arrays() {
        let document = json!({"file":[{
            "package":"reproit.validation",
            "messageType":[
                {"name":"Request","field":[{"name":"name","type":"TYPE_STRING"}]},
                {"name":"Reply","field":[{"name":"message","type":"TYPE_STRING"}]}
            ],
            "service":[{"name":"Streaming","method":[{
                "name":"Chat",
                "inputType":".reproit.validation.Request",
                "outputType":".reproit.validation.Reply",
                "clientStreaming":true,
                "serverStreaming":true
            }]}]
        }]});
        let endpoint = grpc_endpoints(&document).pop().unwrap();
        assert!(endpoint.client_streaming);
        assert!(endpoint.server_streaming);
        assert!(matches!(
            endpoint.contract.input,
            Some(ValueDomain::Array { .. })
        ));
        assert!(matches!(
            endpoint.contract.output,
            Some(ValueDomain::Array { .. })
        ));
    }

    #[test]
    fn declared_operation_can_make_a_safe_grpc_query_scannable() {
        let mut imported = OperationContract {
            id: "inventory.Reader/Get".into(),
            authority: backend::Authority::Schema,
            input: None,
            output: None,
            outputs_by_status: BTreeMap::new(),
            success_statuses: Vec::new(),
            read_only: false,
            idempotent: false,
            tenant_isolated: false,
            promised_effects: Vec::new(),
        };
        let mut declared = imported.clone();
        declared.authority = backend::Authority::Declared;
        declared.read_only = true;
        declared.idempotent = true;
        apply_operation_override(&mut imported, &declared);
        assert!(imported.read_only);
        assert!(imported.idempotent);
    }
}
