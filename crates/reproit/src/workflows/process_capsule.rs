//! The process capsule: capture and hermetic re-execution for programs that
//! are not request shaped.
//!
//! A backend capture records one request and its dependency exchanges. A
//! process capsule records a whole PROCESS: its argv, cwd, pinned
//! environment, executable digest, every read of the outside world observed
//! at the dynamic-linking boundary (runners/process-shim), and the oracle a
//! machine can decide, which for a program is how it died.
//!
//! It is a sibling format of `reproit-backend-capture`, not an extension of
//! it, because the two differ in trigger and oracle: a backend capture
//! re-fires one recorded request and judges an HTTP status, while a process
//! capsule re-execs a command line and judges an exit status or fatal
//! signal. Both share the determinism envelope and the same four-way verdict
//! vocabulary, so one CI gate and one agent tool read either.

use crate::interface::cli::context::{Ctx, Exit};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::BufRead;
use std::path::Path;
use std::process::{ExitCode, Stdio};
use std::sync::{Arc, Mutex};

use crate::workflows::backend_headless::HermeticVerdict;

const FORMAT: &str = "reproit-process-capsule";
const VERSION: u16 = 1;
const DIVERGENCE_MARKER: &str = "REPROIT:DIVERGENCE ";
const COUNTER_MARKER: &str = "REPROIT:PROCESS-REPLAY ";
/// Environment names a capsule pins verbatim. An allowlist, not the whole
/// environment: a developer's shell carries credentials a capsule must not.
const ENV_ALLOWLIST: [&str; 6] = ["PATH", "LANG", "LC_ALL", "TZ", "HOME", "SHELL"];
/// A capsule holds a bounded log; a program that reads without limit is
/// truncated with the count stated rather than silently trimmed.
const MAX_ENTRIES: usize = 8192;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessCapsule {
    pub format: String,
    pub version: u16,
    pub command: Vec<String>,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable_sha256: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    pub envelope: Value,
    pub oracle: String,
    pub outcome: Outcome,
    /// The shim's tab separated boundary log, one entry per line.
    pub entries: Vec<String>,
    #[serde(default)]
    pub truncated_entries: usize,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Outcome {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<i32>,
}

impl Outcome {
    /// A program's oracle: how it died. Anything other than a clean exit is a
    /// failure a capsule can be built around.
    fn failed(&self) -> bool {
        self.signal.is_some() || self.exit_code.is_some_and(|code| code != 0)
    }

    /// How a program died, canonicalized. A capture spawns the program
    /// directly and sees the fatal SIGNAL; a replay runs it through a shell,
    /// and POSIX shells report a signal death as exit code 128 plus the
    /// signal number. Both describe the same death, so the comparison folds
    /// that convention rather than calling one death a different failure.
    fn canonical_signal(&self) -> Option<i32> {
        if let Some(signal) = self.signal {
            return Some(signal);
        }
        match self.exit_code {
            Some(code) if (129..=192).contains(&code) => Some(code - 128),
            _ => None,
        }
    }

    fn same_as(&self, other: &Outcome) -> bool {
        match (self.canonical_signal(), other.canonical_signal()) {
            (Some(left), Some(right)) => left == right,
            (None, None) => self.exit_code == other.exit_code,
            _ => false,
        }
    }

    fn describe(&self) -> String {
        match (self.canonical_signal(), self.exit_code) {
            (Some(signal), _) => format!("fatal signal {signal}"),
            (None, Some(code)) => format!("exit {code}"),
            (None, None) => "unknown".to_string(),
        }
    }
}

/// Routing sniff: is this file a process capsule? Parse failures read as no,
/// so an unreadable path falls through to the other check routes.
pub fn is_process_capsule(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|value| value.get("format").map(|format| format == FORMAT))
        .unwrap_or(false)
}

fn parse(path: &Path) -> Result<ProcessCapsule> {
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let capsule: ProcessCapsule =
        serde_json::from_slice(&bytes).context("file is not a reproit-process-capsule payload")?;
    if capsule.format != FORMAT {
        bail!("unsupported capsule format {:?}", capsule.format);
    }
    if capsule.version != VERSION {
        bail!("unsupported capsule version {}", capsule.version);
    }
    Ok(capsule)
}

/// The shim library the capsule was recorded with, and that replay must load.
/// Repo-local configuration only: a capsule can never name a library to load,
/// exactly as it can never supply an exec command.
fn shim_path() -> Result<String> {
    if let Ok(path) = std::env::var("REPROIT_PROCESS_SHIM") {
        if Path::new(&path).is_file() {
            return Ok(path);
        }
    }
    bail!(
        "no process shim is available. Build runners/process-shim/reproit_shim.c for this \
         platform and set REPROIT_PROCESS_SHIM to the resulting library"
    )
}

/// The loader variable that injects the shim: LD_PRELOAD on Linux,
/// DYLD_INSERT_LIBRARIES on macOS, where SIP additionally strips it for
/// system binaries.
fn preload_var() -> &'static str {
    if cfg!(target_os = "macos") {
        "DYLD_INSERT_LIBRARIES"
    } else {
        "LD_PRELOAD"
    }
}

fn digest_of(path: &Path) -> Option<String> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(path).ok()?;
    let digest = Sha256::digest(&bytes);
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(&mut encoded, "{byte:02x}");
    }
    Some(format!("sha256:{encoded}"))
}

/// `reproit internal process-capture --out <capsule> -- <command>`: run the
/// command under the recording shim and assemble a capsule from what the
/// boundary saw plus how the program died.
pub fn capture(ctx: &Ctx, out: &Path, command: &[String]) -> Result<ExitCode> {
    let Some((program, arguments)) = command.split_first() else {
        bail!("process capture needs a command to run");
    };
    let shim = shim_path()?;
    let log = tempfile_path("record");
    let seed = format!("{:016x}", rand_seed());
    let status = std::process::Command::new(program)
        .args(arguments)
        .env(preload_var(), &shim)
        .env("REPROIT_RECORD", &log)
        .env("REPROIT_REPLAY_SEED", &seed)
        .status()
        .with_context(|| format!("spawn {program}"))?;

    let mut entries: Vec<String> = std::fs::read_to_string(&log)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect();
    let truncated = entries.len().saturating_sub(MAX_ENTRIES);
    entries.truncate(MAX_ENTRIES);
    let _ = std::fs::remove_file(&log);

    let outcome = Outcome {
        exit_code: status.code(),
        signal: exit_signal(&status),
    };
    if !outcome.failed() {
        ctx.say("the command exited cleanly, so there is no failure to capture");
    }
    let env = ENV_ALLOWLIST
        .iter()
        .filter_map(|name| {
            std::env::var(name)
                .ok()
                .map(|value| (name.to_string(), value))
        })
        .collect::<BTreeMap<_, _>>();
    let capsule = ProcessCapsule {
        format: FORMAT.to_string(),
        version: VERSION,
        command: command.to_vec(),
        cwd: std::env::current_dir()?.display().to_string(),
        executable_sha256: digest_of(Path::new(program)),
        env,
        envelope: json!({
            "observedAtMs": chrono::Utc::now().timestamp_millis(),
            "tz": std::env::var("TZ").unwrap_or_default(),
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "replaySeed": seed,
        }),
        oracle: if outcome.signal.is_some() {
            "process-signal".to_string()
        } else {
            "process-exit".to_string()
        },
        outcome,
        entries,
        truncated_entries: truncated,
    };
    std::fs::write(out, serde_json::to_vec_pretty(&capsule)?)?;
    ctx.emit(&json!({
        "command": "process capture",
        "capsule": out.display().to_string(),
        "entries": capsule.entries.len(),
        "truncated": capsule.truncated_entries,
        "oracle": capsule.oracle,
        "outcome": capsule.outcome,
    }));
    ctx.say(format!(
        "Captured {} into {}",
        capsule.oracle,
        out.display()
    ));
    ctx.say(format!("  boundary entries: {}", capsule.entries.len()));
    ctx.say(format!(
        "  outcome:          {}",
        capsule.outcome.describe()
    ));
    Ok(ExitCode::SUCCESS)
}

/// Counters the shim reports at replay exit. Best effort: a program that
/// dies on a fatal signal never runs the reporting destructor, so absent
/// counters are normal for exactly the crashes this feature exists to
/// reproduce, and the divergence LINES (streamed as they happen) are the
/// authority instead.
#[derive(Default, Debug, Clone, Copy)]
struct Counters {
    served: u64,
    diverged: u64,
    clock_overrun: u64,
    random_overrun: u64,
    env_fallthrough: u64,
}

struct ReplayObservation {
    divergences: Vec<Value>,
    counters: Option<Counters>,
}

fn watch(child: &mut std::process::Child) -> Arc<Mutex<ReplayObservation>> {
    let sink = Arc::new(Mutex::new(ReplayObservation {
        divergences: Vec::new(),
        counters: None,
    }));
    if let Some(stderr) = child.stderr.take() {
        let sink = Arc::clone(&sink);
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(stderr)
                .lines()
                .map_while(Result::ok)
            {
                if let Some(report) = line.strip_prefix(DIVERGENCE_MARKER) {
                    let parsed =
                        serde_json::from_str(report).unwrap_or_else(|_| json!({ "raw": report }));
                    if let Ok(mut observation) = sink.lock() {
                        observation.divergences.push(parsed);
                    }
                } else if let Some(report) = line.strip_prefix(COUNTER_MARKER) {
                    if let Ok(value) = serde_json::from_str::<Value>(report) {
                        let read =
                            |name: &str| value.get(name).and_then(Value::as_u64).unwrap_or(0);
                        if let Ok(mut observation) = sink.lock() {
                            observation.counters = Some(Counters {
                                served: read("served"),
                                diverged: read("diverged"),
                                clock_overrun: read("clockOverrun"),
                                random_overrun: read("randomOverrun"),
                                env_fallthrough: read("envFallthrough"),
                            });
                        }
                    }
                } else {
                    eprintln!("{line}");
                }
            }
        });
    }
    sink
}

/// `reproit check <capsule> --exec "<command>"`: re-exec the program under
/// the serving shim, with the capsule's environment and seed pinned, and
/// judge the verdict from how the re-executed program died.
pub async fn check_exec(ctx: &Ctx, file: &Path, command: &str, _auto: bool) -> Result<ExitCode> {
    let capsule = parse(file)?;
    let shim = shim_path()?;
    let log = tempfile_path("replay");
    std::fs::write(&log, capsule.entries.join("\n") + "\n")?;

    let seed = capsule
        .envelope
        .get("replaySeed")
        .and_then(Value::as_str)
        .unwrap_or("c0ffee00c0ffee00")
        .to_string();
    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .env(preload_var(), &shim)
        .env("REPROIT_REPLAY_LOG", &log)
        .env("REPROIT_REPLAY_SEED", &seed)
        .envs(capsule.env.clone())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawn --exec command {command:?}"))?;
    let observation = watch(&mut child);
    let status = child.wait()?;
    let _ = std::fs::remove_file(&log);

    let observed = Outcome {
        exit_code: status.code(),
        signal: exit_signal(&status),
    };
    let (divergences, counters) = {
        let guard = observation
            .lock()
            .map_err(|_| anyhow::anyhow!("stderr reader panicked"))?;
        (guard.divergences.clone(), guard.counters)
    };
    let verdict = if !divergences.is_empty() {
        HermeticVerdict::Diverged
    } else if observed.same_as(&capsule.outcome) {
        HermeticVerdict::Reproduced
    } else if !observed.failed() {
        HermeticVerdict::Fixed
    } else {
        // A different failure is not this failure. Fails closed rather than
        // claiming a reproduction the capsule does not describe.
        HermeticVerdict::Inconclusive
    };

    ctx.emit(&json!({
        "command": "check",
        "capsule": {
            "file": file.display().to_string(),
            "format": FORMAT,
            "mode": "process-hermetic",
            "oracle": capsule.oracle,
            "verdict": verdict.as_str(),
            "recordedOutcome": capsule.outcome,
            "observedOutcome": observed,
            "divergences": divergences,
            "counters": counters.map(|c| json!({
                "served": c.served,
                "diverged": c.diverged,
                "clockOverrun": c.clock_overrun,
                "randomOverrun": c.random_overrun,
                "envFallthrough": c.env_fallthrough,
            })),
            "envelope": capsule.envelope,
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
    ctx.say(format!(
        "check capsule {} (process hermetic)",
        file.display()
    ));
    match verdict {
        HermeticVerdict::Reproduced => ctx.say(format!(
            "  FAIL reproduced by re-execution ({} on {})",
            capsule.outcome.describe(),
            capsule.oracle
        )),
        HermeticVerdict::Fixed => ctx.say("  PASS the program now exits cleanly"),
        HermeticVerdict::Diverged => {
            ctx.say("  DIVERGED the program read something the capsule never recorded:");
            for report in &divergences {
                ctx.say(format!("    {report}"));
            }
        }
        HermeticVerdict::Inconclusive => ctx.say(format!(
            "  INCONCLUSIVE recorded {}, observed {}; failing closed",
            capsule.outcome.describe(),
            observed.describe()
        )),
    }
    if let Some(counters) = counters {
        ctx.say(format!(
            "  boundary: {} served, {} diverged, {} clock overrun, {} rng overrun, {} env \
             fallthrough",
            counters.served,
            counters.diverged,
            counters.clock_overrun,
            counters.random_overrun,
            counters.env_fallthrough
        ));
    }
    Ok(match verdict {
        HermeticVerdict::Fixed => ExitCode::SUCCESS,
        HermeticVerdict::Reproduced => Exit::Regression.code(),
        _ => ExitCode::from(3),
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

fn tempfile_path(kind: &str) -> String {
    let directory = std::env::temp_dir();
    let stamp = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
    directory
        .join(format!(
            "reproit-process-{kind}-{}-{stamp}.log",
            std::process::id()
        ))
        .display()
        .to_string()
}

fn rand_seed() -> u64 {
    let stamp = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default() as u64;
    stamp ^ (std::process::id() as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_failure_and_equality_follow_how_the_program_died() {
        let clean = Outcome {
            exit_code: Some(0),
            signal: None,
        };
        let aborted = Outcome {
            exit_code: None,
            signal: Some(6),
        };
        let nonzero = Outcome {
            exit_code: Some(4),
            signal: None,
        };
        assert!(!clean.failed());
        assert!(aborted.failed());
        assert!(nonzero.failed());
        assert!(aborted.same_as(&aborted));
        assert!(!aborted.same_as(&nonzero));
        assert_eq!(aborted.describe(), "fatal signal 6");
        assert_eq!(nonzero.describe(), "exit 4");
        // A shell reports the same abort as 128 + SIGABRT; the two spellings
        // of one death must compare equal, and must not swallow a genuinely
        // different exit code.
        let through_shell = Outcome {
            exit_code: Some(134),
            signal: None,
        };
        assert!(through_shell.same_as(&aborted));
        assert!(aborted.same_as(&through_shell));
        assert_eq!(through_shell.describe(), "fatal signal 6");
        assert!(!through_shell.same_as(&nonzero));
        assert!(!clean.same_as(&aborted));
    }

    #[test]
    fn a_non_capsule_file_does_not_route_to_the_process_path() {
        let directory =
            std::env::temp_dir().join(format!("reproit-capsule-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let backend = directory.join("backend.json");
        std::fs::write(&backend, br#"{"format":"reproit-backend-capture"}"#).unwrap();
        assert!(!is_process_capsule(&backend));
        let capsule = directory.join("process.json");
        std::fs::write(&capsule, br#"{"format":"reproit-process-capsule"}"#).unwrap();
        assert!(is_process_capsule(&capsule));
        assert!(!is_process_capsule(&directory.join("absent.json")));
        let _ = std::fs::remove_dir_all(&directory);
    }
}
