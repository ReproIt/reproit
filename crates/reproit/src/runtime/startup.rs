//! Native process startup and runtime ownership.

use anyhow::{Context, Result};
use std::ffi::OsString;
use std::process::ExitCode;

// Windows reserves only a small stack for the process entry thread. Debug
// builds of the command-dispatch future need more room; 8 MiB is an explicit,
// finite bound that matches common native main-thread stack budgets without
// hiding accidental unbounded recursion behind an excessive reservation.
const CLI_STACK_BYTES: usize = 8 * 1024 * 1024;

/// Collect process arguments and run the async CLI on its bounded-stack thread.
pub fn run() -> Result<ExitCode> {
    let args: Vec<OsString> = std::env::args_os().collect();
    run_on_startup_thread(move || {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .context("build Tokio runtime")?;
        runtime.block_on(crate::run_from(args))
    })?
}

fn run_on_startup_thread<F, T>(task: F) -> Result<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let thread = std::thread::Builder::new()
        .name("reproit-startup".to_string())
        .stack_size(CLI_STACK_BYTES)
        .spawn(task)
        .context("spawn Reproit startup thread")?;
    match thread.join() {
        Ok(output) => Ok(output),
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_thread_returns_its_result() {
        let code = run_on_startup_thread(|| ExitCode::from(7)).unwrap();
        assert_eq!(code, ExitCode::from(7));

        let error: Result<ExitCode> =
            run_on_startup_thread(|| Err(anyhow::anyhow!("startup error sentinel"))).unwrap();
        assert_eq!(error.unwrap_err().to_string(), "startup error sentinel");
    }

    #[test]
    fn startup_thread_resumes_panics_on_the_caller() {
        let panic = std::panic::catch_unwind(|| {
            let _ = run_on_startup_thread::<_, ()>(|| panic!("startup panic sentinel"));
        });
        assert!(panic.is_err());
    }
}
