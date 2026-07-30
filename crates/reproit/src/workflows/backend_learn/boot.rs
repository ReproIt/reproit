//! Zero-flag live target resolution for `reproit init` and `reproit find`.
//!
//! Two sources, in order. A server already listening on a conventional dev
//! port is trusted ONLY after one derived route answers with something other
//! than 404: a port belonging to a different project must never enrich this
//! one. When nothing matching is running, the package.json `start`/`dev`
//! script is booted on a private port, awaited within a hard readiness
//! budget, and torn down on every exit path (the group kill lives in Drop,
//! so an error or early return cannot leak the server).
//!
//! A server this module booted itself can also serve as a RESET mechanism:
//! a full process restart returns the service to its declared starting state,
//! which is exactly what stateful finding confirmation needs and exactly what
//! `REPROIT_BACKEND_RESET_URL` cannot provide for a process nobody else owns.
//! The restart capability is installed process-wide (`install_process_reset`)
//! so the backend executor can reach it without threading a handle through
//! every replay signature.

use crate::interface::cli::context::Ctx;
use anyhow::{bail, Context as _, Result};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// Ports dev servers conventionally bind, probed before booting anything.
const CONVENTIONAL_PORTS: [u16; 4] = [3000, 8000, 8080, 5000];
/// One bounded verification request; also bounds each readiness poll.
const VERIFY_TIMEOUT: Duration = Duration::from_millis(600);
/// Hard cap on waiting for a booted script to serve its first route.
const READY_BUDGET: Duration = Duration::from_secs(20);
const READY_POLL: Duration = Duration::from_millis(250);
/// How long each of TERM and KILL may take before shutdown gives up waiting.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(2);

/// A live target resolved without flags. `server` is Some only when the caller
/// booted the process itself and must tear it down after its run.
pub(crate) struct AutoTarget {
    pub(crate) url: String,
    /// Where the target came from, for the probe report line.
    pub(crate) source: String,
    pub(crate) server: Option<BootedServer>,
}

/// The HTTP status a bounded GET observed, or None when nothing answered
/// (connection refused or timed out).
async fn probe_status(client: &reqwest::Client, port: u16, path: &str) -> Option<u16> {
    let url = format!("http://127.0.0.1:{port}{path}");
    client
        .get(&url)
        .send()
        .await
        .ok()
        .map(|response| response.status().as_u16())
}

/// The second match signal: a path this repo does not serve must answer 404.
/// One signal is not enough to trust a running server: DynamoDB-local on its
/// conventional port answers EVERY path with 400, which a bare "not 404"
/// check on the derived route read as a match.
async fn nonce_is_absent(client: &reqwest::Client, port: u16) -> bool {
    let nonce = format!("/reproit-init-verify-{}", std::process::id());
    probe_status(client, port, &nonce).await == Some(404)
}

/// The bounded client every verification probe uses.
fn probe_client() -> Option<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(VERIFY_TIMEOUT)
        .redirect(reqwest::redirect::Policy::limited(3))
        .build()
        .ok()
}

/// Scan the conventional dev ports for a server passing the two-signal match
/// on `path`. Returns the matched port (if any) plus the ports that were
/// silent, the only acceptable fallback addresses for a boot whose start
/// script ignores `PORT`.
async fn scan_conventional_ports(client: &reqwest::Client, path: &str) -> (Option<u16>, Vec<u16>) {
    let mut silent = Vec::new();
    for port in CONVENTIONAL_PORTS {
        match probe_status(client, port, path).await {
            None => silent.push(port),
            // Occupied by something that does not serve this repo's routes:
            // never trusted, and never reused as a boot fallback either.
            Some(404) => {}
            Some(_) if nonce_is_absent(client, port).await => return (Some(port), silent),
            Some(_) => {}
        }
    }
    (None, silent)
}

/// What zero-flag target resolution WOULD do, decided without booting
/// anything: the already-running match (same two-signal trust as
/// `auto_target`) or the package.json script a run would boot. Doctor reports
/// this so "no explicit target" reads as the plan find/check execute, never
/// as a demand for a flag.
pub(crate) enum AutoTargetPlan {
    /// A server already answering the verify path with the two-signal match.
    Running(u16),
    /// The package.json script (`start` or `dev`) a run would boot.
    Boot(String),
}

pub(crate) async fn auto_target_plan(
    root: &Path,
    verify_path: Option<&str>,
) -> Option<AutoTargetPlan> {
    // Same precondition as auto_target: with no parameterless GET route there
    // is nothing to verify a server against and nothing to await a boot on,
    // so no plan may be promised.
    let path = verify_path?;
    if let Some(client) = probe_client() {
        if let (Some(port), _) = scan_conventional_ports(&client, path).await {
            return Some(AutoTargetPlan::Running(port));
        }
    }
    start_script(root).map(|(name, _)| AutoTargetPlan::Boot(name))
}

/// Resolve a live target with zero flags, or None (with the reason said) when
/// nothing can be trusted. `verify_path` is a derived parameterless GET route;
/// with none there is nothing to verify against and nothing worth probing.
pub(crate) async fn auto_target(
    ctx: &Ctx,
    root: &Path,
    verify_path: Option<&str>,
) -> Option<AutoTarget> {
    let path = verify_path?;
    let client = probe_client()?;
    let (matched, silent) = scan_conventional_ports(&client, path).await;
    if let Some(port) = matched {
        ctx.say(format!(
            "  found a server on port {port} answering {path} (it matches the \
             derived routes); assuming it is this service. Override with --target \
             <url>"
        ));
        return Some(AutoTarget {
            url: format!("http://127.0.0.1:{port}"),
            source: "already running, matched a derived route".to_string(),
            server: None,
        });
    }
    let (name, command) = start_script(root)?;
    let port = free_port()?;
    let mut server = match BootedServer::spawn(root, &command, port) {
        Ok(server) => server,
        Err(error) => {
            ctx.say(format!(
                "  could not boot the package.json `{name}` script ({error}); continuing \
                 without live enrichment (pass --target <url> to enrich)"
            ));
            return None;
        }
    };
    ctx.say(format!(
        "  booting the package.json `{name}` script on port {port} to observe responses \
         (torn down when this run completes; override with --target <url>)"
    ));
    let started = Instant::now();
    while started.elapsed() < READY_BUDGET {
        if let Some(status) = server.exited() {
            ctx.say(format!(
                "  the `{name}` script exited ({status}) before serving {path}; continuing \
                 without live enrichment (pass --target <url> to enrich)"
            ));
            return None;
        }
        // The private port was bound by init moments ago, so anything
        // answering on it is the booted server.
        if probe_status(&client, port, path).await.is_some() {
            return Some(AutoTarget {
                url: format!("http://127.0.0.1:{port}"),
                source: format!("booted from the package.json `{name}` script"),
                server: Some(server),
            });
        }
        // The script may ignore PORT. A conventional port is accepted as the
        // boot's address only if it was silent before this boot started and
        // now passes the same two-signal route match as a running server.
        for &fallback in &silent {
            let answered = matches!(
                probe_status(&client, fallback, path).await,
                Some(status) if status != 404
            );
            if answered && nonce_is_absent(&client, fallback).await {
                return Some(AutoTarget {
                    url: format!("http://127.0.0.1:{fallback}"),
                    source: format!("booted from the package.json `{name}` script"),
                    server: Some(server),
                });
            }
        }
        tokio::time::sleep(READY_POLL).await;
    }
    ctx.say(format!(
        "  the `{name}` script did not serve {path} within {}s; continuing without live \
         enrichment (pass --target <url> to enrich)",
        READY_BUDGET.as_secs()
    ));
    None
}

/// The package.json script bare init may boot: `start` first, then `dev`.
fn start_script(root: &Path) -> Option<(String, String)> {
    let raw = std::fs::read_to_string(root.join("package.json")).ok()?;
    let parsed: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let scripts = parsed.get("scripts")?.as_object()?;
    for name in ["start", "dev"] {
        if let Some(command) = scripts.get(name).and_then(serde_json::Value::as_str) {
            if !command.trim().is_empty() {
                return Some((name.to_string(), command.to_string()));
            }
        }
    }
    None
}

/// An OS-assigned free port, so a temporary boot never fights another
/// project's dev server for a conventional one.
fn free_port() -> Option<u16> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).ok()?;
    listener.local_addr().ok().map(|address| address.port())
}

/// A server process this run booted itself. The process group is killed in
/// Drop, which is the teardown guarantee for every exit path, including errors
/// and panics between boot and the explicit shutdown.
pub(crate) struct BootedServer {
    child: tokio::process::Child,
    process_id: u32,
    /// The spawn parameters, kept so a `RestartableServer` can respawn the
    /// exact same process for a clean-state reset.
    root: PathBuf,
    command: String,
    port: u16,
}

impl BootedServer {
    fn spawn(root: &Path, command: &str, port: u16) -> Result<Self> {
        let mut spawned = shell_command(command);
        spawned
            .current_dir(root)
            .env("PORT", port.to_string())
            .env("PATH", path_with_node_bin(root))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        configure_process_group(&mut spawned);
        let child = spawned.spawn().context("spawning the start script")?;
        let process_id = child.id().context("booted server has no pid")?;
        Ok(BootedServer {
            child,
            process_id,
            root: root.to_path_buf(),
            command: command.to_string(),
            port,
        })
    }

    fn exited(&mut self) -> Option<std::process::ExitStatus> {
        self.child.try_wait().ok().flatten()
    }

    /// Orderly teardown: TERM the group, then KILL it, each waited briefly.
    /// Drop remains the backstop if a wait is cut short.
    pub(crate) async fn shutdown(mut self) {
        signal_group(self.process_id, false);
        if tokio::time::timeout(SHUTDOWN_WAIT, self.child.wait())
            .await
            .is_ok()
        {
            return;
        }
        signal_group(self.process_id, true);
        let _ = tokio::time::timeout(SHUTDOWN_WAIT, self.child.wait()).await;
    }
}

/// A booted server plus everything needed to boot it again: the reset
/// mechanism for a service reproit owns. A full process restart returns the
/// service to its declared starting state, so stateful confirmation and shrink
/// replays can run without a `REPROIT_BACKEND_RESET_URL`.
pub(crate) struct RestartableServer {
    /// The port the server actually answers on (it may differ from the spawn
    /// port when a start script ignores `PORT` and binds a conventional one).
    ready_port: u16,
    /// A route the server is known to serve, polled for restart readiness.
    ready_path: String,
    server: Option<BootedServer>,
}

impl RestartableServer {
    pub(crate) fn adopt(server: BootedServer, ready_port: u16, ready_path: String) -> Self {
        RestartableServer {
            ready_port,
            ready_path,
            server: Some(server),
        }
    }

    /// Tear the current process down and boot an identical replacement,
    /// waiting (bounded) until it serves the known route again.
    async fn restart(&mut self) -> Result<()> {
        let Some(previous) = self.server.take() else {
            bail!("the booted server was already shut down");
        };
        let (root, command, port) = (
            previous.root.clone(),
            previous.command.clone(),
            previous.port,
        );
        previous.shutdown().await;
        let mut server =
            BootedServer::spawn(&root, &command, port).context("restarting the booted server")?;
        let client = reqwest::Client::builder().timeout(VERIFY_TIMEOUT).build()?;
        let started = Instant::now();
        while started.elapsed() < READY_BUDGET {
            if let Some(status) = server.exited() {
                bail!("the restarted server exited ({status}) before answering");
            }
            if probe_status(&client, self.ready_port, &self.ready_path)
                .await
                .is_some()
            {
                self.server = Some(server);
                return Ok(());
            }
            tokio::time::sleep(READY_POLL).await;
        }
        // Keep the (possibly slow) replacement so teardown still reaps it.
        self.server = Some(server);
        bail!(
            "the restarted server did not answer {} within {}s",
            self.ready_path,
            READY_BUDGET.as_secs()
        )
    }

    async fn teardown(mut self) {
        if let Some(server) = self.server.take() {
            server.shutdown().await;
        }
    }
}

/// The process-wide restart-reset slot. One CLI invocation runs at most one
/// booted backend target, so a single slot is the honest capacity.
static PROCESS_RESET: OnceLock<tokio::sync::Mutex<Option<RestartableServer>>> = OnceLock::new();
static PROCESS_RESET_INSTALLED: AtomicBool = AtomicBool::new(false);

fn process_reset_slot() -> &'static tokio::sync::Mutex<Option<RestartableServer>> {
    PROCESS_RESET.get_or_init(|| tokio::sync::Mutex::new(None))
}

/// Whether a restart-reset is available to the current run. Sync so gating
/// code inside the backend executor can consult it alongside the env check.
pub(crate) fn process_reset_installed() -> bool {
    PROCESS_RESET_INSTALLED.load(Ordering::SeqCst)
}

pub(crate) async fn install_process_reset(server: RestartableServer) {
    *process_reset_slot().lock().await = Some(server);
    PROCESS_RESET_INSTALLED.store(true, Ordering::SeqCst);
}

/// Reset the booted target by restarting its process. Errors when nothing is
/// installed or the replacement never becomes ready; callers treat that as a
/// failed reset (the finding stays a candidate), never as clean state.
pub(crate) async fn run_process_reset() -> Result<()> {
    let mut slot = process_reset_slot().lock().await;
    match slot.as_mut() {
        Some(server) => server.restart().await,
        None => bail!("no reproit-booted server is installed as the reset target"),
    }
}

/// Tear down the booted server and clear the reset capability. Called on every
/// find/check exit path that booted a target.
pub(crate) async fn shutdown_process_reset() {
    PROCESS_RESET_INSTALLED.store(false, Ordering::SeqCst);
    if let Some(server) = process_reset_slot().lock().await.take() {
        server.teardown().await;
    }
}

impl Drop for BootedServer {
    fn drop(&mut self) {
        // kill_on_drop reaps the direct child; the group signal reaches any
        // grandchildren a shell start script left behind.
        signal_group(self.process_id, true);
    }
}

#[cfg(unix)]
fn shell_command(command: &str) -> tokio::process::Command {
    let mut spawned = tokio::process::Command::new("sh");
    spawned.arg("-c").arg(command);
    spawned
}

#[cfg(not(unix))]
fn shell_command(command: &str) -> tokio::process::Command {
    let mut spawned = tokio::process::Command::new("cmd");
    spawned.arg("/C").arg(command);
    spawned
}

/// `npm run` puts node_modules/.bin first on PATH; running the script through
/// the shell must match, or `nodemon`/`ts-node` start scripts break.
fn path_with_node_bin(root: &Path) -> std::ffi::OsString {
    let current = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![root.join("node_modules/.bin")];
    paths.extend(std::env::split_paths(&current));
    std::env::join_paths(paths).unwrap_or(current)
}

#[cfg(unix)]
fn configure_process_group(command: &mut tokio::process::Command) {
    use std::os::unix::process::CommandExt;
    command.as_std_mut().process_group(0);
}

#[cfg(not(unix))]
fn configure_process_group(_command: &mut tokio::process::Command) {}

#[cfg(unix)]
fn signal_group(process_id: u32, kill: bool) {
    let signal = if kill { libc::SIGKILL } else { libc::SIGTERM };
    // SAFETY: the child was placed in its own process group whose id is the
    // root child pid; a negative pid targets only that owned group. The root
    // may already be gone; ESRCH is harmless.
    unsafe {
        libc::kill(-(process_id as i32), signal);
    }
}

#[cfg(not(unix))]
fn signal_group(_process_id: u32, _kill: bool) {}
