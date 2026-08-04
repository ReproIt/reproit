//! Universal command and process capture.

use crate::adapters::execution::{
    compile_local_command_package, LocalCommandObservation, LocalCommandPlan,
};
use crate::interface::cli::context::Ctx;
use anyhow::{Context, Result};
use reproit_protocol::{
    compile_capture_failure, ArtifactPolicy, CaptureAssessmentScope, CaptureEmitter,
    CaptureEmitterKind, CaptureEventKind, CapturedValue, CollectionMethod, ConsentClass,
    EvidenceArtifact, EvidencePolicy, FailureRecord, ObservationAuthority, ObservationKind,
    OperationOutcome, ProcessIdentity, RedactionState, TriggerKind,
};
use reproit_recorder::{EventContext, Recorder, RecorderConfig};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{ExitCode, ExitStatus, Stdio};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt};

mod capabilities;
mod platform;
mod platform_command;
mod process_tree;

pub(crate) use platform_command::{collect_platform, PlatformCollectArgs};

const MAX_TIMEOUT_MS: u64 = 10 * 60 * 1000;
const MAX_STREAM_BYTES: usize = 1024 * 1024;
const MAX_COMMAND_ARGUMENTS: usize = 128;
const MAX_ARGUMENT_BYTES: usize = 16 * 1024;

pub(crate) struct CommandCaptureArgs {
    pub(crate) project: Option<String>,
    pub(crate) component: Option<String>,
    pub(crate) identity: Option<String>,
    pub(crate) timeout_ms: u64,
    pub(crate) include_output: bool,
    pub(crate) local_only: bool,
    pub(crate) command: Vec<OsString>,
}

#[derive(Clone, Copy)]
enum StreamDestination {
    Stdout,
    Stderr,
}

struct StreamCapture {
    bytes: Vec<u8>,
    total_bytes: u64,
    truncated: bool,
}

struct OwnedStagingDirectory {
    path: PathBuf,
    installed: bool,
}

impl OwnedStagingDirectory {
    fn create(path: PathBuf) -> Result<Self> {
        std::fs::create_dir(&path).with_context(|| format!("creating {}", path.display()))?;
        Ok(Self {
            path,
            installed: false,
        })
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn install(mut self, destination: &Path) -> Result<()> {
        std::fs::rename(&self.path, destination)
            .with_context(|| format!("installing {}", destination.display()))?;
        self.installed = true;
        Ok(())
    }
}

impl Drop for OwnedStagingDirectory {
    fn drop(&mut self) {
        if !self.installed {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

enum CommandOutcome {
    Exited(ExitStatus),
    TimedOut,
    Interrupted,
}

struct CaptureResult<'a> {
    batch_id: &'a str,
    occurrence_id: Option<&'a str>,
    directory: &'a Path,
    outcome: &'a CommandOutcome,
    stdout_bytes: u64,
    stderr_bytes: u64,
    cloud_occurrence: Option<&'a str>,
}

pub(crate) async fn run(ctx: &Ctx, args: CommandCaptureArgs) -> Result<ExitCode> {
    validate_args(&args)?;
    let root = std::env::current_dir()?.canonicalize()?;
    let argv = utf8_argv(&args.command)?;
    let executable = Path::new(&argv[0])
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(&argv[0]);
    let selected_cloud_project = super::cloud::cloud_app_id(None).ok();
    let project = token(
        args.project
            .as_deref()
            .or(selected_cloud_project.as_deref())
            .or_else(|| root.file_name().and_then(|name| name.to_str()))
            .unwrap_or("local-project"),
    );
    let component = token(args.component.as_deref().unwrap_or(executable));
    let observed_at = chrono::Utc::now().to_rfc3339();
    let capture_hash = capture_hash(&root, &argv, &observed_at);
    let batch_id = format!("cb_{}", &capture_hash[..16]);
    let session_id = format!("session_{}", &capture_hash[16..32]);
    let staging_path = root
        .join(".reproit")
        .join("captures")
        .join(format!(".{batch_id}.staging"));
    let final_directory = root.join(".reproit").join("captures").join(&batch_id);
    if staging_path.exists() || final_directory.exists() {
        anyhow::bail!("capture {batch_id} already exists");
    }
    std::fs::create_dir_all(
        final_directory
            .parent()
            .context("capture directory has no parent")?,
    )?;
    let staging = OwnedStagingDirectory::create(staging_path)?;

    let argv_bytes = serde_json::to_vec_pretty(&argv)?;
    let argv_artifact = write_private_artifact(
        &staging.path().join("argv.json"),
        &argv_bytes,
        "application/json",
        "argv.json",
    )?;
    let capabilities = capabilities::initial(args.include_output);
    let platform_collection = platform::collect(&component).await;
    let deployment = deployment_from_environment(
        platform_collection.evidence,
        platform_collection.gaps.clone(),
    );
    let mut recorder = Recorder::new(RecorderConfig {
        batch_id: batch_id.clone(),
        project_id: project,
        session_id,
        emitter: CaptureEmitter {
            id: format!("collector-{}", std::process::id()),
            kind: CaptureEmitterKind::HostCollector,
            component,
            runtime: Some(std::env::consts::OS.to_string()),
            parent_id: None,
        },
        deployment,
        observed_at: observed_at.clone(),
        policy: EvidencePolicy {
            consent: ConsentClass::LocalAnalysis,
            retention_class: "local-private".into(),
        },
        capabilities,
        max_events: reproit_protocol::MAX_CAPTURE_EVENTS,
        max_artifacts: reproit_protocol::MAX_CAPTURE_ARTIFACTS,
    })?;
    recorder.add_artifact(argv_artifact.clone())?;
    for gap in platform_collection.gaps {
        recorder.record(
            EventContext::default(),
            CaptureEventKind::Defect {
                defect: reproit_protocol::CaptureDefectKind::Unavailable,
                detail: gap,
                artifact_id: None,
            },
        );
    }

    let mut command = tokio::process::Command::new(&args.command[0]);
    command
        .args(&args.command[1..])
        .current_dir(&root)
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    configure_process_group(&mut command);
    let started = Instant::now();
    let mut child = command
        .spawn()
        .with_context(|| format!("launching {}", argv[0]))?;
    let process_id = child.id().context("captured command has no process id")? as u64;
    let stdout = child
        .stdout
        .take()
        .context("captured stdout pipe missing")?;
    let stderr = child
        .stderr
        .take()
        .context("captured stderr pipe missing")?;
    let json_output = ctx.json;
    let stdout_task = tokio::spawn(drain_stream(
        stdout,
        if json_output {
            StreamDestination::Stderr
        } else {
            StreamDestination::Stdout
        },
    ));
    let stderr_task = tokio::spawn(drain_stream(stderr, StreamDestination::Stderr));

    let process_start = recorder.record(
        event_context(started, process_id),
        CaptureEventKind::ProcessStart {
            process: ProcessIdentity {
                process_id,
                executable: argv[0].clone(),
                parent_process_id: Some(std::process::id() as u64),
                executable_hash: executable_hash(&args.command[0]),
            },
            arguments: Some(CapturedValue::Artifact {
                artifact_id: argv_artifact.id.clone(),
                policy: ArtifactPolicy::LocalAnalysisOnly,
            }),
            working_directory: Some(CapturedValue::Structural {
                shape: serde_json::json!({"scope": "checkout-root"}),
            }),
        },
    );
    let trigger = recorder.record(
        EventContext {
            causal_parent_ids: vec![process_start.clone()],
            ..event_context(started, process_id)
        },
        CaptureEventKind::Trigger {
            trigger: TriggerKind::Command,
            subject: executable.to_string(),
            value: Some(CapturedValue::Artifact {
                artifact_id: argv_artifact.id.clone(),
                policy: ArtifactPolicy::LocalAnalysisOnly,
            }),
        },
    );

    let process_tree = process_tree::observe(process_id, started);

    let outcome = wait_for_command(&mut child, process_id, args.timeout_ms).await?;
    let descendants = process_tree.finish().await;
    terminate_remaining_process_group(process_id);
    let stdout = join_stream_task(stdout_task, "stdout").await?;
    let stderr = join_stream_task(stderr_task, "stderr").await?;
    let mut observation_artifacts = Vec::new();
    if args.include_output {
        let mut retained_artifact_ids = std::collections::BTreeSet::new();
        for (name, capture) in [("stdout.bin", &stdout), ("stderr.bin", &stderr)] {
            let artifact = write_private_artifact(
                &staging.path().join(name),
                &capture.bytes,
                "application/octet-stream",
                name,
            )?;
            if retained_artifact_ids.insert(artifact.id.clone()) {
                observation_artifacts.push(artifact.id.clone());
                recorder.add_artifact(artifact)?;
            }
        }
    }
    if stdout.truncated || stderr.truncated {
        recorder.record(
            event_context(started, process_id),
            CaptureEventKind::Defect {
                defect: reproit_protocol::CaptureDefectKind::Truncated,
                detail: format!(
                    "stdout retained {}/{} bytes; stderr retained {}/{} bytes",
                    stdout.bytes.len(),
                    stdout.total_bytes,
                    stderr.bytes.len(),
                    stderr.total_bytes
                ),
                artifact_id: None,
            },
        );
    }
    process_tree::record_observation(
        &mut recorder,
        descendants,
        &process_start,
        started,
        process_id,
    );

    let (exit_code, signal) = exit_details(&outcome);
    let process_exit = recorder.record(
        EventContext {
            causal_parent_ids: vec![trigger.clone()],
            ..event_context(started, process_id)
        },
        CaptureEventKind::ProcessExit {
            process_id,
            exit_code,
            signal: signal.clone().or_else(|| match outcome {
                CommandOutcome::TimedOut => Some("timeout".into()),
                CommandOutcome::Interrupted => Some("collector-interrupt".into()),
                CommandOutcome::Exited(_) => None,
            }),
        },
    );
    let failure = failure_record(
        &outcome,
        executable,
        exit_code,
        signal.as_deref(),
        observation_artifacts,
        args.identity.as_deref(),
    );
    let mut identity = None;
    if let Some(failure) = failure {
        identity = failure.signature.clone();
        recorder.record(
            EventContext {
                causal_parent_ids: vec![process_exit],
                ..event_context(started, process_id)
            },
            CaptureEventKind::Observation { failure },
        );
    } else {
        recorder.record(
            EventContext {
                causal_parent_ids: vec![process_exit],
                ..event_context(started, process_id)
            },
            CaptureEventKind::OperationEnd {
                name: executable.to_string(),
                outcome: OperationOutcome::Succeeded,
            },
        );
    }

    let batch = recorder.finish()?;
    write_json(&staging.path().join("capture.json"), &batch)?;
    staging.install(&final_directory)?;

    let received_at = chrono::Utc::now().to_rfc3339();
    let compilation = compile_capture_failure(
        &batch,
        &received_at,
        CaptureAssessmentScope::SourceEnvironment,
    )
    .map_err(|error| anyhow::anyhow!("capture compilation failed: {error}"))?;
    let mut occurrence_id = None;
    if let Some(compilation) = compilation {
        let identity = identity.context("failure capture omitted its identity")?;
        let local_observation = match outcome {
            CommandOutcome::Exited(status) => {
                if let Some(code) = status.code() {
                    LocalCommandObservation::ExitCode(code)
                } else {
                    #[cfg(unix)]
                    {
                        use std::os::unix::process::ExitStatusExt;
                        LocalCommandObservation::Signal(
                            status.signal().context("signal exit omitted its signal")?,
                        )
                    }
                    #[cfg(not(unix))]
                    {
                        anyhow::bail!("captured process omitted an exit code");
                    }
                }
            }
            CommandOutcome::TimedOut => LocalCommandObservation::Timeout,
            CommandOutcome::Interrupted => {
                emit_result(
                    ctx,
                    CaptureResult {
                        batch_id: &batch_id,
                        occurrence_id: None,
                        directory: &final_directory,
                        outcome: &outcome,
                        stdout_bytes: stdout.total_bytes,
                        stderr_bytes: stderr.total_bytes,
                        cloud_occurrence: None,
                    },
                );
                return Ok(ExitCode::from(130));
            }
        };
        let compiled = compile_local_command_package(LocalCommandPlan {
            root: &root,
            occurrence: compilation.occurrence,
            assessment: compilation.assessment,
            argv,
            working_directory: &root,
            timeout_ms: args.timeout_ms,
            identity: &identity,
            observation: local_observation,
        })?;
        let package = &compiled.package;
        let occurrence_directory = persist_occurrence(&root, &batch, package)?;
        compiled.install_provider(&root)?;
        occurrence_id = Some(package.occurrence.occurrence_id.clone());
        write_private_text(
            &occurrence_directory.join("capture-directory"),
            &final_directory.display().to_string(),
        )?;
    }

    let cloud_occurrence = if args.local_only {
        None
    } else {
        upload_if_configured(&batch).await?
    };
    if let (Some(local), Some(cloud)) = (occurrence_id.as_deref(), cloud_occurrence.as_deref()) {
        if local != cloud {
            anyhow::bail!(
                "Cloud returned occurrence `{cloud}` for local occurrence `{local}`; \
                 capture identity is inconsistent"
            );
        }
    }
    emit_result(
        ctx,
        CaptureResult {
            batch_id: &batch_id,
            occurrence_id: occurrence_id.as_deref(),
            directory: &final_directory,
            outcome: &outcome,
            stdout_bytes: stdout.total_bytes,
            stderr_bytes: stderr.total_bytes,
            cloud_occurrence: cloud_occurrence.as_deref(),
        },
    );
    Ok(capture_exit_code(&outcome))
}

fn capture_exit_code(outcome: &CommandOutcome) -> ExitCode {
    match outcome {
        CommandOutcome::Exited(status) if status.success() => ExitCode::SUCCESS,
        CommandOutcome::Exited(status) => status
            .code()
            .and_then(|code| u8::try_from(code).ok())
            .map(ExitCode::from)
            .unwrap_or(ExitCode::FAILURE),
        CommandOutcome::TimedOut => ExitCode::from(124),
        CommandOutcome::Interrupted => ExitCode::from(130),
    }
}

fn validate_args(args: &CommandCaptureArgs) -> Result<()> {
    if args.command.is_empty() || args.command.len() > MAX_COMMAND_ARGUMENTS {
        anyhow::bail!("capture command must contain 1..={MAX_COMMAND_ARGUMENTS} arguments");
    }
    if args.timeout_ms == 0 || args.timeout_ms > MAX_TIMEOUT_MS {
        anyhow::bail!("--timeout-ms must be within 1..={MAX_TIMEOUT_MS}");
    }
    for argument in &args.command {
        if argument.as_encoded_bytes().len() > MAX_ARGUMENT_BYTES {
            anyhow::bail!("capture command argument exceeds {MAX_ARGUMENT_BYTES} bytes");
        }
    }
    if let Some(identity) = &args.identity {
        if identity.is_empty()
            || identity.len() > MAX_ARGUMENT_BYTES
            || identity
                .chars()
                .any(|character| matches!(character, '\0' | '\r' | '\n'))
        {
            anyhow::bail!("--identity must be bounded non-empty single-line text");
        }
    }
    Ok(())
}

fn utf8_argv(command: &[OsString]) -> Result<Vec<String>> {
    command
        .iter()
        .map(|argument| {
            argument
                .to_str()
                .map(str::to_string)
                .context("capture currently requires UTF-8 command arguments")
        })
        .collect()
}

fn token(value: &str) -> String {
    let mut token = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '-'
            }
        })
        .take(128)
        .collect::<String>();
    while token.starts_with('-') {
        token.remove(0);
    }
    if token.is_empty() {
        "unknown".into()
    } else {
        token
    }
}

fn capture_hash(root: &Path, argv: &[String], observed_at: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(root.as_os_str().as_encoded_bytes());
    digest.update([0]);
    for argument in argv {
        digest.update(argument.as_bytes());
        digest.update([0]);
    }
    digest.update(observed_at.as_bytes());
    hex_digest(digest.finalize())
}

fn deployment_from_environment(
    platforms: Vec<reproit_protocol::PlatformEvidence>,
    platform_gaps: Vec<String>,
) -> Option<reproit_protocol::DeploymentIdentity> {
    let version = std::env::var("REPROIT_BUILD_VERSION").ok();
    let commit = std::env::var("REPROIT_BUILD_COMMIT").ok();
    (version.is_some() || commit.is_some() || !platforms.is_empty() || !platform_gaps.is_empty())
        .then_some(reproit_protocol::DeploymentIdentity {
            version,
            commit,
            platforms,
            platform_gaps,
        })
}

fn event_context(started: Instant, process_id: u64) -> EventContext {
    EventContext {
        monotonic_ns: started.elapsed().as_nanos().min(u64::MAX as u128) as u64,
        wall_time: Some(chrono::Utc::now().to_rfc3339()),
        process_id: Some(process_id),
        ..EventContext::default()
    }
}

#[cfg(unix)]
fn configure_process_group(command: &mut tokio::process::Command) {
    use std::os::unix::process::CommandExt;
    command.as_std_mut().process_group(0);
}

#[cfg(not(unix))]
fn configure_process_group(_command: &mut tokio::process::Command) {}

async fn wait_for_command(
    child: &mut tokio::process::Child,
    process_id: u64,
    timeout_ms: u64,
) -> Result<CommandOutcome> {
    tokio::select! {
        status = child.wait() => Ok(CommandOutcome::Exited(status?)),
        _ = tokio::time::sleep(Duration::from_millis(timeout_ms)) => {
            terminate_process_tree(child, process_id).await?;
            Ok(CommandOutcome::TimedOut)
        }
        interrupt = tokio::signal::ctrl_c() => {
            interrupt.context("installing interrupt listener")?;
            terminate_process_tree(child, process_id).await?;
            Ok(CommandOutcome::Interrupted)
        }
    }
}

#[cfg(unix)]
async fn terminate_process_tree(child: &mut tokio::process::Child, process_id: u64) -> Result<()> {
    let process_group = -(process_id as i32);
    // SAFETY: process_id came from the child we placed in its own process
    // group. A negative pid targets only that owned group.
    unsafe {
        libc::kill(process_group, libc::SIGTERM);
    }
    if tokio::time::timeout(Duration::from_secs(2), child.wait())
        .await
        .is_err()
    {
        // SAFETY: the same owned process group is still the exact target.
        unsafe {
            libc::kill(process_group, libc::SIGKILL);
        }
        tokio::time::timeout(Duration::from_secs(2), child.wait())
            .await
            .context("captured process did not exit after SIGKILL")??;
    }
    Ok(())
}

#[cfg(not(unix))]
async fn terminate_process_tree(child: &mut tokio::process::Child, _process_id: u64) -> Result<()> {
    child.kill().await.context("terminating captured process")?;
    child.wait().await?;
    Ok(())
}

#[cfg(unix)]
fn terminate_remaining_process_group(process_id: u64) {
    // SAFETY: the command was placed in a dedicated process group whose id is
    // the root child pid. The root may already be gone; ESRCH is harmless.
    unsafe {
        libc::kill(-(process_id as i32), libc::SIGTERM);
    }
}

#[cfg(not(unix))]
fn terminate_remaining_process_group(_process_id: u64) {}

async fn join_stream_task(
    mut task: tokio::task::JoinHandle<Result<StreamCapture>>,
    name: &str,
) -> Result<StreamCapture> {
    match tokio::time::timeout(Duration::from_secs(2), &mut task).await {
        Ok(result) => result.with_context(|| format!("{name} capture task failed"))?,
        Err(_) => {
            task.abort();
            let _ = task.await;
            Ok(StreamCapture {
                bytes: vec![],
                total_bytes: 0,
                truncated: true,
            })
        }
    }
}

async fn drain_stream<R>(mut reader: R, destination: StreamDestination) -> Result<StreamCapture>
where
    R: AsyncRead + Unpin,
{
    let mut bytes = Vec::new();
    let mut total_bytes = 0u64;
    let mut buffer = [0u8; 8192];
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        total_bytes = total_bytes.saturating_add(read as u64);
        write_stream(destination, &buffer[..read])?;
        let remaining = MAX_STREAM_BYTES.saturating_sub(bytes.len());
        bytes.extend_from_slice(&buffer[..read.min(remaining)]);
    }
    Ok(StreamCapture {
        truncated: total_bytes > bytes.len() as u64,
        bytes,
        total_bytes,
    })
}

fn write_stream(destination: StreamDestination, bytes: &[u8]) -> Result<()> {
    match destination {
        StreamDestination::Stdout => {
            let mut output = std::io::stdout().lock();
            output.write_all(bytes)?;
            output.flush()?;
        }
        StreamDestination::Stderr => {
            let mut output = std::io::stderr().lock();
            output.write_all(bytes)?;
            output.flush()?;
        }
    }
    Ok(())
}

fn exit_details(outcome: &CommandOutcome) -> (Option<i32>, Option<String>) {
    let CommandOutcome::Exited(status) = outcome else {
        return (None, None);
    };
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        (
            status.code(),
            status.signal().map(|signal| format!("signal-{signal}")),
        )
    }
    #[cfg(not(unix))]
    {
        (status.code(), None)
    }
}

fn failure_record(
    outcome: &CommandOutcome,
    executable: &str,
    exit_code: Option<i32>,
    signal: Option<&str>,
    artifact_ids: Vec<String>,
    asserted_identity: Option<&str>,
) -> Option<FailureRecord> {
    let (observation, summary, derived_signature) = match outcome {
        CommandOutcome::Exited(status) if status.success() => return None,
        CommandOutcome::Exited(_) => {
            let identity = exit_code
                .map(|code| format!("process-exit:{code}"))
                .or_else(|| signal.map(|signal| format!("process-{signal}")))
                .unwrap_or_else(|| "process-exit:unknown".into());
            (
                ObservationKind::Exit,
                format!("{executable} terminated with {identity}"),
                identity,
            )
        }
        CommandOutcome::TimedOut => (
            ObservationKind::Hang,
            format!("{executable} exceeded the capture timeout"),
            "process-timeout".into(),
        ),
        CommandOutcome::Interrupted => return None,
    };
    let signature = asserted_identity.unwrap_or(&derived_signature).to_string();
    let summary = if asserted_identity.is_some() {
        format!("{summary}; trusted verifier asserted {signature}")
    } else {
        summary
    };
    Some(FailureRecord {
        observation,
        authority: ObservationAuthority::RuntimeDiagnosis,
        summary,
        signature: Some(signature),
        observation_point: Some(format!("{executable}/process-exit")),
        artifact_ids,
    })
}

fn executable_hash(executable: &OsString) -> Option<String> {
    let path = Path::new(executable);
    path.is_file()
        .then(|| std::fs::read(path).ok())
        .flatten()
        .map(|bytes| format!("sha256:{}", hex_digest(Sha256::digest(bytes))))
}

fn write_private_artifact(
    path: &Path,
    bytes: &[u8],
    media_type: &str,
    name: &str,
) -> Result<EvidenceArtifact> {
    write_private_bytes(path, bytes)?;
    Ok(EvidenceArtifact {
        id: format!("sha256:{}", hex_digest(Sha256::digest(bytes))),
        kind: if name == "argv.json" {
            reproit_protocol::EvidenceArtifactKind::InteractionTrace
        } else {
            reproit_protocol::EvidenceArtifactKind::TextLog
        },
        media_type: media_type.into(),
        bytes: bytes.len() as u64,
        policy: ArtifactPolicy::LocalAnalysisOnly,
        redaction: RedactionState::UnredactedRestricted,
        collection: CollectionMethod::FlightRecorder,
        encryption_key_id: None,
        name: Some(name.into()),
    })
}

fn write_private_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("creating {}", path.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("writing {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("syncing {}", path.display()))
}

fn write_private_text(path: &Path, value: &str) -> Result<()> {
    write_private_bytes(path, value.as_bytes())
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    write_private_bytes(path, &serde_json::to_vec_pretty(value)?)
}

fn persist_occurrence(
    root: &Path,
    batch: &reproit_protocol::CaptureBatch,
    package: &reproit_protocol::ReproductionPackage,
) -> Result<PathBuf> {
    let parent = root.join(".reproit").join("occurrences");
    std::fs::create_dir_all(&parent)?;
    let final_directory = parent.join(&package.occurrence.occurrence_id);
    if final_directory.exists() {
        let existing = std::fs::read(final_directory.join("package.json"))?;
        if existing == serde_json::to_vec_pretty(package)? {
            return Ok(final_directory);
        }
        anyhow::bail!(
            "occurrence {} already exists with different contents",
            package.occurrence.occurrence_id
        );
    }
    let staging_path = parent.join(format!(
        ".{}.{}.staging",
        package.occurrence.occurrence_id,
        std::process::id()
    ));
    let staging = OwnedStagingDirectory::create(staging_path)?;
    write_json(&staging.path().join("capture.json"), batch)?;
    write_json(&staging.path().join("package.json"), package)?;
    staging.install(&final_directory)?;
    Ok(final_directory)
}

async fn upload_if_configured(batch: &reproit_protocol::CaptureBatch) -> Result<Option<String>> {
    let (cloud, key) = super::cloud::cloud_creds(None, None);
    let Some(key) = key else {
        return Ok(None);
    };
    let cloud = cloud.unwrap_or_else(|| "https://cloud.reproit.com".into());
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .context("creating Cloud capture client")?;
    let response = client
        .post(format!(
            "{}/v1/capture-batches",
            cloud.trim_end_matches('/')
        ))
        .bearer_auth(key)
        .json(batch)
        .send()
        .await
        .with_context(|| {
            format!(
                "uploading capture {} to Cloud; local evidence remains available",
                batch.batch_id
            )
        })?;
    let status = response.status();
    let value = bounded_response_json(response).await?;
    if !status.is_success() {
        let detail = value
            .get("error")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("Cloud rejected the capture batch");
        anyhow::bail!(
            "Cloud capture upload failed ({status}): {detail}; local evidence remains available"
        );
    }
    Ok(value
        .get("occurrenceId")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string))
}

async fn bounded_response_json(mut response: reqwest::Response) -> Result<serde_json::Value> {
    const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        anyhow::bail!("Cloud capture response exceeded 1 MiB");
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            anyhow::bail!("Cloud capture response exceeded 1 MiB");
        }
        bytes.extend_from_slice(&chunk);
    }
    if bytes.is_empty() {
        return Ok(serde_json::Value::Null);
    }
    serde_json::from_slice(&bytes).context("Cloud capture response was not valid JSON")
}

fn emit_result(ctx: &Ctx, result: CaptureResult<'_>) {
    let (exit_code, signal) = exit_details(result.outcome);
    let timed_out = matches!(result.outcome, CommandOutcome::TimedOut);
    let interrupted = matches!(result.outcome, CommandOutcome::Interrupted);
    ctx.emit(&serde_json::json!({
        "command": "capture",
        "batchId": result.batch_id,
        "occurrenceId": result.occurrence_id,
        "cloudOccurrenceId": result.cloud_occurrence,
        "directory": result.directory,
        "process": {
            "exitCode": exit_code,
            "signal": signal,
            "timedOut": timed_out,
            "interrupted": interrupted,
        },
        "streams": {
            "stdoutBytes": result.stdout_bytes,
            "stderrBytes": result.stderr_bytes,
        }
    }));
    ctx.say(format!("Captured command as {}", result.batch_id));
    ctx.say(format!("  evidence:   {}", result.directory.display()));
    if let Some(occurrence_id) = result.occurrence_id {
        ctx.say(format!("  occurrence: {occurrence_id}"));
        ctx.say(format!("  reproduce:  reproit {occurrence_id}"));
    } else if interrupted {
        ctx.say("  status:     interrupted");
    } else {
        ctx.say("  status:     clean, no failure occurrence created");
    }
    if result.cloud_occurrence.is_some() {
        ctx.say("  cloud:      uploaded");
    }
}

fn hex_digest(bytes: impl AsRef<[u8]>) -> String {
    let bytes = bytes.as_ref();
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write;
        write!(&mut encoded, "{byte:02x}").unwrap();
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn capture_preserves_a_bounded_child_exit_code() {
        let status = std::process::Command::new("/bin/sh")
            .args(["-c", "exit 17"])
            .status()
            .unwrap();
        assert_eq!(
            capture_exit_code(&CommandOutcome::Exited(status)),
            ExitCode::from(17)
        );
        assert_eq!(
            capture_exit_code(&CommandOutcome::TimedOut),
            ExitCode::from(124)
        );
    }

    #[test]
    fn token_normalization_is_bounded_and_nonempty() {
        assert_eq!(token("Invoice Importer"), "Invoice-Importer");
        assert_eq!(token("***"), "unknown");
        assert!(token(&"x".repeat(200)).len() <= 128);
    }

    #[test]
    fn command_bounds_reject_empty_and_excessive_input() {
        let args = CommandCaptureArgs {
            project: None,
            component: None,
            identity: None,
            timeout_ms: 1,
            include_output: false,
            local_only: true,
            command: vec![],
        };
        assert!(validate_args(&args).is_err());
        let args = CommandCaptureArgs {
            timeout_ms: MAX_TIMEOUT_MS + 1,
            command: vec!["true".into()],
            ..args
        };
        assert!(validate_args(&args).is_err());
    }

    #[test]
    fn identical_output_artifacts_are_retained_once() {
        let mut retained = std::collections::BTreeSet::new();
        let digest = format!("sha256:{}", "0".repeat(64));
        assert!(retained.insert(digest.clone()));
        assert!(!retained.insert(digest));
        assert_eq!(retained.len(), 1);
    }
}
