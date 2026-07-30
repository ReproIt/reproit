use super::*;
use crate::domain::backend::{BackendReset, BackendResetStep};

/// Run the declared backend reset contract.
///
/// Fuzzing a stateful service without a reset means run N inherits whatever run
/// N-1 left behind: findings stop being independently reproducible and a shrink
/// can chase state that no longer exists. The UI side learned this and has had a
/// first-class `reset:` block for a long time. The backend had only
/// `REPROIT_BACKEND_RESET_URL`, one URL in the environment, which cannot express
/// the several ordered steps a real service needs (clear one table, re-seed a
/// pair, restore a counter) and is invisible to anyone reading reproit.yaml.
///
/// Whether ANY clean-state mechanism exists for this run: the env reset URL,
/// or a process restart of a server reproit booted itself. When reproit owns
/// the target process, a full restart IS a legitimate reset, so stateful
/// confirmation must not dead-end demanding a reset URL nobody can supply.
pub(super) fn reset_capability_available() -> bool {
    std::env::var_os("REPROIT_BACKEND_RESET_URL").is_some()
        || crate::workflows::backend_learn::boot::process_reset_installed()
}

/// Best-effort per step unless `required`. A failed required step aborts: a
/// reset that silently did not happen is worse than none, because the run still
/// presents its findings as reproducible from a clean state.
pub(super) async fn run_reset(ctx: &Ctx, reset: &BackendReset, root: &Path) -> Result<()> {
    if reset.is_empty() {
        return Ok(());
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()?;
    for step in &reset.steps {
        match step {
            BackendResetStep::Command { run, required } => {
                let outcome = crate::runtime::process::run_configured_shell(run, root).await;
                report(ctx, outcome.ok(), *required, run, &outcome.stderr)?;
            }
            BackendResetStep::Http {
                method,
                url,
                body,
                required,
            } => {
                validate_base_url(url).context("backend.reset http step url")?;
                let verb =
                    reqwest::Method::from_bytes(method.as_bytes()).unwrap_or(reqwest::Method::POST);
                let mut request = client.request(verb, url);
                if let Some(body) = body {
                    request = request
                        .header(CONTENT_TYPE, "application/json")
                        .body(body.clone());
                }
                let outcome = request.send().await;
                let ok = matches!(&outcome, Ok(response) if response.status().is_success());
                let detail = match &outcome {
                    Ok(response) => format!("status {}", response.status().as_u16()),
                    Err(error) => error.to_string(),
                };
                report(ctx, ok, *required, &format!("{method} {url}"), &detail)?;
            }
        }
    }
    Ok(())
}

fn report(ctx: &Ctx, ok: bool, required: bool, label: &str, detail: &str) -> Result<()> {
    if ok {
        ctx.say(format!("  reset ok    {label}"));
    } else if required {
        bail!("required backend reset step failed: {label}\n{detail}");
    } else {
        ctx.say(format!("  reset skip  {label}"));
    }
    Ok(())
}

/// Replay-side reset: same contract, no narration. Replay is called from
/// `verify` in a loop and from a bare `reproit <id>`, where a per-step log per
/// finding would bury the verdict.
pub(super) async fn run_reset_quiet(client: &reqwest::Client, reset: &BackendReset) -> Result<()> {
    for step in &reset.steps {
        let (ok, label, detail) = match step {
            BackendResetStep::Command { run, .. } => {
                let outcome =
                    crate::runtime::process::run_configured_shell(run, &std::env::current_dir()?)
                        .await;
                (outcome.ok(), run.clone(), outcome.stderr)
            }
            BackendResetStep::Http {
                method, url, body, ..
            } => {
                validate_base_url(url).context("backend.reset http step url")?;
                let verb =
                    reqwest::Method::from_bytes(method.as_bytes()).unwrap_or(reqwest::Method::POST);
                let mut request = client.request(verb, url);
                if let Some(body) = body {
                    request = request
                        .header(CONTENT_TYPE, "application/json")
                        .body(body.clone());
                }
                let outcome = request.send().await;
                let ok = matches!(&outcome, Ok(response) if response.status().is_success());
                let detail = match &outcome {
                    Ok(response) => format!("status {}", response.status().as_u16()),
                    Err(error) => error.to_string(),
                };
                (ok, format!("{method} {url}"), detail)
            }
        };
        let required = matches!(
            step,
            BackendResetStep::Command { required: true, .. }
                | BackendResetStep::Http { required: true, .. }
        );
        if !ok && required {
            bail!("required backend reset step failed before replay: {label}\n{detail}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> Ctx {
        Ctx {
            quiet: true,
            ..Ctx::default()
        }
    }

    #[tokio::test]
    async fn an_empty_contract_is_a_no_op() {
        let reset = BackendReset::default();
        assert!(run_reset(&context(), &reset, Path::new(".")).await.is_ok());
    }

    #[tokio::test]
    async fn a_failing_optional_step_does_not_abort_the_run() {
        let reset = BackendReset {
            steps: vec![BackendResetStep::Command {
                run: "exit 3".into(),
                required: false,
            }],
        };
        assert!(run_reset(&context(), &reset, Path::new(".")).await.is_ok());
    }

    #[tokio::test]
    async fn a_failing_required_step_fails_the_run_closed() {
        // The whole point: a reset that did not happen must not let the run
        // present its findings as reproducible from a clean state.
        let reset = BackendReset {
            steps: vec![BackendResetStep::Command {
                run: "exit 3".into(),
                required: true,
            }],
        };
        let error = run_reset(&context(), &reset, Path::new("."))
            .await
            .expect_err("a required step must abort");
        assert!(error
            .to_string()
            .contains("required backend reset step failed"));
    }

    #[tokio::test]
    async fn steps_run_in_declared_order() {
        let directory = std::env::temp_dir().join(format!("reproit-reset-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let reset = BackendReset {
            steps: vec![
                BackendResetStep::Command {
                    run: "echo one >> order.txt".into(),
                    required: true,
                },
                BackendResetStep::Command {
                    run: "echo two >> order.txt".into(),
                    required: true,
                },
            ],
        };
        run_reset(&context(), &reset, &directory).await.unwrap();
        let order = std::fs::read_to_string(directory.join("order.txt")).unwrap();
        assert_eq!(order, "one\ntwo\n");
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn an_http_step_defaults_to_post() {
        let step: BackendResetStep =
            serde_yaml::from_str("kind: http\nurl: http://127.0.0.1:1/reset\n").unwrap();
        match step {
            BackendResetStep::Http { method, .. } => assert_eq!(method, "POST"),
            _ => panic!("expected an http step"),
        }
    }
}
