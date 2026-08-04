use super::debug_control;
use super::model::{
    CommandProvider, DebugProfile, DockerComposeCell, ProviderCatalog, ReproductionCell,
};
use crate::domain::execution::ExecutionPhase;
use anyhow::{Context, Result};
use reproit_protocol::{
    CellReceipt, CleanupStatus, DebugEndpoint, DiagnosticReceipt, ExecutionDestination,
    ReproductionPlan, SourceMapping, CELL_RECEIPT_VERSION, DIAGNOSTIC_RECEIPT_VERSION,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;

const MAX_DOCKER_OUTPUT_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ExecutionMode {
    Authoritative,
    Diagnostic,
}

pub(super) struct CellSession {
    root: PathBuf,
    cell_id: String,
    cell: DockerComposeCell,
    run_id: String,
    receipt_id: String,
    project_name: String,
    override_path: PathBuf,
    configuration_sha256: String,
    services: Vec<String>,
    mode: ExecutionMode,
}

impl CellSession {
    pub(super) async fn prepare(
        root: &Path,
        plan: &ReproductionPlan,
        providers: &[(String, &CommandProvider)],
        catalog: &ProviderCatalog,
        mode: ExecutionMode,
    ) -> Result<Option<Self>> {
        let Some(cell_id) = selected_cell(plan, providers)? else {
            return Ok(None);
        };
        let ReproductionCell::DockerCompose(cell) = catalog
            .cells
            .get(&cell_id)
            .context("selected execution cell disappeared")?
            .clone();
        if mode == ExecutionMode::Diagnostic && cell.debug.is_none() {
            anyhow::bail!("cell `{cell_id}` has no debug profile");
        }
        let root = root.canonicalize().context("resolving checkout root")?;
        let run_id = random_id("run")?;
        let receipt_id = random_id("cell")?;
        let project_name = compose_project_name(&run_id);
        let run_directory = root.join(".reproit").join("cells").join(&run_id);
        std::fs::create_dir_all(&run_directory)
            .with_context(|| format!("creating {}", run_directory.display()))?;
        let override_path = run_directory.join("compose.override.json");
        let services = service_names(&cell);
        write_override(
            &override_path,
            &services,
            &receipt_id,
            cell.platform.as_deref(),
            mode.then_debug(&cell),
        )?;
        let mut session = Self {
            root,
            cell_id,
            cell,
            run_id,
            receipt_id,
            project_name,
            override_path,
            configuration_sha256: String::new(),
            services,
            mode,
        };
        let effective = session
            .compose_output(&["config", "--format", "json"])
            .await?;
        validate_effective_config(&session.root, &session.cell, &effective)?;
        session.configuration_sha256 = normalized_configuration_sha256(
            &effective,
            &session.receipt_id,
            &session.project_name,
        )?;
        Ok(Some(session))
    }

    pub(super) async fn before_phase(&self, phase: ExecutionPhase) -> Result<bool> {
        match phase {
            ExecutionPhase::Reserve if !self.cell.dependency_services.is_empty() => {
                self.compose_up(&self.cell.dependency_services).await?;
                Ok(true)
            }
            ExecutionPhase::Launch => {
                self.compose_up(std::slice::from_ref(&self.cell.application_service))
                    .await?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    pub(super) async fn debug_receipt(
        &self,
        occurrence_id: &str,
    ) -> Result<Option<DiagnosticReceipt>> {
        if self.mode != ExecutionMode::Diagnostic {
            return Ok(None);
        }
        let profile = self
            .cell
            .debug
            .as_ref()
            .context("debug profile disappeared")?;
        let endpoint = self.wait_for_debug_endpoint(profile.port).await?;
        let receipt = DiagnosticReceipt {
            version: DIAGNOSTIC_RECEIPT_VERSION,
            receipt_id: random_id("diag")?,
            occurrence_id: occurrence_id.to_string(),
            run_id: self.run_id.clone(),
            cell_receipt_id: self.receipt_id.clone(),
            debugger: profile.debugger,
            endpoint,
            source_mappings: vec![SourceMapping {
                local_root: self
                    .root
                    .join(&profile.local_source_root)
                    .display()
                    .to_string(),
                target_root: profile.target_source_root.display().to_string(),
            }],
            pause_point: "before-trigger".into(),
            perturbations: vec![
                "debugger-command-override".into(),
                "debugger-port-forward".into(),
                "attach-before-trigger-pause".into(),
            ],
            authoritative: false,
        };
        receipt.validate().map_err(|error| anyhow::anyhow!(error))?;
        write_debugger_files(&self.root, &self.run_id, &receipt)?;
        Ok(Some(receipt))
    }

    pub(super) async fn cleanup(
        &self,
        state_fingerprints: BTreeMap<String, String>,
    ) -> (CellReceipt, Option<anyhow::Error>) {
        let down = self
            .compose_output(&["down", "--volumes", "--remove-orphans", "--timeout", "10"])
            .await;
        let remaining = self.remaining_owned_resources().await;
        let mut error = cleanup_error(down, remaining);
        let mut receipt = CellReceipt {
            version: CELL_RECEIPT_VERSION,
            receipt_id: self.receipt_id.clone(),
            run_id: self.run_id.clone(),
            cell_id: self.cell_id.clone(),
            driver: "docker-compose".into(),
            project_name: self.project_name.clone(),
            configuration_sha256: self.configuration_sha256.clone(),
            services: self.services.clone(),
            state_fingerprints,
            cleanup: if error.is_none() {
                CleanupStatus::Verified
            } else {
                CleanupStatus::Failed
            },
            missing_capabilities: Vec::new(),
        };
        let receipt_path = self
            .override_path
            .parent()
            .expect("cell override always has a parent")
            .join("cell-receipt.json");
        if let Err(protocol_error) = receipt.validate() {
            receipt.cleanup = CleanupStatus::Failed;
            error = Some(anyhow::anyhow!(
                "generated cell receipt failed protocol validation: {protocol_error}"
            ));
        }
        if let Err(write_error) = std::fs::write(
            &receipt_path,
            serde_json::to_vec_pretty(&receipt).expect("cell receipt serializes"),
        ) {
            receipt.cleanup = CleanupStatus::Failed;
            error = Some(
                anyhow::Error::new(write_error)
                    .context(format!("writing cell receipt {}", receipt_path.display())),
            );
        }
        (receipt, error)
    }

    async fn compose_up(&self, services: &[String]) -> Result<()> {
        let wait_seconds = self.cell.timeout_ms.div_ceil(1_000).to_string();
        let mut arguments = vec![
            "up",
            "-d",
            "--wait",
            "--wait-timeout",
            wait_seconds.as_str(),
            "--pull",
            "never",
        ];
        arguments.push(if self.cell.allow_local_build {
            "--build"
        } else {
            "--no-build"
        });
        arguments.extend(services.iter().map(String::as_str));
        self.compose_output(&arguments).await.map(drop)
    }

    async fn wait_for_debug_endpoint(&self, container_port: u16) -> Result<DebugEndpoint> {
        let port = container_port.to_string();
        let service = self.cell.application_service.as_str();
        let attempts = (self.cell.timeout_ms / 100).clamp(1, 600);
        for _ in 0..attempts {
            if let Ok(output) = self.compose_output(&["port", service, &port]).await {
                if let Some(endpoint) = parse_debug_endpoint(&output) {
                    if tokio::net::TcpStream::connect((endpoint.host.as_str(), endpoint.port))
                        .await
                        .is_ok()
                    {
                        return Ok(endpoint);
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        anyhow::bail!("debug endpoint did not become ready within the cell timeout")
    }

    async fn compose_output(&self, arguments: &[&str]) -> Result<String> {
        let compose_file = self.root.join(&self.cell.compose_file);
        let mut command = Command::new("docker");
        command
            .arg("compose")
            .arg("--project-directory")
            .arg(&self.root)
            .arg("--project-name")
            .arg(&self.project_name)
            .arg("--file")
            .arg(compose_file)
            .arg("--file")
            .arg(&self.override_path)
            .args(arguments)
            .env("COMPOSE_IGNORE_ORPHANS", "false")
            .kill_on_drop(true);
        let output = tokio::time::timeout(
            Duration::from_millis(self.cell.timeout_ms),
            command.output(),
        )
        .await
        .context("docker compose timed out")?
        .context("starting docker compose")?;
        if output.stdout.len() > MAX_DOCKER_OUTPUT_BYTES
            || output.stderr.len() > MAX_DOCKER_OUTPUT_BYTES
        {
            anyhow::bail!("docker compose output exceeded 1 MiB");
        }
        if !output.status.success() {
            anyhow::bail!(
                "docker compose failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        String::from_utf8(output.stdout).context("docker compose returned non-UTF-8 output")
    }

    async fn remaining_owned_resources(&self) -> Result<Vec<String>> {
        let filter = format!("label=com.docker.compose.project={}", self.project_name);
        let mut remaining = Vec::new();
        for (kind, arguments) in [
            ("container", vec!["ps", "-aq", "--filter", filter.as_str()]),
            (
                "volume",
                vec!["volume", "ls", "-q", "--filter", filter.as_str()],
            ),
            (
                "network",
                vec!["network", "ls", "-q", "--filter", filter.as_str()],
            ),
        ] {
            let output = bounded_docker_output(&arguments, self.cell.timeout_ms).await?;
            for id in output.lines().filter(|line| !line.trim().is_empty()) {
                remaining.push(format!("{kind}:{}", id.trim()));
            }
        }
        Ok(remaining)
    }
}

trait DebugSelection {
    fn then_debug(self, cell: &DockerComposeCell) -> Option<&DebugProfile>;
}

impl DebugSelection for ExecutionMode {
    fn then_debug(self, cell: &DockerComposeCell) -> Option<&DebugProfile> {
        (self == Self::Diagnostic)
            .then_some(cell.debug.as_ref())
            .flatten()
    }
}

fn selected_cell(
    plan: &ReproductionPlan,
    providers: &[(String, &CommandProvider)],
) -> Result<Option<String>> {
    let cells = providers
        .iter()
        .filter_map(|(_, provider)| provider.cell.as_deref())
        .collect::<BTreeSet<_>>();
    let has_host_provider = providers
        .iter()
        .any(|(_, provider)| provider.cell.is_none());
    if cells.len() > 1 || (!cells.is_empty() && has_host_provider) {
        anyhow::bail!("a plan must bind entirely to one execution cell or entirely to the host");
    }
    let selected = cells.into_iter().next().map(str::to_string);
    let expected = if selected.is_some() {
        ExecutionDestination::LocalCompose
    } else {
        ExecutionDestination::LocalProcess
    };
    if plan.destination != expected {
        anyhow::bail!("plan destination does not match its trusted provider cell bindings");
    }
    Ok(selected)
}

fn service_names(cell: &DockerComposeCell) -> Vec<String> {
    let mut services = cell.dependency_services.clone();
    services.push(cell.application_service.clone());
    services
}

fn write_override(
    path: &Path,
    services: &[String],
    receipt_id: &str,
    platform: Option<&str>,
    debug: Option<&DebugProfile>,
) -> Result<()> {
    let mut definitions = serde_json::Map::new();
    for service in services {
        let mut definition = json!({
            "labels": {
                "dev.reproit.owner": receipt_id,
                "dev.reproit.cell": "true"
            }
        });
        if let Some(platform) = platform {
            definition["platform"] = Value::String(platform.to_string());
        }
        if let Some(profile) = debug.filter(|_| services.last() == Some(service)) {
            definition["command"] = serde_json::to_value(&profile.argv)?;
            definition["ports"] = json!([{
                "target": profile.port,
                "published": "0",
                "host_ip": "127.0.0.1",
                "protocol": "tcp"
            }]);
        }
        definitions.insert(service.clone(), definition);
    }
    let override_value = json!({
        "services": definitions,
        "networks": { "default": { "internal": true } }
    });
    std::fs::write(path, serde_json::to_vec_pretty(&override_value)?)
        .with_context(|| format!("writing {}", path.display()))
}

fn validate_effective_config(root: &Path, cell: &DockerComposeCell, raw: &str) -> Result<()> {
    let value: Value = serde_json::from_str(raw).context("parsing effective Compose config")?;
    let services = value
        .get("services")
        .and_then(Value::as_object)
        .context("effective Compose config has no services")?;
    for service_name in service_names(cell) {
        let service = services
            .get(&service_name)
            .with_context(|| format!("Compose service `{service_name}` is missing"))?;
        validate_service(root, &service_name, service, cell.allow_local_build)?;
    }
    let networks = value
        .get("networks")
        .and_then(Value::as_object)
        .context("effective Compose config has no networks")?;
    if networks.is_empty()
        || networks
            .values()
            .any(|network| network.get("internal").and_then(Value::as_bool) != Some(true))
    {
        anyhow::bail!("every effective Compose cell network must be internal");
    }
    Ok(())
}

fn normalized_configuration_sha256(
    raw: &str,
    receipt_id: &str,
    project_name: &str,
) -> Result<String> {
    let mut value: Value = serde_json::from_str(raw).context("parsing effective Compose config")?;
    let object = value
        .as_object_mut()
        .context("effective Compose config is not an object")?;
    if object.get("name").and_then(Value::as_str) == Some(project_name) {
        object.remove("name");
    }
    if let Some(services) = object.get_mut("services").and_then(Value::as_object_mut) {
        for service in services.values_mut() {
            let Some(labels) = service.get_mut("labels").and_then(Value::as_object_mut) else {
                continue;
            };
            if labels.get("dev.reproit.owner").and_then(Value::as_str) == Some(receipt_id) {
                labels.remove("dev.reproit.owner");
            }
        }
    }
    for section in ["networks", "volumes"] {
        let Some(resources) = object.get_mut(section).and_then(Value::as_object_mut) else {
            continue;
        };
        for resource in resources.values_mut() {
            let Some(resource) = resource.as_object_mut() else {
                continue;
            };
            if resource
                .get("name")
                .and_then(Value::as_str)
                .is_some_and(|name| name.starts_with(project_name))
            {
                resource.remove("name");
            }
        }
    }
    Ok(sha256_raw(&serde_json::to_vec(&value)?))
}

fn validate_service(root: &Path, name: &str, service: &Value, allow_build: bool) -> Result<()> {
    if service.get("privileged").and_then(Value::as_bool) == Some(true)
        || service.get("network_mode").and_then(Value::as_str) == Some("host")
        || service.get("pid").and_then(Value::as_str) == Some("host")
        || service.get("ipc").and_then(Value::as_str) == Some("host")
        || service
            .get("devices")
            .is_some_and(|value| !value.as_array().is_some_and(Vec::is_empty))
    {
        anyhow::bail!("Compose service `{name}` requests a host-level capability");
    }
    let pinned = service
        .get("image")
        .and_then(Value::as_str)
        .is_some_and(is_pinned_image);
    if !pinned && !(allow_build && service.get("build").is_some()) {
        anyhow::bail!(
            "Compose service `{name}` needs a digest-pinned image or allowed local build"
        );
    }
    validate_ports(name, service.get("ports"))?;
    validate_mounts(root, name, service.get("volumes"))
}

fn validate_ports(name: &str, ports: Option<&Value>) -> Result<()> {
    let Some(ports) = ports.and_then(Value::as_array) else {
        return Ok(());
    };
    for port in ports {
        let host = port.get("host_ip").and_then(Value::as_str);
        if !matches!(host, Some("127.0.0.1" | "::1")) {
            anyhow::bail!("Compose service `{name}` publishes a non-loopback port");
        }
    }
    Ok(())
}

fn validate_mounts(root: &Path, name: &str, mounts: Option<&Value>) -> Result<()> {
    let Some(mounts) = mounts.and_then(Value::as_array) else {
        return Ok(());
    };
    for mount in mounts {
        if mount.get("type").and_then(Value::as_str) != Some("bind") {
            continue;
        }
        let source = mount
            .get("source")
            .and_then(Value::as_str)
            .context("bind mount omitted its source")?;
        let source = Path::new(source);
        if !source.starts_with(root) {
            anyhow::bail!("Compose service `{name}` bind mount escapes the checkout");
        }
    }
    Ok(())
}

fn is_pinned_image(image: &str) -> bool {
    let Some((_, digest)) = image.rsplit_once("@sha256:") else {
        return false;
    };
    digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn parse_debug_endpoint(output: &str) -> Option<DebugEndpoint> {
    let endpoint = output.lines().next()?.trim();
    let (host, port) = endpoint.rsplit_once(':')?;
    let host = host.trim_matches(['[', ']']);
    if host != "127.0.0.1" {
        return None;
    }
    Some(DebugEndpoint {
        host: host.to_string(),
        port: port.parse().ok()?,
    })
}

fn vscode_descriptor(receipt: &DiagnosticReceipt) -> Option<Value> {
    let mapping = receipt.source_mappings.first()?;
    use reproit_protocol::DebuggerKind;
    let configuration = match receipt.debugger {
        DebuggerKind::NodeInspector => json!({
            "name": "Reproit: attach before trigger",
            "type": "node",
            "request": "attach",
            "address": receipt.endpoint.host,
            "port": receipt.endpoint.port,
            "localRoot": mapping.local_root,
            "remoteRoot": mapping.target_root,
        }),
        DebuggerKind::ChromeDevtools => json!({
            "name": "Reproit: attach before trigger",
            "type": "chrome",
            "request": "attach",
            "address": receipt.endpoint.host,
            "port": receipt.endpoint.port,
            "webRoot": mapping.local_root,
        }),
        DebuggerKind::Gdb | DebuggerKind::Lldb => json!({
            "name": "Reproit: attach before trigger",
            "type": "cppdbg",
            "request": "launch",
            "program": "${input:reproitProgram}",
            "miDebuggerServerAddress": format!(
                "{}:{}",
                receipt.endpoint.host, receipt.endpoint.port
            ),
            "sourceFileMap": { mapping.target_root.clone(): mapping.local_root.clone() },
        }),
        DebuggerKind::Jdwp => json!({
            "name": "Reproit: attach before trigger",
            "type": "java",
            "request": "attach",
            "hostName": receipt.endpoint.host,
            "port": receipt.endpoint.port,
        }),
        DebuggerKind::Dotnet | DebuggerKind::LanguageSpecific => return None,
    };
    Some(json!({
        "version": "0.2.0",
        "configurations": [configuration],
        "inputs": [{
            "id": "reproitProgram",
            "type": "promptString",
            "description": "Program path inside the reproduction cell"
        }]
    }))
}

async fn bounded_docker_output(arguments: &[&str], timeout_ms: u64) -> Result<String> {
    let output = tokio::time::timeout(
        Duration::from_millis(timeout_ms),
        Command::new("docker")
            .args(arguments)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("docker resource inspection timed out")?
    .context("starting docker resource inspection")?;
    if !output.status.success() || output.stdout.len() > MAX_DOCKER_OUTPUT_BYTES {
        anyhow::bail!("docker resource inspection failed or exceeded 1 MiB");
    }
    String::from_utf8(output.stdout).context("docker returned non-UTF-8 output")
}

fn cleanup_error(down: Result<String>, remaining: Result<Vec<String>>) -> Option<anyhow::Error> {
    if let Err(error) = down {
        return Some(error.context("tearing down the Compose cell"));
    }
    match remaining {
        Err(error) => Some(error.context("verifying Compose cell cleanup")),
        Ok(resources) if !resources.is_empty() => Some(anyhow::anyhow!(
            "owned resources remain: {}",
            resources.join(", ")
        )),
        Ok(_) => None,
    }
}

pub(super) fn write_debugger_files(
    root: &Path,
    run_id: &str,
    receipt: &DiagnosticReceipt,
) -> Result<()> {
    let directory = root.join(".reproit").join("cells").join(run_id);
    let receipt_path = directory.join("diagnostic-receipt.json");
    debug_control::write_private(&receipt_path, &serde_json::to_vec_pretty(receipt)?)?;
    let generic_path = directory.join("debug-session.json");
    debug_control::write_private(&generic_path, &serde_json::to_vec_pretty(receipt)?)?;
    if let Some(descriptor) = vscode_descriptor(receipt) {
        let vscode_path = directory.join("launch.json");
        debug_control::write_private(&vscode_path, &serde_json::to_vec_pretty(&descriptor)?)?;
        eprintln!("VS Code configuration: {}", vscode_path.display());
    }
    eprintln!("Generic debugger session: {}", generic_path.display());
    Ok(())
}

pub(super) fn random_id(prefix: &str) -> Result<String> {
    let mut bytes = [0_u8; 12];
    getrandom::fill(&mut bytes).context("reading operating-system randomness")?;
    Ok(format!("{prefix}_{}", encode_hex(&bytes)))
}

fn compose_project_name(run_id: &str) -> String {
    format!("reproit-{}", run_id.trim_start_matches("run_"))
}

fn sha256_raw(bytes: &[u8]) -> String {
    encode_hex(&Sha256::digest(bytes))
}

fn encode_hex(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write;
        write!(&mut encoded, "{byte:02x}").unwrap();
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::super::model::{CommandObservation, ObservationMatcher};
    use super::super::CATALOG_VERSION;
    use super::*;
    use reproit_protocol::{
        MechanismAuthority, ObservationAuthority, ObservationTarget, PLAN_VERSION,
    };

    #[test]
    fn debug_endpoint_accepts_only_loopback() {
        assert_eq!(
            parse_debug_endpoint("127.0.0.1:49152\n"),
            Some(DebugEndpoint {
                host: "127.0.0.1".into(),
                port: 49_152,
            })
        );
        assert_eq!(parse_debug_endpoint("0.0.0.0:49152\n"), None);
    }

    #[test]
    fn effective_service_rejects_unpinned_images_and_public_ports() {
        let root = Path::new("/tmp/checkout");
        let unpinned = json!({ "image": "postgres:latest" });
        assert!(validate_service(root, "db", &unpinned, false).is_err());
        let public = json!({
            "image": format!("postgres@sha256:{}", "a".repeat(64)),
            "ports": [{ "host_ip": "0.0.0.0", "target": 5432 }]
        });
        assert!(validate_service(root, "db", &public, false).is_err());
    }

    #[test]
    fn configuration_digest_ignores_only_run_ownership_values() {
        let first = json!({
            "name": "reproit-first",
            "services": { "app": { "labels": { "dev.reproit.owner": "cell_first" } } },
            "networks": { "default": { "name": "reproit-first_default" } }
        });
        let second = json!({
            "name": "reproit-second",
            "services": { "app": { "labels": { "dev.reproit.owner": "cell_second" } } },
            "networks": { "default": { "name": "reproit-second_default" } }
        });
        assert_eq!(
            normalized_configuration_sha256(&first.to_string(), "cell_first", "reproit-first")
                .unwrap(),
            normalized_configuration_sha256(&second.to_string(), "cell_second", "reproit-second")
                .unwrap()
        );
    }

    #[test]
    fn node_debugger_descriptor_is_attach_before_trigger() {
        let receipt = DiagnosticReceipt {
            version: DIAGNOSTIC_RECEIPT_VERSION,
            receipt_id: "diag_test".into(),
            occurrence_id: "occ_test".into(),
            run_id: "run_test".into(),
            cell_receipt_id: "cell_test".into(),
            debugger: reproit_protocol::DebuggerKind::NodeInspector,
            endpoint: DebugEndpoint {
                host: "127.0.0.1".into(),
                port: 49_152,
            },
            source_mappings: vec![SourceMapping {
                local_root: "/checkout".into(),
                target_root: "/workspace".into(),
            }],
            pause_point: "before-trigger".into(),
            perturbations: vec!["debugger-command-override".into()],
            authoritative: false,
        };
        let descriptor = vscode_descriptor(&receipt).unwrap();
        assert_eq!(descriptor["configurations"][0]["request"], "attach");
        assert_eq!(descriptor["configurations"][0]["port"], 49_152);
    }

    #[tokio::test]
    #[ignore = "requires a working Docker daemon"]
    async fn compose_cell_owns_launch_and_verified_cleanup() {
        let Some(image) = local_composer_image() else {
            return;
        };
        let root = docker_test_root();
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("compose.yaml"),
            format!("services:\n  app:\n    image: {image}\n    command: [\"sleep\", \"60\"]\n"),
        )
        .unwrap();
        let cell = DockerComposeCell {
            compose_file: "compose.yaml".into(),
            application_service: "app".into(),
            dependency_services: Vec::new(),
            allow_local_build: false,
            platform: None,
            timeout_ms: 120_000,
            debug: None,
        };
        let provider = test_provider("test-cell");
        let catalog = ProviderCatalog {
            version: CATALOG_VERSION,
            cells: BTreeMap::from([("test-cell".into(), ReproductionCell::DockerCompose(cell))]),
            providers: BTreeMap::from([("launch".into(), provider.clone())]),
        };
        let plan = ReproductionPlan {
            version: PLAN_VERSION,
            id: "plan_test".into(),
            occurrence_id: "occ_test".into(),
            target: "test".into(),
            destination: ExecutionDestination::LocalCompose,
            bindings: Vec::new(),
            observation: ObservationTarget {
                observation: reproit_protocol::ObservationKind::Exit,
                identity: "test".into(),
                authority: ObservationAuthority::RuntimeDiagnosis,
            },
        };
        let providers = vec![("launch".into(), catalog.providers.get("launch").unwrap())];
        let session = CellSession::prepare(
            &root,
            &plan,
            &providers,
            &catalog,
            ExecutionMode::Authoritative,
        )
        .await
        .unwrap()
        .unwrap();
        session.before_phase(ExecutionPhase::Launch).await.unwrap();
        let (receipt, error) = session.cleanup(BTreeMap::new()).await;
        assert!(error.is_none(), "{error:?}");
        assert_eq!(receipt.cleanup, CleanupStatus::Verified);
        std::fs::remove_dir_all(&root).unwrap();
    }

    fn test_provider(cell: &str) -> CommandProvider {
        CommandProvider {
            authority: MechanismAuthority::TrustedCheckout,
            phase: ExecutionPhase::Launch,
            capabilities: BTreeSet::new(),
            source: None,
            cell: Some(cell.into()),
            debug: None,
            argv: vec!["true".into()],
            environment: BTreeMap::new(),
            working_directory: None,
            timeout_ms: 1_000,
            clean_exit_codes: vec![0],
            observation: Some(CommandObservation {
                identity: "test".into(),
                matcher: ObservationMatcher::ExitCode { code: 0 },
            }),
            state_fingerprint: None,
            cleanup: None,
        }
    }

    fn docker_test_root() -> PathBuf {
        let mut bytes = [0_u8; 8];
        getrandom::fill(&mut bytes).unwrap();
        std::env::temp_dir().join(format!("reproit-cell-test-{}", encode_hex(&bytes)))
    }

    fn local_composer_image() -> Option<String> {
        let output = std::process::Command::new("docker")
            .args([
                "image",
                "ls",
                "composer:2",
                "--digests",
                "--format",
                "{{.Repository}}@{{.Digest}}",
            ])
            .output()
            .ok()?;
        let image = String::from_utf8(output.stdout).ok()?;
        let image = image.lines().next()?.trim();
        is_pinned_image(image).then(|| image.to_string())
    }
}
