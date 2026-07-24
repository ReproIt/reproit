use super::*;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Scan-time trace identity attached to one inspected request so an
/// instrumented target returns its effect trail as `x-reproit-events`.
pub(super) struct InspectTrace {
    pub(super) trace_id: String,
    pub(super) action_index: u32,
}

/// The adapter effect trail of one live invocation. `Absent` and `Malformed`
/// are distinguished so inspection can say why no trail is shown instead of
/// treating a broken header as an empty one.
pub(super) enum AdapterTrail {
    Absent,
    Events(Vec<BackendEvent>),
    Malformed,
}

/// Run-level adapter-tier evidence. Every live HTTP invocation is traced, so
/// an instrumented target answers with its `x-reproit-events` trail; the run
/// summary states the resulting verdict tier once. A CLI process executes one
/// scan/fuzz command, so process-global flags are the run scope.
static TRACED_REQUESTS: AtomicBool = AtomicBool::new(false);
static ADAPTER_EVENTS: AtomicBool = AtomicBool::new(false);
static TRACE_COUNTER: AtomicU64 = AtomicU64::new(1);

/// The one-line verdict-fidelity statement for the run summary. None when the
/// run sent no traced HTTP request (nothing honest to claim).
pub(super) fn adapter_tier_line() -> Option<&'static str> {
    if ADAPTER_EVENTS.load(Ordering::Relaxed) {
        Some("tier: effect-grounded (adapter detected)")
    } else if TRACED_REQUESTS.load(Ordering::Relaxed) {
        Some("tier: black-box (no adapter; response-level checks only)")
    } else {
        None
    }
}

pub(super) async fn invoke(
    client: &reqwest::Client,
    endpoint: &Endpoint,
    artifact: RequestArtifact,
) -> Result<InvocationResult> {
    let trace = InspectTrace {
        trace_id: format!("run{:012x}", TRACE_COUNTER.fetch_add(1, Ordering::Relaxed)),
        action_index: 1,
    };
    let http = endpoint.transport == Transport::Http;
    let (result, trail) = invoke_traced(client, endpoint, artifact, Some(trace)).await?;
    if http {
        TRACED_REQUESTS.store(true, Ordering::Relaxed);
        if matches!(trail, AdapterTrail::Events(_)) {
            ADAPTER_EVENTS.store(true, Ordering::Relaxed);
        }
    }
    Ok(result)
}

/// `invoke`, plus (when `trace` is set) the scan-time correlation headers on
/// the request and the decoded `x-reproit-events` trail from the response.
/// With `trace == None` the request and result are byte-identical to the
/// pre-existing `invoke` behavior.
pub(super) async fn invoke_traced(
    client: &reqwest::Client,
    endpoint: &Endpoint,
    artifact: RequestArtifact,
    trace: Option<InspectTrace>,
) -> Result<(InvocationResult, AdapterTrail)> {
    if endpoint.transport == Transport::Grpc {
        let output = invoke_grpc(&artifact).await?;
        return Ok((
            evaluate_invocation(endpoint, &artifact, 200, output),
            AdapterTrail::Absent,
        ));
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
    if let Some(trace) = &trace {
        headers.insert(
            HeaderName::from_static("x-reproit-trace"),
            HeaderValue::from_str(&trace.trace_id)?,
        );
        headers.insert(
            HeaderName::from_static("x-reproit-action"),
            HeaderValue::from_str(&trace.action_index.to_string())?,
        );
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
    let adapter = if trace.is_some() {
        decode_adapter_trail(response.headers().get("x-reproit-events"))
    } else {
        AdapterTrail::Absent
    };
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
    Ok((
        evaluate_invocation(endpoint, &artifact, status, output),
        adapter,
    ))
}

/// SDK adapters encode the finished trace as base64url (no padding) over the
/// JSON event array, capped at 60 KB by the SDK. Decoding is bounded and a
/// broken header is reported as `Malformed`, never silently emptied.
fn decode_adapter_trail(header: Option<&HeaderValue>) -> AdapterTrail {
    const MAX_TRAIL_HEADER_BYTES: usize = 64 * 1024;
    let Some(header) = header else {
        return AdapterTrail::Absent;
    };
    let Ok(encoded) = header.to_str() else {
        return AdapterTrail::Malformed;
    };
    if encoded.len() > MAX_TRAIL_HEADER_BYTES {
        return AdapterTrail::Malformed;
    }
    use base64::Engine as _;
    let Ok(bytes) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(encoded) else {
        return AdapterTrail::Malformed;
    };
    match serde_json::from_slice::<Vec<BackendEvent>>(&bytes) {
        Ok(mut events) => {
            events.sort_by_key(|event| event.sequence);
            AdapterTrail::Events(events)
        }
        Err(_) => AdapterTrail::Malformed,
    }
}

pub(super) fn evaluate_invocation(
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
        resources: endpoint.policy.resources.clone(),
        proofs: endpoint.policy.proofs.clone(),
        fleet: endpoint.policy.fleet.clone(),
        ..BackendConfig::default()
    };
    let violations = backend::evaluate(&config, &events);
    InvocationResult {
        status,
        output,
        violations,
        events,
    }
}

pub(super) async fn invoke_grpc(artifact: &RequestArtifact) -> Result<Value> {
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

pub(super) async fn ensure_grpcurl() -> Result<PathBuf> {
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
    let directory = layout::tool_dir(&std::env::current_dir()?, "grpcurl-1.9.3");
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

pub(super) fn which_tool(name: &str) -> Result<PathBuf> {
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

pub(super) fn grpcurl_asset() -> Result<(&'static str, &'static str)> {
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

pub(super) fn extra_headers() -> Result<HeaderMap> {
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
