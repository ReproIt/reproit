//! Process helpers. Everything here is best-effort by default: simctl calls
//! routinely "fail" benignly (already booted, already granted), matching the
//! original shell harness's `|| true` style.

use std::path::Path;
use tokio::process::Command;

pub struct RunResult {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl RunResult {
    pub fn ok(&self) -> bool {
        self.code == Some(0)
    }
}

/// Run a command to completion, capturing output. Never errors on non-zero exit.
pub async fn run(cmd: &str, args: &[&str]) -> RunResult {
    match Command::new(cmd).args(args).output().await {
        Ok(out) => RunResult {
            code: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        },
        Err(e) => RunResult {
            code: None,
            stdout: String::new(),
            stderr: e.to_string(),
        },
    }
}

/// Run a config-provided command string via the shell, in `cwd`.
pub async fn run_shell(command: &str, cwd: &Path) -> RunResult {
    match Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(cwd)
        .output()
        .await
    {
        Ok(out) => RunResult {
            code: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        },
        Err(e) => RunResult {
            code: None,
            stdout: String::new(),
            stderr: e.to_string(),
        },
    }
}

/// True if `bin` resolves on PATH.
pub async fn which(bin: &str) -> bool {
    run("sh", &["-c", &format!("command -v {bin}")]).await.ok()
}

/// SIGINT a pid (used to finalize simctl recordings so the moov atom is written).
pub async fn sigint(pid: u32) {
    let _ = run("kill", &["-INT", &pid.to_string()]).await;
}
