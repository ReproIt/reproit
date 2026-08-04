use anyhow::{Context, Result};
use reproit_protocol::{
    DebugEndpoint, DebugSessionCommand, DebugSessionDescriptor, DebugSessionRequest,
    DebugSessionResponse, DebugSessionState, DiagnosticReceipt, DEBUG_SESSION_VERSION,
};
use serde_json::Value;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const MAX_CONTROL_REQUEST_BYTES: usize = 8 * 1024;

pub(super) enum DebugDecision {
    ReplayTrigger,
    Stop,
}

pub(super) struct DebugControl {
    listener: TcpListener,
    descriptor: DebugSessionDescriptor,
    descriptor_path: PathBuf,
}

impl DebugControl {
    pub(super) async fn start(root: &Path, receipt: &DiagnosticReceipt) -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .context("binding the local debug control endpoint")?;
        let port = listener.local_addr()?.port();
        let descriptor_path = root
            .join(".reproit")
            .join("cells")
            .join(&receipt.run_id)
            .join("debug-session.json");
        let descriptor = DebugSessionDescriptor {
            version: DEBUG_SESSION_VERSION,
            session_id: receipt.run_id.clone(),
            occurrence_id: receipt.occurrence_id.clone(),
            diagnostic_receipt_id: receipt.receipt_id.clone(),
            state: DebugSessionState::WaitingForDebugger,
            control_endpoint: DebugEndpoint {
                host: "127.0.0.1".into(),
                port,
            },
            authorization_token: random_token()?,
            debugger: receipt.debugger,
            debugger_endpoint: receipt.endpoint.clone(),
            source_mappings: receipt.source_mappings.clone(),
            authoritative: false,
        };
        descriptor
            .validate()
            .map_err(|error| anyhow::anyhow!(error))?;
        write_descriptor(&descriptor_path, &descriptor)?;
        Ok(Self {
            listener,
            descriptor,
            descriptor_path,
        })
    }

    pub(super) fn descriptor_path(&self) -> &Path {
        &self.descriptor_path
    }

    pub(super) async fn wait_for_trigger(mut self) -> Result<DebugDecision> {
        if !std::io::stdin().is_terminal() {
            anyhow::bail!("debug execution requires an interactive terminal before the trigger");
        }
        eprintln!(
            "Debug control: {}:{}",
            self.descriptor.control_endpoint.host, self.descriptor.control_endpoint.port
        );
        eprintln!("Session descriptor: {}", self.descriptor_path.display());
        eprintln!("Attach the debugger, then press Enter or send replay-trigger from the IDE.");

        let (terminal_sender, mut terminal_receiver) = tokio::sync::mpsc::channel(1);
        std::thread::spawn(move || {
            let mut confirmation = String::new();
            let result = std::io::stdin().read_line(&mut confirmation);
            let _ = terminal_sender.blocking_send(result);
        });

        loop {
            tokio::select! {
                terminal = terminal_receiver.recv() => {
                    let bytes = terminal.context("terminal confirmation channel closed")?
                        .context("waiting for debugger attachment confirmation")?;
                    if bytes == 0 {
                        anyhow::bail!("debugger attachment was not confirmed before input closed");
                    }
                    self.update_state(DebugSessionState::PausedBeforeTrigger)?;
                    return Ok(DebugDecision::ReplayTrigger);
                }
                accepted = self.listener.accept() => {
                    let (stream, _) = accepted.context("accepting debug control connection")?;
                    if let Some(decision) = self.handle_connection(stream).await? {
                        return Ok(decision);
                    }
                }
            }
        }
    }

    async fn handle_connection(&mut self, mut stream: TcpStream) -> Result<Option<DebugDecision>> {
        let mut bytes = Vec::new();
        (&mut stream)
            .take(MAX_CONTROL_REQUEST_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .await
            .context("reading debug control request")?;
        if bytes.len() > MAX_CONTROL_REQUEST_BYTES {
            anyhow::bail!("debug control request exceeded 8 KiB");
        }
        let request: DebugSessionRequest =
            serde_json::from_slice(&bytes).context("parsing debug control request")?;
        request.validate().map_err(|error| anyhow::anyhow!(error))?;
        if request.authorization_token != self.descriptor.authorization_token {
            self.respond(&mut stream, false, Some("authorization failed"))
                .await?;
            return Ok(None);
        }
        let decision = match request.command {
            DebugSessionCommand::Status => None,
            DebugSessionCommand::DebuggerAttached => {
                self.update_state(DebugSessionState::PausedBeforeTrigger)?;
                None
            }
            DebugSessionCommand::ReplayTrigger => {
                self.update_state(DebugSessionState::Triggering)?;
                Some(DebugDecision::ReplayTrigger)
            }
            DebugSessionCommand::Stop => {
                self.update_state(DebugSessionState::Cleaning)?;
                Some(DebugDecision::Stop)
            }
        };
        self.respond(&mut stream, true, None).await?;
        Ok(decision)
    }

    async fn respond(
        &self,
        stream: &mut TcpStream,
        accepted: bool,
        detail: Option<&str>,
    ) -> Result<()> {
        let response = DebugSessionResponse {
            version: DEBUG_SESSION_VERSION,
            accepted,
            state: self.descriptor.state,
            detail: detail.map(str::to_string),
        };
        stream
            .write_all(&serde_json::to_vec(&response)?)
            .await
            .context("writing debug control response")
    }

    fn update_state(&mut self, state: DebugSessionState) -> Result<()> {
        self.descriptor.state = state;
        write_descriptor(&self.descriptor_path, &self.descriptor)
    }
}

pub(super) async fn open_ide(
    root: &Path,
    control: &DebugControl,
    ide: &str,
    open: bool,
) -> Result<()> {
    if !open || ide == "json" {
        return Ok(());
    }
    let selected = if ide == "auto" {
        if command_available("code") {
            "vscode"
        } else {
            eprintln!("No supported IDE command was detected; use the session descriptor.");
            return Ok(());
        }
    } else {
        ide
    };
    if selected != "vscode" {
        anyhow::bail!("unsupported IDE `{selected}`; use auto, vscode, or json");
    }
    if !command_available("code") {
        anyhow::bail!("VS Code was requested but the `code` command is unavailable");
    }
    let workspace = write_vscode_workspace(root, control.descriptor_path())?;
    let status = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::process::Command::new("code")
            .arg("--reuse-window")
            .arg(&workspace)
            .kill_on_drop(true)
            .status(),
    )
    .await
    .context("VS Code launch timed out")?
    .context("launching VS Code")?;
    if !status.success() {
        anyhow::bail!("VS Code launch command failed");
    }
    Ok(())
}

pub(super) fn finish(
    root: &Path,
    receipt: &DiagnosticReceipt,
    cleanup_verified: bool,
) -> Result<()> {
    let path = root
        .join(".reproit")
        .join("cells")
        .join(&receipt.run_id)
        .join("debug-session.json");
    let mut descriptor: DebugSessionDescriptor = serde_json::from_slice(
        &std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?,
    )?;
    descriptor.state = if cleanup_verified {
        DebugSessionState::Completed
    } else {
        DebugSessionState::Failed
    };
    write_descriptor(&path, &descriptor)
}

fn write_vscode_workspace(root: &Path, descriptor_path: &Path) -> Result<PathBuf> {
    let directory = descriptor_path
        .parent()
        .context("debug session descriptor has no parent")?;
    let launch_path = directory.join("launch.json");
    let launch: Value = serde_json::from_slice(
        &std::fs::read(&launch_path)
            .with_context(|| format!("reading {}", launch_path.display()))?,
    )?;
    let workspace = serde_json::json!({
        "folders": [{ "path": root }],
        "launch": launch,
        "settings": {
            "reproit.debugSession": descriptor_path
        }
    });
    let path = directory.join("reproit.code-workspace");
    write_private(&path, &serde_json::to_vec_pretty(&workspace)?)?;
    Ok(path)
}

fn command_available(command: &str) -> bool {
    std::process::Command::new(command)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn write_descriptor(path: &Path, descriptor: &DebugSessionDescriptor) -> Result<()> {
    descriptor
        .validate()
        .map_err(|error| anyhow::anyhow!(error))?;
    write_private(path, &serde_json::to_vec_pretty(descriptor)?)
}

pub(super) fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let temporary = path.with_extension("json.tmp");
    let mut options = std::fs::OpenOptions::new();
    options.create(true).write(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .with_context(|| format!("creating {}", temporary.display()))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(&temporary, path).with_context(|| format!("installing {}", path.display()))
}

fn random_token() -> Result<String> {
    let mut bytes = [0_u8; 24];
    getrandom::fill(&mut bytes).context("reading operating-system randomness")?;
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write;
        write!(&mut encoded, "{byte:02x}").unwrap();
    }
    Ok(encoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ide_auto_detection_never_selects_an_unknown_adapter() {
        assert!(!command_available("reproit-command-that-does-not-exist"));
    }

    #[tokio::test]
    async fn authenticated_ide_command_releases_the_trigger() {
        let root = std::env::temp_dir().join(format!("reproit-debug-{}", random_token().unwrap()));
        let receipt = DiagnosticReceipt {
            version: reproit_protocol::DIAGNOSTIC_RECEIPT_VERSION,
            receipt_id: "diag_test".into(),
            occurrence_id: "occ_test".into(),
            run_id: "run_test".into(),
            cell_receipt_id: "cell_test".into(),
            debugger: reproit_protocol::DebuggerKind::NodeInspector,
            endpoint: DebugEndpoint {
                host: "127.0.0.1".into(),
                port: 9_229,
            },
            source_mappings: Vec::new(),
            pause_point: "before-trigger".into(),
            perturbations: Vec::new(),
            authoritative: false,
        };
        std::fs::create_dir_all(root.join(".reproit/cells/run_test")).unwrap();
        let mut control = DebugControl::start(&root, &receipt).await.unwrap();
        let endpoint = control.descriptor.control_endpoint.clone();
        let request = DebugSessionRequest {
            version: DEBUG_SESSION_VERSION,
            authorization_token: control.descriptor.authorization_token.clone(),
            command: DebugSessionCommand::ReplayTrigger,
        };
        let client = tokio::spawn(async move {
            let mut stream = TcpStream::connect((endpoint.host.as_str(), endpoint.port))
                .await
                .unwrap();
            stream
                .write_all(&serde_json::to_vec(&request).unwrap())
                .await
                .unwrap();
            stream.shutdown().await.unwrap();
            let mut bytes = Vec::new();
            stream.read_to_end(&mut bytes).await.unwrap();
            serde_json::from_slice::<DebugSessionResponse>(&bytes).unwrap()
        });
        let (stream, _) = control.listener.accept().await.unwrap();
        let decision = control.handle_connection(stream).await.unwrap().unwrap();
        let response = client.await.unwrap();
        assert!(matches!(decision, DebugDecision::ReplayTrigger));
        assert!(response.accepted);
        assert_eq!(response.state, DebugSessionState::Triggering);
        let persisted: DebugSessionDescriptor =
            serde_json::from_slice(&std::fs::read(control.descriptor_path()).unwrap()).unwrap();
        assert_eq!(persisted.state, DebugSessionState::Triggering);
        std::fs::remove_dir_all(root).unwrap();
    }
}
