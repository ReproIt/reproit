//! Process helpers. Everything here is best-effort by default: simctl calls
//! routinely "fail" benignly (already booted, already granted), matching the
//! original shell harness's `|| true` style.

use std::path::Path;
use std::time::Duration;
use tokio::process::Command;

/// Wall-clock ceiling for a single process helper call. These are short
/// management commands (simctl, which, kill, osascript) and config reset hooks;
/// a wedged one (a CoreSimulator boot that never returns, a hook waiting on
/// stdin) would otherwise hang the run forever. Generous so a legitimate slow
/// command is not killed, but bounded so a wedge can't deadlock a CI step.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);

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

fn out_result(out: std::process::Output) -> RunResult {
    RunResult {
        code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// Drive a Command to completion under a timeout. `kill_on_drop` means a
/// timeout (or a cancelled run) drops the output future and kills the child,
/// rather than leaving an orphaned process and a hung await.
async fn run_command(mut cmd: Command, timeout: Duration) -> RunResult {
    cmd.kill_on_drop(true);
    match tokio::time::timeout(timeout, cmd.output()).await {
        Ok(Ok(out)) => out_result(out),
        Ok(Err(e)) => RunResult {
            code: None,
            stdout: String::new(),
            stderr: e.to_string(),
        },
        Err(_) => RunResult {
            code: None,
            stdout: String::new(),
            stderr: format!("timed out after {}s", timeout.as_secs()),
        },
    }
}

/// Run a command to completion, capturing output. Never errors on non-zero
/// exit; bounded by `DEFAULT_TIMEOUT`.
pub async fn run(cmd: &str, args: &[&str]) -> RunResult {
    run_timeout(cmd, args, DEFAULT_TIMEOUT).await
}

/// `run` with an explicit timeout (for a call that legitimately needs more, or
/// less, than the default).
pub async fn run_timeout(cmd: &str, args: &[&str], timeout: Duration) -> RunResult {
    let mut c = Command::new(cmd);
    c.args(args);
    run_command(c, timeout).await
}

/// Run a config-provided command string via the shell, in `cwd`. Bounded by
/// `DEFAULT_TIMEOUT` so a wedged reset hook can't hang the run.
pub async fn run_shell(command: &str, cwd: &Path) -> RunResult {
    let mut c = Command::new("sh");
    c.arg("-c").arg(command).current_dir(cwd);
    run_command(c, DEFAULT_TIMEOUT).await
}

/// True if `bin` resolves on PATH.
pub async fn which(bin: &str) -> bool {
    run("sh", &["-c", &format!("command -v {bin}")]).await.ok()
}

/// SIGINT a pid (used to finalize simctl recordings so the moov atom is
/// written).
pub async fn sigint(pid: u32) {
    let _ = run("kill", &["-INT", &pid.to_string()]).await;
}
