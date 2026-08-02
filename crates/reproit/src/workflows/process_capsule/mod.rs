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
/// Bounded stderr retention: enough to hold an assertion plus a short
/// backtrace, never enough for a looping program to exhaust memory.
const MAX_STDERR_LINES: usize = 64;

/// The failure text a program prints when it aborts on a declared invariant.
/// These are exact formats emitted by the runtime itself, not prose guesses:
/// glibc `assert`, Rust panics, C++ `terminate`, and Go panics.
const ASSERTION_MARKERS: [&str; 5] = [
    "Assertion `",
    "assertion failed",
    "panicked at",
    "terminate called",
    "panic: ",
];

/// Reduce one failure line to an identity that survives a rerun. ONLY
/// hexadecimal addresses are folded, because ASLR moves them between two runs
/// of the same defect while nothing else in the line moves: a record and its
/// replay run the same binary, so file names, line numbers, and the asserted
/// predicate are all stable.
///
/// Folding decimal digits as well was tried and rejected. It made
/// `Assertion `n < 8'` and `Assertion `n < 9'` compare EQUAL, which is exactly
/// the false proof this identity exists to prevent: two different assertions
/// reported as one reproduction. When a signature must be loosened, the cost
/// is always paid in the direction of calling different failures the same, so
/// the bias here is to fold as little as possible.
/// Rust's default panic line embeds the OS thread id: `thread 'main' (1681)
/// panicked at ...`. The id changes on every run, so leaving it in makes a
/// panic identity unmatchable with its own replay (measured on the bevy
/// session subject: recorded exit 101, observed exit 101, INCONCLUSIVE).
/// Fold exactly that token and nothing else; the thread NAME stays, because
/// two panics on differently named threads are different failures.
fn fold_thread_id(line: &str) -> String {
    if let Some(start) = line.find("' (") {
        let rest = &line[start + 3..];
        if let Some(end) = rest.find(") panicked at ") {
            if end > 0 && rest[..end].bytes().all(|b| b.is_ascii_digit()) {
                return format!("{}{}", &line[..start + 1], &rest[end + 1..]);
            }
        }
    }
    line.to_string()
}

fn normalize_failure(line: &str) -> String {
    let line = fold_thread_id(line);
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '0' && chars.peek() == Some(&'x') {
            chars.next();
            while chars.peek().is_some_and(|n| n.is_ascii_hexdigit()) {
                chars.next();
            }
            out.push_str("0xADDR");
        } else {
            out.push(c);
        }
    }
    out.trim().to_string()
}

/// The recorded or observed failure identity: the normalized assertion or
/// panic line, when the program declared one. `None` means the program died
/// without declaring why, in which case the signal is the whole story.
fn failure_signature(stderr: &[String]) -> Option<String> {
    stderr
        .iter()
        .rev()
        .find(|line| ASSERTION_MARKERS.iter().any(|m| line.contains(m)))
        .map(|line| normalize_failure(line))
}
const COUNTER_MARKER: &str = "REPROIT:PROCESS-REPLAY ";
/// Environment names whose VALUES a capsule refuses to carry. Everything else
/// is recorded verbatim and restored at replay, because an interpreter's
/// startup path is decided by its environment: measured, a python3 replay
/// resolved a different locale and a different prefix than the recorded run
/// and diverged on both. Pinning the block is what makes the two runs take
/// the same path. Secret shaped names are dropped rather than shipped, on the
/// same fold-to-alphanumerics rule the SDKs use.
const ENV_SECRET_PARTS: [&str; 14] = [
    "password",
    "passwd",
    "secret",
    "token",
    "authorization",
    "cookie",
    "email",
    "phone",
    "apikey",
    "publishablekey",
    "privatekey",
    "accesskey",
    "signingkey",
    "idempotencykey",
];

fn env_is_secret(name: &str) -> bool {
    let folded: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    ENV_SECRET_PARTS.iter().any(|part| folded.contains(part))
}

/// The environment a capsule carries: the whole block minus secret shaped
/// names, minus the tool's own control variables, which replay sets itself.
fn capture_env() -> BTreeMap<String, String> {
    std::env::vars()
        .filter(|(name, _)| !name.starts_with("REPROIT_"))
        .filter(|(name, _)| !env_is_secret(name))
        .collect()
}
/// The determinism envelope, in the SAME shape the backend SDKs emit as their
/// `determinism-envelope` checkpoint. One envelope contract across every
/// capture kind is what lets a single reader pin a replay's clock, timezone
/// and seed without asking which capture produced it. `imageDigest` is
/// carried only when the environment states one, because a field a capture
/// cannot know must be ABSENT rather than guessed.
fn determinism_envelope(seed: &str) -> Value {
    let mut envelope = serde_json::Map::new();
    envelope.insert(
        "observedAtMs".into(),
        json!(chrono::Utc::now().timestamp_millis()),
    );
    envelope.insert("tz".into(), json!(std::env::var("TZ").unwrap_or_default()));
    envelope.insert("os".into(), json!(std::env::consts::OS));
    envelope.insert("arch".into(), json!(std::env::consts::ARCH));
    envelope.insert("replaySeed".into(), json!(seed));
    if let Ok(digest) = std::env::var("REPROIT_IMAGE_DIGEST") {
        if !digest.is_empty() {
            envelope.insert("imageDigest".into(), json!(digest));
        }
    }
    Value::Object(envelope)
}

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
    /// The normalized assertion or panic line the program printed when it
    /// died, when it declared one. Absent for a program that died silently,
    /// where the signal is the whole story.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
    pub outcome: Outcome,
    /// The shim's tab separated boundary log, one entry per line.
    pub entries: Vec<String>,
    #[serde(default)]
    pub truncated_entries: usize,
    /// Additive, versioned anchor section (anchor.rs for the application
    /// kind, workflows/checkpoint.rs for the criu kind). A capsule without
    /// one loads and replays exactly as before; with one, the boundary log
    /// covers the tail from the anchor forward only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<Value>,
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

/// Resolve a command word to a file, through PATH when it carries no slash,
/// so the guard below judges the binary that will actually run.
fn which_program(program: &str) -> Result<std::path::PathBuf> {
    let direct = Path::new(program);
    if program.contains('/') {
        return Ok(direct.to_path_buf());
    }
    let path = std::env::var("PATH").unwrap_or_default();
    for directory in path.split(':').filter(|entry| !entry.is_empty()) {
        let candidate = Path::new(directory).join(program);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    bail!("{program} is not on PATH")
}

/// Whether an ELF executable is dynamically linked, read from its program
/// headers. A statically linked program resolves no dynamic symbols, so the
/// shim's entry points are never called and the boundary sees nothing at all.
/// That must be a refusal to capture, never a capsule of nothing that would
/// later replay as a silent success.
///
/// `None` means "not an ELF we can judge" (a script, a wrapper, another
/// format), which is not evidence of static linking and so never refuses.
fn elf_is_dynamic(path: &Path) -> Option<bool> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() < 64 || &bytes[0..4] != b"\x7fELF" {
        return None;
    }
    let sixty_four = bytes[4] == 2;
    let little = bytes[5] == 1;
    if !sixty_four {
        // A 32 bit ELF is judged by its PT_INTERP too, but this tool's
        // supported targets are 64 bit; saying nothing beats guessing.
        return None;
    }
    let word = |offset: usize| -> Option<u64> {
        let slice = bytes.get(offset..offset + 8)?;
        let mut raw = [0u8; 8];
        raw.copy_from_slice(slice);
        Some(if little {
            u64::from_le_bytes(raw)
        } else {
            u64::from_be_bytes(raw)
        })
    };
    let half = |offset: usize| -> Option<u16> {
        let slice = bytes.get(offset..offset + 2)?;
        let mut raw = [0u8; 2];
        raw.copy_from_slice(slice);
        Some(if little {
            u16::from_le_bytes(raw)
        } else {
            u16::from_be_bytes(raw)
        })
    };
    let program_headers = word(0x20)? as usize;
    let entry_size = half(0x36)? as usize;
    let entry_count = half(0x38)? as usize;
    const PT_INTERP: u32 = 3;
    for index in 0..entry_count {
        let offset = program_headers + index * entry_size;
        let slice = bytes.get(offset..offset + 4)?;
        let mut raw = [0u8; 4];
        raw.copy_from_slice(slice);
        let kind = if little {
            u32::from_le_bytes(raw)
        } else {
            u32::from_be_bytes(raw)
        };
        if kind == PT_INTERP {
            return Some(true);
        }
    }
    Some(false)
}

/// The first statically linked image the recording saw the process exec into,
/// if any. The shim records one `exec` entry per new image and only for the
/// static ones, so the presence of the line is the whole signal.
fn static_exec(entries: &[String]) -> Option<&str> {
    entries
        .iter()
        .filter_map(|line| line.strip_prefix("exec\t"))
        .filter_map(|rest| rest.split('\t').next())
        .find(|image| !image.is_empty() && *image != "-")
}

pub(super) fn hex_digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(&mut encoded, "{byte:02x}");
    }
    encoded
}

fn digest_of(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    Some(format!("sha256:{}", hex_digest(&bytes)))
}

/// An anchored capture's request: the checkpoint artifact the command
/// resumes from, and the position it corresponds to. Both come from the
/// operator's argv, never from a capsule.
pub struct AnchorRequest {
    pub checkpoint: std::path::PathBuf,
    pub position: u64,
}

/// `reproit internal process-capture --out <capsule> -- <command>`: run the
/// command under the recording shim and assemble a capsule from what the
/// boundary saw plus how the program died. With `--anchor-checkpoint`, the
/// command must be the program's own resume invocation; the capsule then
/// carries the checkpoint artifact as an application anchor and its boundary
/// log covers the tail only, from the anchor forward.
pub fn capture(
    ctx: &Ctx,
    out: &Path,
    command: &[String],
    anchor_request: Option<&AnchorRequest>,
) -> Result<ExitCode> {
    let Some((program, arguments)) = command.split_first() else {
        bail!("process capture needs a command to run");
    };
    // Built BEFORE the program runs, so the digest describes the artifact the
    // program is about to load rather than whatever the run leaves behind.
    let application_anchor = anchor_request
        .map(|request| anchor::build(&request.checkpoint, request.position))
        .transpose()?;
    let shim = shim_path()?;
    // Refuse a target the boundary cannot see, BEFORE running it. A static
    // binary would produce a capsule of nothing, and a capsule of nothing
    // replays as a silent success.
    if let Ok(resolved) = which_program(program) {
        if elf_is_dynamic(&resolved) == Some(false) {
            bail!(
                "{} is statically linked, so no dynamic symbol resolution happens and the process \
                 boundary observes nothing. Capturing it would produce an empty capsule that \
                 replays as a false success. Rebuild the subject dynamically, or wait for the \
                 syscall-only capture path",
                resolved.display()
            );
        }
    }
    let log = tempfile_path("record");
    let seed = format!("{:016x}", rand_seed());
    // stderr is piped rather than inherited so the program's own failure text
    // can be recorded as the capsule's failure identity, and echoed as it
    // arrives so the operator still sees the run exactly as before.
    let mut child = std::process::Command::new(program)
        .args(arguments)
        .env(preload_var(), &shim)
        .env("REPROIT_RECORD", &log)
        .env("REPROIT_REPLAY_SEED", &seed)
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawn {program}"))?;
    let recorded_stderr = Arc::new(Mutex::new(Vec::<String>::new()));
    let reader = child.stderr.take().map(|stderr| {
        let sink = Arc::clone(&recorded_stderr);
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(stderr)
                .lines()
                .map_while(Result::ok)
            {
                eprintln!("{line}");
                if let Ok(mut lines) = sink.lock() {
                    if lines.len() < MAX_STDERR_LINES {
                        lines.push(line);
                    }
                }
            }
        })
    });
    let status = child.wait()?;
    if let Some(reader) = reader {
        let _ = reader.join();
    }
    let failure = failure_signature(
        &recorded_stderr
            .lock()
            .map_err(|_| anyhow::anyhow!("stderr reader panicked"))?
            .clone(),
    );

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
    // A program that ran and read nothing at all did not have its boundary
    // observed; the shim failed to load. Refuse rather than write a capsule
    // whose replay would trivially "succeed" because it serves nothing.
    if entries.is_empty() {
        bail!(
            "the boundary observed nothing while {program} ran, so the shim was not loaded (a \
             static binary, or a loader that dropped the preload). Refusing to write a capsule \
             that would replay as a false success"
        );
    }
    // The launched command was judged dynamic before it ran, but a program is
    // free to exec into something else. A seccomp filter survives execve while
    // LD_PRELOAD does not reach a static image at all, so the capture would
    // hold real file entries and silently no clock, randomness, environment,
    // or socket traffic. That capsule looks complete and is not.
    if let Some(image) = static_exec(&entries) {
        bail!(
            "{program} exec'd into {image}, which is statically linked. The syscall layer still \
             sees its files, but the libc layer sees none of its clock, randomness, environment, \
             or socket traffic, so this capture is INCOMPLETE in classes it cannot report. \
             Refusing to write a capsule that would look complete. Rebuild that image \
             dynamically, or capture it directly so the refusal happens before it runs"
        );
    }
    let env = capture_env();
    let capsule = ProcessCapsule {
        format: FORMAT.to_string(),
        version: VERSION,
        command: command.to_vec(),
        cwd: std::env::current_dir()?.display().to_string(),
        executable_sha256: digest_of(Path::new(program)),
        env,
        envelope: determinism_envelope(&seed),
        // A declared assertion or panic is a stronger identity than the
        // signal: every failed assert dies with SIGABRT, so the signal alone
        // cannot tell two of them apart.
        oracle: if failure.is_some() {
            "process-assertion".to_string()
        } else if outcome.signal.is_some() {
            "process-signal".to_string()
        } else {
            "process-exit".to_string()
        },
        failure,
        outcome,
        entries,
        truncated_entries: truncated,
        anchor: application_anchor
            .as_ref()
            .map(serde_json::to_value)
            .transpose()?,
    };
    std::fs::write(out, serde_json::to_vec_pretty(&capsule)?)?;
    ctx.emit(&json!({
        "command": "process capture",
        "capsule": out.display().to_string(),
        "entries": capsule.entries.len(),
        "truncated": capsule.truncated_entries,
        "oracle": capsule.oracle,
        "outcome": capsule.outcome,
        "anchor": capsule.anchor,
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
    if let Some(anchored) = &application_anchor {
        ctx.say(format!(
            "  anchor:           application checkpoint at {} {} ({})",
            anchored.position.unit, anchored.position.ordinal, anchored.checkpoint_sha256
        ));
        ctx.say("  the boundary log covers the tail from the anchor forward only");
    }
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
    /// Every non marker stderr line, kept so the replay's failure identity can
    /// be compared with the recording's. Bounded: a program that dies in a
    /// loop must not be able to grow this without limit.
    stderr: Vec<String>,
}

fn watch(child: &mut std::process::Child) -> Arc<Mutex<ReplayObservation>> {
    let sink = Arc::new(Mutex::new(ReplayObservation {
        divergences: Vec::new(),
        counters: None,
        stderr: Vec::new(),
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
                    if let Ok(mut observation) = sink.lock() {
                        if observation.stderr.len() < MAX_STDERR_LINES {
                            observation.stderr.push(line.clone());
                        }
                    }
                    eprintln!("{line}");
                }
            }
        });
    }
    sink
}

/// What one hermetic re-execution of a capsule produced.
pub(super) struct Replay {
    pub verdict: HermeticVerdict,
    observed: Outcome,
    observed_failure: Option<String>,
    divergences: Vec<Value>,
    counters: Option<Counters>,
    /// Why the run was not attempted at all. An abstention is INCONCLUSIVE
    /// with a named cause, never a pass and never a reproduction.
    abstained: Option<String>,
}

/// Re-exec the program under the serving shim, with the capsule's working
/// directory, environment, and seed pinned, and judge how it died.
pub(super) async fn replay(capsule: &ProcessCapsule, command: &str) -> Result<Replay> {
    let abstain = |reason: String| Replay {
        verdict: HermeticVerdict::Inconclusive,
        observed: Outcome {
            exit_code: None,
            signal: None,
        },
        observed_failure: None,
        divergences: Vec::new(),
        counters: None,
        abstained: Some(reason),
    };
    let shim = shim_path()?;
    // Relative paths are keyed against the working directory at BOTH record
    // and replay, so replaying from somewhere else would ask the capsule
    // about files it never saw. A guard kept in a repo is checked from the
    // repo root, so the directory has to come from the capsule rather than
    // from wherever the operator happens to stand.
    let cwd = Path::new(&capsule.cwd);
    if !cwd.is_dir() {
        return Ok(abstain(format!(
            "the recorded working directory {} no longer exists, and a program's relative paths \
             cannot resolve the same way from anywhere else",
            capsule.cwd
        )));
    }
    let log = tempfile_path("replay");
    std::fs::write(&log, capsule.entries.join("\n") + "\n")?;

    let seed = capsule
        .envelope
        .get("replaySeed")
        .and_then(Value::as_str)
        .unwrap_or("c0ffee00c0ffee00")
        .to_string();
    // Restore the recorded environment as the WHOLE block, not as additions
    // to the developer's shell. An interpreter decides where its prefix and
    // its locale live from this block, so an inherited variable the recording
    // did not have sends replay down a different path and diverges.
    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(cwd)
        .env_clear()
        .envs(capsule.env.clone())
        .env(preload_var(), &shim)
        .env("REPROIT_REPLAY_LOG", &log)
        .env("REPROIT_REPLAY_SEED", &seed)
        // The block above is authoritative, so the shim must not serve a
        // stale getenv snapshot over it.
        .env("REPROIT_ENV_PINNED", "1")
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
    let (divergences, counters, observed_failure) = {
        let guard = observation
            .lock()
            .map_err(|_| anyhow::anyhow!("stderr reader panicked"))?;
        (
            guard.divergences.clone(),
            guard.counters,
            failure_signature(&guard.stderr),
        )
    };
    // A capsule that recorded a DECLARED failure (an assertion or a panic)
    // demands the same one back. Without this, two unrelated assertions both
    // die with SIGABRT and the outcome comparison alone calls the second a
    // reproduction of the first, which is a false proof in the one direction
    // this product must never get wrong.
    let failure_matches = match (&capsule.failure, &observed_failure) {
        (Some(recorded), Some(seen)) => recorded == seen,
        (Some(_), None) => false,
        (None, _) => true,
    };
    let verdict = if !divergences.is_empty() {
        HermeticVerdict::Diverged
    } else if observed.same_as(&capsule.outcome) && failure_matches {
        HermeticVerdict::Reproduced
    } else if !observed.failed() {
        HermeticVerdict::Fixed
    } else {
        // A different failure is not this failure. Fails closed rather than
        // claiming a reproduction the capsule does not describe.
        HermeticVerdict::Inconclusive
    };
    Ok(Replay {
        verdict,
        observed,
        observed_failure,
        divergences,
        counters,
        abstained: None,
    })
}

/// `reproit check <capsule> --exec "<command>"`: replay the capsule and
/// report the four-way verdict. An application anchor is verified and its
/// checkpoint put back where the program loads it BEFORE the program runs;
/// any anchor problem is a named refusal, never a replay from zero.
pub async fn check_exec(ctx: &Ctx, file: &Path, command: &str, _auto: bool) -> Result<ExitCode> {
    let capsule = parse(file)?;
    let abstain = |reason: String| Replay {
        verdict: HermeticVerdict::Inconclusive,
        observed: Outcome {
            exit_code: None,
            signal: None,
        },
        observed_failure: None,
        divergences: Vec::new(),
        counters: None,
        abstained: Some(reason),
    };
    match anchor::from_capsule(capsule.anchor.as_ref()) {
        Err(refusal) => return Ok(report(ctx, file, &capsule, &abstain(refusal))),
        Ok(Some(anchored)) => {
            if let Err(refusal) = anchor::prepare(&anchored) {
                return Ok(report(ctx, file, &capsule, &abstain(refusal)));
            }
        }
        Ok(None) => {}
    }
    let outcome = replay(&capsule, command).await?;
    Ok(report(ctx, file, &capsule, &outcome))
}

/// Emit and print one replay's verdict, and return its exit code.
pub(super) fn report(
    ctx: &Ctx,
    file: &Path,
    capsule: &ProcessCapsule,
    replayed: &Replay,
) -> ExitCode {
    let Replay {
        verdict,
        observed,
        observed_failure,
        divergences,
        counters,
        abstained,
    } = replayed;
    let (verdict, observed, divergences, counters) = (*verdict, *observed, divergences, *counters);
    // The honesty bound travels with the verdict: the statement STORED in the
    // artifact (not a constant this build happens to agree with) is printed
    // verbatim whenever the capsule carries an application anchor.
    let statement = capsule
        .anchor
        .as_ref()
        .filter(|raw| raw.get("kind").and_then(Value::as_str) == Some("application"))
        .and_then(|raw| raw.get("uncontrolledSources"))
        .and_then(Value::as_str)
        .map(str::to_string);

    ctx.emit(&json!({
        "command": "check",
        "capsule": {
            "file": file.display().to_string(),
            "format": FORMAT,
            "mode": "process-hermetic",
            "anchor": capsule.anchor,
            "oracle": capsule.oracle,
            "verdict": verdict.as_str(),
            "recordedOutcome": capsule.outcome,
            "observedOutcome": observed,
            "recordedFailure": capsule.failure,
            "observedFailure": observed_failure,
            "divergences": divergences,
            "abstained": abstained,
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
            for report in divergences {
                ctx.say(format!("    {report}"));
            }
        }
        HermeticVerdict::Inconclusive => match abstained {
            Some(reason) => ctx.say(format!("  INCONCLUSIVE not replayed: {reason}")),
            None => ctx.say(format!(
                "  INCONCLUSIVE recorded {}, observed {}; failing closed",
                capsule.outcome.describe(),
                observed.describe()
            )),
        },
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
    if let Some(statement) = statement {
        ctx.say(format!("  {statement}"));
    }
    match verdict {
        HermeticVerdict::Fixed => ExitCode::SUCCESS,
        HermeticVerdict::Reproduced => Exit::Regression.code(),
        _ => ExitCode::from(3),
    }
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

pub mod anchor;
mod keep;
pub use keep::{keep_capsule_guard, try_replay_process_guard};

#[cfg(test)]
mod tests;
