//! Hermetic re-execution of a captured production failure.
//!
//! `reproit check <capture.json> --exec "<command>"` boots the application
//! under the SDK's replay mode (`REPROIT_REPLAY`), fires the capture's
//! recorded inbound request at it, and states a verdict from the LIVE
//! response of the re-executed code, not from the recorded log:
//!
//! - `reproduced`: the oracle fires again (5xx for `backend-server-error`).
//! - `fixed`: the operation now answers cleanly.
//! - `diverged`: the code made an outbound call the capture never saw (the
//!   SDK's `REPROIT:DIVERGENCE` marker); drift, neither proof nor pass.
//! - `inconclusive`: the app did not boot, did not answer, or answered in a
//!   class the oracle cannot judge. Fails closed, like the gate.
//!
//! No live dependency is contacted: the SDK serves every recorded exchange
//! in process, which is why this run is valid on a laptop with the database
//! stopped and the network denied.

use crate::domain::backend::BackendEventKind;
use crate::interface::cli::context::{Ctx, Exit};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::io::BufRead;
use std::path::Path;
use std::process::{ExitCode, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The SDK's structured divergence marker (replay.js DIVERGENCE_MARKER).
const DIVERGENCE_MARKER: &str = "REPROIT:DIVERGENCE ";
/// Boot budget before the run is declared inconclusive.
const BOOT_TIMEOUT: Duration = Duration::from_secs(20);
/// Grace for the child's stderr to flush after the response arrives.
const STDERR_GRACE: Duration = Duration::from_millis(200);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HermeticVerdict {
    Reproduced,
    Fixed,
    Diverged,
    Inconclusive,
}

impl HermeticVerdict {
    pub fn as_str(self) -> &'static str {
        match self {
            HermeticVerdict::Reproduced => "reproduced",
            HermeticVerdict::Fixed => "fixed",
            HermeticVerdict::Diverged => "diverged",
            HermeticVerdict::Inconclusive => "inconclusive",
        }
    }

    /// check's CI exit contract: reproduced is the regression stop; diverged
    /// and inconclusive both fail closed as "go re-record / go look", never
    /// as pass.
    pub(crate) fn exit_code(self) -> u8 {
        match self {
            HermeticVerdict::Fixed => Exit::Clean as u8,
            HermeticVerdict::Reproduced => Exit::Regression as u8,
            HermeticVerdict::Diverged | HermeticVerdict::Inconclusive => Exit::Stale as u8,
        }
    }

    fn exit(self) -> ExitCode {
        ExitCode::from(self.exit_code())
    }
}

/// One divergence report parsed off the child's stderr.
struct ChildOutput {
    divergences: Arc<Mutex<Vec<Value>>>,
}

fn watch_stderr(child: &mut std::process::Child) -> ChildOutput {
    let divergences = Arc::new(Mutex::new(Vec::new()));
    if let Some(stderr) = child.stderr.take() {
        let sink = Arc::clone(&divergences);
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(stderr)
                .lines()
                .map_while(Result::ok)
            {
                if let Some(report) = line.strip_prefix(DIVERGENCE_MARKER) {
                    let parsed =
                        serde_json::from_str(report).unwrap_or_else(|_| json!({"raw": report}));
                    if let Ok(mut reports) = sink.lock() {
                        reports.push(parsed);
                    }
                }
            }
        });
    }
    ChildOutput { divergences }
}

/// Fire the capture's recorded inbound trigger at a booted instance. Shared
/// with re-recording (`keep --refresh`), so a refresh asks the app exactly
/// what production asked it, never a reconstruction.
pub(super) async fn fire_recorded_trigger(
    client: &reqwest::Client,
    base: &str,
    artifact: &super::capture_replay::CaptureArtifact,
) -> Result<reqwest::Response> {
    Ok(inbound_request(client, base, artifact)?.send().await?)
}

/// Build the capture's recorded inbound request against the booted port.
fn inbound_request(
    client: &reqwest::Client,
    base: &str,
    artifact: &super::capture_replay::CaptureArtifact,
) -> Result<reqwest::RequestBuilder> {
    let (method, path) = artifact
        .operation
        .split_once(' ')
        .context("capture operation is not 'METHOD /path'")?;
    let method = reqwest::Method::from_bytes(method.as_bytes())
        .with_context(|| format!("capture operation method {method:?}"))?;
    let start_input = artifact
        .events
        .iter()
        .find_map(|event| match &event.event {
            BackendEventKind::Start { input } => Some(input.clone()),
            _ => None,
        })
        .unwrap_or(Value::Null);
    let mut url = reqwest::Url::parse(&format!("{base}{path}"))
        .with_context(|| format!("capture operation path {path:?}"))?;
    if let Some(query) = start_input.get("query").and_then(Value::as_object) {
        let mut pairs = url.query_pairs_mut();
        for (name, value) in query {
            let rendered = match value {
                Value::String(text) => text.clone(),
                other => other.to_string(),
            };
            pairs.append_pair(name, &rendered);
        }
    }
    let mut request = client.request(method, url);
    if let Some(body) = start_input.get("body").filter(|body| !body.is_null()) {
        request = request.json(body);
    }
    Ok(request)
}

/// The programmatic result of one hermetic run, shared by `check --exec` and
/// the kept-guard suite.
pub struct HermeticOutcome {
    pub verdict: HermeticVerdict,
    pub divergences: Vec<Value>,
    pub operation: String,
    pub oracle: String,
    pub envelope: Option<Value>,
}

/// Boot `command` under `REPROIT_REPLAY`, fire the capture's recorded inbound
/// request, and observe the verdict. Does not print or exit.
pub async fn run_hermetic(file: &Path, command: &str) -> Result<HermeticOutcome> {
    let (outcome, held) = run_hermetic_session(file, command, false).await?;
    drop(held);
    Ok(outcome)
}

/// A replayed app deliberately kept alive after its verdict so a developer
/// can inspect the failing state. Dropping it kills the process.
pub struct HeldSession {
    pub base: String,
    _guard: KillOnDrop,
}

/// True when a human is on both ends of this process and can use a held
/// session; agents and CI (non-TTY, `--json`, `--yes`) always get the
/// headless `--auto` behavior without asking for it.
pub fn interactive_session(ctx: &Ctx, auto: bool) -> bool {
    use std::io::IsTerminal;
    !auto
        && !ctx.json
        && !ctx.confirmed()
        && std::io::stdin().is_terminal()
        && std::io::stdout().is_terminal()
}

/// The daily-action variant: with `hold`, the replayed app is kept running
/// after the verdict (returned as a `HeldSession`) so the developer can
/// attach a debugger, curl the held URL, or otherwise observe the failing
/// state before releasing it.
async fn run_hermetic_session(
    file: &Path,
    command: &str,
    hold: bool,
) -> Result<(HermeticOutcome, Option<HeldSession>)> {
    let bytes = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    let artifact = super::capture_replay::parse_capture(&bytes)?;
    let has_exchanges = artifact.events.iter().any(|event| {
        matches!(
            &event.event,
            BackendEventKind::Effect {
                exchange: Some(_),
                ..
            }
        )
    });
    if !has_exchanges {
        bail!(
            "capture has no recorded dependency exchanges, so hermetic replay would silently \
             reach live dependencies; re-capture with the SDK's outbound instrumentation \
             (instrument.install()), or evaluate offline with `reproit check {}`",
            file.display()
        );
    }

    let port = free_port()?;
    let base = format!("http://127.0.0.1:{port}");
    let capture_path = std::fs::canonicalize(file)?;
    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .env("REPROIT_REPLAY", &capture_path)
        .env("PORT", port.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawn --exec command {command:?}"))?;
    let output = watch_stderr(&mut child);
    let guard = KillOnDrop(child);

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()?;
    let booted = wait_for_boot(&client, &base).await;
    let verdict = if !booted {
        HermeticVerdict::Inconclusive
    } else {
        let response = inbound_request(&client, &base, &artifact)?.send().await;
        tokio::time::sleep(STDERR_GRACE).await;
        let diverged = !output
            .divergences
            .lock()
            .map(|d| d.is_empty())
            .unwrap_or(true);
        match (diverged, response) {
            (true, _) => HermeticVerdict::Diverged,
            (false, Ok(response)) => {
                let status = response.status().as_u16();
                if status >= 500 {
                    HermeticVerdict::Reproduced
                } else if (200..400).contains(&status) {
                    HermeticVerdict::Fixed
                } else {
                    HermeticVerdict::Inconclusive
                }
            }
            (false, Err(_)) => HermeticVerdict::Inconclusive,
        }
    };
    let held = if hold && booted {
        Some(HeldSession {
            base: base.clone(),
            _guard: guard,
        })
    } else {
        drop(guard);
        None
    };

    let divergences = output
        .divergences
        .lock()
        .map(|reports| reports.clone())
        .unwrap_or_default();
    Ok((
        HermeticOutcome {
            verdict,
            divergences,
            operation: artifact.operation,
            oracle: artifact.oracle,
            envelope: artifact.envelope,
        },
        held,
    ))
}

/// `check <capture.json> --exec "<command>"`: the hermetic verdict path.
/// Interactive sessions (a human on a TTY without `--auto`) hold the
/// replayed app alive after the verdict for debugger-attach or curl.
pub async fn check_capture_exec(
    ctx: &Ctx,
    file: &Path,
    command: &str,
    auto: bool,
) -> Result<ExitCode> {
    let hold = interactive_session(ctx, auto);
    let (outcome, held) = run_hermetic_session(file, command, hold).await?;
    let HermeticOutcome {
        verdict,
        divergences,
        operation,
        oracle,
        envelope,
    } = outcome;
    ctx.emit(&json!({
        "command": "check",
        "capture": {
            "file": file.display().to_string(),
            "operation": operation,
            "oracle": oracle,
            "mode": "hermetic-exec",
            "verdict": verdict.as_str(),
            "divergences": divergences,
            // The determinism envelope the SDK pinned the replay to (TZ,
            // capture clock, replay seed); null on envelope-less captures.
            "envelope": envelope,
        },
        "outcome": match verdict {
            HermeticVerdict::Fixed => "pass",
            HermeticVerdict::Reproduced => "fail",
            _ => "stale",
        },
        "exit": match verdict {
            HermeticVerdict::Fixed => 0u8,
            HermeticVerdict::Reproduced => 1,
            _ => 3,
        },
    }));
    ctx.say(format!("check capture {} (hermetic)", file.display()));
    match verdict {
        HermeticVerdict::Reproduced => ctx.say(format!(
            "  FAIL reproduced by re-execution ({oracle} on {operation})"
        )),
        HermeticVerdict::Fixed => ctx.say("  PASS the operation now answers cleanly"),
        HermeticVerdict::Diverged => {
            ctx.say("  DIVERGED the code no longer makes the captured calls:");
            for report in &divergences {
                ctx.say(format!("    {report}"));
            }
        }
        HermeticVerdict::Inconclusive => {
            ctx.say("  INCONCLUSIVE the app did not boot or did not answer; failing closed")
        }
    }
    if let Some(held) = held {
        ctx.say(format!(
            "\n  held for inspection: the app is still running under hermetic replay at {}\n  \
             attach a debugger, curl the operation, or re-fire the request; Enter releases it",
            held.base
        ));
        let _ = tokio::task::spawn_blocking(|| {
            let mut line = String::new();
            let _ = std::io::stdin().read_line(&mut line);
        })
        .await;
        drop(held);
    }
    Ok(verdict.exit())
}

/// The exec recipe a hermetic guard stores (hermetic.json). None when the
/// recipe is unreadable or names no `exec` command; callers fail closed.
pub(super) fn guard_exec(recipe: &Path) -> Option<String> {
    serde_json::from_slice::<Value>(&std::fs::read(recipe).ok()?)
        .ok()?
        .get("exec")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// `reproit check <guard-id>` routing: when the referenced repro directory
/// holds a hermetic capture guard (capture.json + hermetic.json), replay it
/// through the hermetic verdict path with the stored exec recipe. Returns
/// None when the reference is not a hermetic guard.
pub async fn try_replay_hermetic_guard(
    ctx: &Ctx,
    reference: &str,
    auto: bool,
) -> Result<Option<ExitCode>> {
    let root = std::env::current_dir()?;
    let Some(meta) = crate::domain::repro::resolve(&root, reference) else {
        return Ok(None);
    };
    let directory = crate::domain::repro::repro_dir(&root, &meta.id);
    let capture = directory.join("capture.json");
    let recipe = directory.join("hermetic.json");
    if !capture.is_file() || !recipe.is_file() {
        return Ok(None);
    }
    let exec = guard_exec(&recipe)
        .with_context(|| format!("{} has no `exec` command", recipe.display()))?;
    let code = check_capture_exec(ctx, &capture, &exec, auto).await?;
    Ok(Some(code))
}

/// `reproit keep <capture.json> --exec "<command>"`: land a hermetic capture
/// guard in `.reproit/repros/<id>/` so `reproit check` re-executes it on
/// every run. The guard is proven live BEFORE it is kept: a capture whose
/// current verdict is diverged or inconclusive would be dead on arrival and
/// is refused with the verdict named. The exec recipe is user-authored repo
/// config (hermetic.json); the capture never supplies the command.
pub async fn keep_capture_guard(
    ctx: &Ctx,
    file: &Path,
    exec: &str,
    alias: Option<&str>,
    strict: bool,
) -> Result<ExitCode> {
    let outcome = run_hermetic(file, exec).await?;
    match outcome.verdict {
        HermeticVerdict::Reproduced | HermeticVerdict::Fixed => {}
        verdict => bail!(
            "refusing to keep a guard whose current hermetic verdict is {}; a guard must \
             reproduce (bug present) or hold (bug fixed) at keep time to mean anything in CI",
            verdict.as_str()
        ),
    }
    let bytes = std::fs::read(file)?;
    let digest = super::encoding::hex_hash(&bytes);
    let id = digest[..12].to_string();
    let root = std::env::current_dir()?;
    let directory = crate::domain::repro::repro_dir(&root, &id);
    std::fs::create_dir_all(&directory)?;
    std::fs::write(directory.join("capture.json"), &bytes)?;
    std::fs::write(
        directory.join("hermetic.json"),
        serde_json::to_vec_pretty(&json!({ "exec": exec }))?,
    )?;
    let meta = crate::domain::repro::Meta {
        id: id.clone(),
        alias: alias.map(str::to_string),
        status: if strict {
            crate::domain::repro::Status::Required
        } else {
            crate::domain::repro::Status::Quarantined
        },
        seed: 0,
        created: chrono::Utc::now().to_rfc3339(),
        last_checked: None,
        last_result: None,
        trigger_index: None,
        trigger_sig: None,
        trigger_selector: None,
        trigger_fingerprint: None,
        oracle: Some(outcome.oracle.clone()),
        record_url: None,
        record_action: None,
    };
    crate::domain::repro::save_meta(&root, &meta)?;
    ctx.emit(&json!({
        "command": "keep",
        "source": "capture",
        "id": id,
        "alias": alias,
        "status": meta.status.as_str(),
        "directory": directory,
        "verdictAtKeep": outcome.verdict.as_str(),
        "oracle": outcome.oracle,
    }));
    ctx.say(format!("Kept hermetic capture guard {id}"));
    ctx.say(format!("  verdict now: {}", outcome.verdict.as_str()));
    ctx.say(format!("  status:      {}", meta.status.as_str()));
    ctx.say(format!("  guard:       {}", directory.display()));
    ctx.say("  reproit check replays it hermetically on every run");
    Ok(ExitCode::SUCCESS)
}

async fn wait_for_boot(client: &reqwest::Client, base: &str) -> bool {
    let deadline = std::time::Instant::now() + BOOT_TIMEOUT;
    while std::time::Instant::now() < deadline {
        // Any HTTP answer at all means the listener is up; readiness is not
        // the same as a 200 (the root path may legitimately 404).
        if client.get(base).send().await.is_ok() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    false
}

fn free_port() -> Result<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}

/// The child must never outlive the check, even on early error returns.
struct KillOnDrop(std::process::Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The invariant ledger found `Diverged` defended by shell scripts alone:
    /// it appeared in production code and in zero Rust tests, so a refactor
    /// that collapsed it into `Inconclusive` would keep every acceptance
    /// script green while destroying the distinction drift quarantine is
    /// built on. A diverged run means the code no longer makes the captured
    /// calls, which is neither a reproduction nor a proof of a fix.
    #[test]
    fn diverged_is_its_own_verdict_and_never_certifies() {
        assert_eq!(HermeticVerdict::Diverged.as_str(), "diverged");
        for verdict in [
            HermeticVerdict::Reproduced,
            HermeticVerdict::Fixed,
            HermeticVerdict::Inconclusive,
        ] {
            assert_ne!(
                verdict.as_str(),
                HermeticVerdict::Diverged.as_str(),
                "diverged must not share an identity with {verdict:?}"
            );
        }
    }

    /// The rule this project has paid for twice (the verify false-open and the
    /// gate blindness) was enforced only by a ratchet that greps for the token
    /// `Inconclusive` in four named files, which is a spelling check rather
    /// than a behavioral one. Absence of an observed failure is not a fix, so
    /// only `Fixed` may exit zero.
    #[test]
    fn only_a_proven_fix_exits_zero() {
        let success = format!("{:?}", ExitCode::SUCCESS);
        assert_eq!(format!("{:?}", HermeticVerdict::Fixed.exit()), success);
        for verdict in [
            HermeticVerdict::Reproduced,
            HermeticVerdict::Diverged,
            HermeticVerdict::Inconclusive,
        ] {
            assert_ne!(
                format!("{:?}", verdict.exit()),
                success,
                "{verdict:?} must never exit zero: absence of evidence is not a fix"
            );
        }
    }

    /// Reproduced is the regression stop, and the two fail-closed verdicts
    /// must be distinguishable from it so CI can block on a real regression
    /// while merely reporting drift.
    #[test]
    fn a_regression_is_distinguishable_from_failing_closed() {
        let reproduced = format!("{:?}", HermeticVerdict::Reproduced.exit());
        let diverged = format!("{:?}", HermeticVerdict::Diverged.exit());
        let inconclusive = format!("{:?}", HermeticVerdict::Inconclusive.exit());
        assert_ne!(reproduced, diverged);
        assert_eq!(
            diverged, inconclusive,
            "both fail-closed verdicts share the stale exit class by design"
        );
    }
}
