use super::*;

pub(super) async fn execute_provider(
    root: &Path,
    provider_id: &str,
    provider: &CommandProvider,
    expected_identity: &str,
) -> Result<(ProviderRun, ProviderVerdict)> {
    let result = run_command(
        root,
        &provider.argv,
        &provider.environment,
        provider.working_directory.as_deref(),
        provider.timeout_ms,
    )
    .await?;
    let observation_matched = provider.observation.as_ref().is_some_and(|observation| {
        observation.identity == expected_identity && observation.matches(&result)
    });
    let verdict = if observation_matched {
        ProviderVerdict::Reproduced
    } else if provider.observation.is_some() {
        if result.timed_out {
            ProviderVerdict::InfrastructureFailed
        } else if result
            .exit_code
            .is_some_and(|code| provider.clean_exit_codes.contains(&code))
        {
            ProviderVerdict::NotReproduced
        } else {
            ProviderVerdict::DifferentFailure
        }
    } else if !result.timed_out
        && result
            .exit_code
            .is_some_and(|code| provider.clean_exit_codes.contains(&code))
    {
        ProviderVerdict::SetupPassed
    } else {
        ProviderVerdict::InfrastructureFailed
    };
    Ok((
        ProviderRun {
            provider_id: provider_id.to_string(),
            phase: provider.phase,
            exit_code: result.exit_code,
            signal: result.signal,
            timed_out: result.timed_out,
            output_truncated: result.output_truncated,
            observation_matched,
            error: None,
        },
        verdict,
    ))
}

impl CommandObservation {
    fn matches(&self, result: &CommandResult) -> bool {
        match &self.matcher {
            ObservationMatcher::ExitCode { code } => result.exit_code == Some(*code),
            ObservationMatcher::Signal { number } => result.signal == Some(*number),
            ObservationMatcher::StdoutContains { value } => {
                String::from_utf8_lossy(&result.stdout).contains(value)
            }
            ObservationMatcher::StderrContains { value } => {
                String::from_utf8_lossy(&result.stderr).contains(value)
            }
            ObservationMatcher::Timeout => result.timed_out,
        }
    }
}

pub(super) async fn run_cleanup(
    root: &Path,
    providers: &[(String, &CommandProvider)],
    provider_runs: &mut Vec<ProviderRun>,
) {
    for (provider_id, provider) in providers.iter().rev() {
        let Some(cleanup) = &provider.cleanup else {
            continue;
        };
        let result = run_command(
            root,
            &cleanup.argv,
            &cleanup.environment,
            cleanup.working_directory.as_deref(),
            cleanup.timeout_ms,
        )
        .await;
        if let Ok(result) = result {
            provider_runs.push(ProviderRun {
                provider_id: format!("{provider_id}:cleanup"),
                phase: ExecutionPhase::Cleanup,
                exit_code: result.exit_code,
                signal: result.signal,
                timed_out: result.timed_out,
                output_truncated: result.output_truncated,
                observation_matched: false,
                error: None,
            });
        }
    }
}

pub(super) async fn run_command(
    root: &Path,
    argv: &[String],
    environment: &BTreeMap<String, String>,
    working_directory: Option<&Path>,
    timeout_ms: u64,
) -> Result<CommandResult> {
    let directory = resolve_working_directory(root, working_directory)?;
    let mut command = tokio::process::Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .current_dir(directory)
        .envs(environment)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .with_context(|| format!("launching trusted provider executable `{}`", argv[0]))?;
    let stdout = child.stdout.take().context("capturing provider stdout")?;
    let stderr = child.stderr.take().context("capturing provider stderr")?;
    let stdout_task = tokio::spawn(read_bounded(stdout));
    let stderr_task = tokio::spawn(read_bounded(stderr));
    let status = tokio::time::timeout(Duration::from_millis(timeout_ms), child.wait()).await;
    let (exit_code, signal, timed_out) = match status {
        Ok(status) => {
            let status = status.context("waiting for trusted provider")?;
            (status.code(), exit_signal(&status), false)
        }
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            (None, None, true)
        }
    };
    let (stdout, stdout_truncated) = stdout_task.await.context("joining stdout collector")??;
    let (stderr, stderr_truncated) = stderr_task.await.context("joining stderr collector")??;
    Ok(CommandResult {
        exit_code,
        signal,
        timed_out,
        stdout,
        stderr,
        output_truncated: stdout_truncated || stderr_truncated,
    })
}

#[cfg(unix)]
fn exit_signal(status: &std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    status.signal()
}

#[cfg(not(unix))]
fn exit_signal(_status: &std::process::ExitStatus) -> Option<i32> {
    None
}

async fn read_bounded(mut reader: impl AsyncRead + Unpin) -> Result<(Vec<u8>, bool)> {
    let mut retained = Vec::new();
    let mut buffer = [0u8; 8192];
    let mut truncated = false;
    loop {
        let count = reader.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        let remaining = MAX_OUTPUT_BYTES.saturating_sub(retained.len());
        let keep = count.min(remaining);
        retained.extend_from_slice(&buffer[..keep]);
        truncated |= keep < count;
    }
    Ok((retained, truncated))
}
