use crate::cli::context::{exit_with, Ctx, Exit};
use anyhow::Result;
use serde::Serialize;
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;
use tokio::process::Command;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
enum ContractStatus {
    Violation,
    Satisfied,
    Abstain,
}

fn classify(exit_code: Option<i32>, output: &str, test_name: &str) -> ContractStatus {
    let infrastructure_failure = [
        "Executable doesn't exist",
        "ERR_PNPM",
        "Failed to load url",
        "Transform failed",
        "Failed Suites",
        "No test files found",
    ]
    .iter()
    .any(|marker| output.contains(marker));
    if infrastructure_failure {
        return ContractStatus::Abstain;
    }

    if exit_code == Some(0)
        && (output.contains("Test Files  1 passed") || output.contains("Tests  1 passed"))
    {
        return ContractStatus::Satisfied;
    }
    let assertion_failure = output.contains("Failed Tests 1")
        || output.contains("Tests  1 failed")
        || output.contains("AssertionError:");
    let source_exception = output.contains("Uncaught Exception")
        && output.contains("originated in")
        && output.contains("test file")
        && output.contains("packages/mui-");
    if exit_code == Some(1) && (assertion_failure || source_exception) && output.contains(test_name)
    {
        return ContractStatus::Violation;
    }
    ContractStatus::Abstain
}

pub(super) async fn run_vitest_contract(
    ctx: &Ctx,
    cwd: &Path,
    test_path: &str,
    test_name: &str,
    pnpm_version: &str,
) -> Result<ExitCode> {
    let mut command = Command::new("corepack");
    let pnpm = format!("pnpm@{pnpm_version}");
    command
        .args([pnpm.as_str(), "vitest", "run", test_path, "-t", test_name])
        .current_dir(cwd)
        .env("CI", "true")
        .env("TEST_SCOPE", "node")
        .kill_on_drop(true);
    let result = tokio::time::timeout(Duration::from_secs(180), command.output()).await;
    let (exit_code, stdout, stderr, timed_out) = match result {
        Ok(Ok(output)) => (
            output.status.code(),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
            false,
        ),
        Ok(Err(error)) => (None, String::new(), error.to_string(), false),
        Err(_) => (None, String::new(), "timed out after 180s".into(), true),
    };
    let combined = format!("{stdout}\n{stderr}");
    let status = classify(exit_code, &combined, test_name);
    ctx.emit(&serde_json::json!({
        "command": "authored-vitest-contract",
        "authority": "exact fixing-revision regression test",
        "status": status,
        "testPath": test_path,
        "testName": test_name,
        "pnpmVersion": pnpm_version,
        "exitCode": exit_code,
        "timedOut": timed_out,
        "stdout": stdout,
        "stderr": stderr,
    }));
    Ok(match status {
        ContractStatus::Satisfied => ExitCode::SUCCESS,
        ContractStatus::Violation => exit_with(Exit::Regression),
        ContractStatus::Abstain => exit_with(Exit::Stale),
    })
}

#[cfg(test)]
mod tests {
    use super::{classify, ContractStatus};

    #[test]
    fn classifies_only_exact_assertion_red_green_pairs() {
        assert_eq!(
            classify(
                Some(1),
                "Failed Tests 1\nAssertionError: nope\nexact behavior",
                "exact behavior"
            ),
            ContractStatus::Violation
        );
        assert_eq!(
            classify(Some(0), "Test Files  1 passed (1)", "exact behavior"),
            ContractStatus::Satisfied
        );
        assert_eq!(
            classify(Some(1), "Failed Suites 1\nexact behavior", "exact behavior"),
            ContractStatus::Abstain
        );
        assert_eq!(
            classify(
                Some(1),
                "Uncaught Exception in packages/mui-material/src/X.js; originated in test file; exact behavior",
                "exact behavior"
            ),
            ContractStatus::Violation
        );
    }
}
