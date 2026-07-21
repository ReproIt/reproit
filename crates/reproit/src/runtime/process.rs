//! Process helpers. Everything here is best-effort by default: simctl calls
//! routinely "fail" benignly (already booted, already granted), matching the
//! original shell harness's `|| true` style.

use std::path::{Path, PathBuf};
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

/// Run an explicitly configured command string via the shell, in `cwd`.
///
/// This is reserved for configuration fields whose contract is a shell
/// program, such as reset steps and marker hooks. Evidence, paths, executable
/// names, and other external values must use [`run`] with separate arguments.
pub async fn run_configured_shell(command: &str, cwd: &Path) -> RunResult {
    let mut c = Command::new("sh");
    c.arg("-c").arg(command).current_dir(cwd);
    run_command(c, DEFAULT_TIMEOUT).await
}

/// True if `bin` resolves on PATH.
pub async fn which(bin: &str) -> bool {
    executable_path(bin).is_some()
}

pub(crate) fn executable_path(bin: &str) -> Option<PathBuf> {
    let requested = Path::new(bin);
    if requested.components().count() > 1 {
        return is_executable(requested).then(|| requested.to_path_buf());
    }

    let path = std::env::var_os("PATH")?;
    for directory in std::env::split_paths(&path) {
        for candidate in executable_candidates(&directory, bin) {
            if is_executable(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(not(windows))]
fn executable_candidates(directory: &Path, bin: &str) -> Vec<PathBuf> {
    vec![directory.join(bin)]
}

#[cfg(windows)]
fn executable_candidates(directory: &Path, bin: &str) -> Vec<PathBuf> {
    let requested = Path::new(bin);
    if requested.extension().is_some() {
        return vec![directory.join(requested)];
    }
    let extensions = std::env::var_os("PATHEXT").unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".into());
    extensions
        .to_string_lossy()
        .split(';')
        .filter(|extension| !extension.is_empty())
        .map(|extension| directory.join(format!("{bin}{extension}")))
        .collect()
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(windows)]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// SIGINT a pid (used to finalize simctl recordings so the moov atom is
/// written).
pub async fn sigint(pid: u32) {
    let _ = run("kill", &["-INT", &pid.to_string()]).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn which_does_not_interpret_shell_syntax() {
        assert!(!which("missing-reproit-bin || true").await);
    }

    #[tokio::test]
    async fn which_finds_a_known_executable() {
        #[cfg(unix)]
        assert!(which("sh").await);
        #[cfg(windows)]
        assert!(which("cmd").await);
    }
}
