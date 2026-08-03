//! Checkpoint anchoring for long-running programs (Class C).
//!
//! A process capsule replays a program from its first instruction. That is
//! correct and useless for a failure at minute 340 of a six hour run: nobody
//! waits six hours to look at a bug twice. An ANCHOR is a checkpoint of the
//! replaying process plus the position it had reached, so a later replay
//! restores the anchor and re-executes only the tail.
//!
//! Where the checkpoint is taken matters, and the survey settled it. A
//! checkpoint cannot be taken of the ORIGINAL run, because CRIU refuses a
//! process holding an established TCP connection (measured: "Failed to lock
//! TCP connection"), which is exactly what a long-running server or trainer
//! holds. It CAN be taken of the REPLAYING process, because under the shim
//! every socket is served from the capsule and no live connection exists. So
//! the anchor is a by-product of one slow replay, and every replay after it is
//! fast.
//!
//! The entry cursor is not a number this module tracks. The replaying process
//! holds the shim's cursor in its own memory, so a checkpoint of that process
//! carries the cursor with it. What the anchor records instead is the
//! OBSERVABLE position (how far the program's own output had got) for a human
//! reading the file, plus two digests that make a mismatched pairing
//! impossible.
//!
//! What an anchor is NOT: valid across a code change. A CRIU image contains
//! the program's memory, including its code, so restoring it re-runs the OLD
//! binary. An anchor accelerates INVESTIGATION of a failure, and must never be
//! used to verify a fix; verification replays from zero against the new
//! binary. `validate` refuses an anchor whose capsule or image digest moved,
//! and the fix-verification path never consults one.

use crate::interface::cli::context::{Ctx, Exit};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{ExitCode, Stdio};
use std::time::{Duration, Instant};

use crate::workflows::backend_headless::HermeticVerdict;

/// The capsule field this module adds. Additive: a capsule without it is a
/// perfectly valid capsule that simply has no fast path.
const ANCHOR_FIELD: &str = "anchor";
const ANCHOR_KIND_CRIU: &str = "criu";
/// A restore that never completes must fail closed rather than hang a gate.
const RESTORE_TIMEOUT: Duration = Duration::from_secs(120);
/// How long to wait for the anchoring replay to reach its trigger before
/// giving up. Bounded for the same reason.
const TRIGGER_TIMEOUT: Duration = Duration::from_secs(600);
/// How long to wait for the restored tail to publish its outcome. Short on
/// purpose: the tail resumes in about a second, so a longer wait only delays
/// the honest "I could not observe this" that follows.
const STATUS_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Anchor {
    /// How the checkpoint was taken. Only `criu` today; an application level
    /// anchor (a program's own save file) is a different kind because it
    /// survives a rebuild, and the two must never be confused.
    pub kind: String,
    /// Directory holding the checkpoint image.
    pub image: String,
    /// Digest over the image contents, so a truncated or edited image is
    /// refused rather than restored into an unknown state.
    pub image_sha256: String,
    /// Digest of the capsule this anchor was taken from. An anchor restored
    /// against a different capsule would replay one program's memory against
    /// another program's boundary log, which is a false reproduction waiting
    /// to happen.
    pub capsule_sha256: String,
    /// Where the program had got to when the checkpoint was taken, in terms a
    /// human can read. The authoritative cursor is inside the image.
    pub progress: Value,
    /// The determinism envelope at the instant of the checkpoint.
    pub envelope: Value,
}

/// Why an anchor cannot be used. Every variant is a refusal, never a fallback
/// to something that looks like success.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnchorRefusal {
    Missing,
    UnknownKind(String),
    ImageAbsent(String),
    ImageDigestMoved { expected: String, actual: String },
    CapsuleDigestMoved { expected: String, actual: String },
}

impl AnchorRefusal {
    pub fn reason(&self) -> String {
        match self {
            AnchorRefusal::Missing => "the capsule carries no anchor".to_string(),
            AnchorRefusal::UnknownKind(kind) => {
                format!("anchor kind {kind:?} is not one this build can restore")
            }
            AnchorRefusal::ImageAbsent(path) => {
                format!("the checkpoint image {path} is not on disk")
            }
            AnchorRefusal::ImageDigestMoved { expected, actual } => format!(
                "the checkpoint image changed since the anchor was taken (expected {expected}, \
                 found {actual})"
            ),
            AnchorRefusal::CapsuleDigestMoved { expected, actual } => format!(
                "this anchor belongs to a different capsule (expected {expected}, found {actual})"
            ),
        }
    }
}

/// Read an anchor out of a capsule document, refusing anything that cannot be
/// restored faithfully. Pure over its inputs so the refusal rules are testable
/// without a checkpoint on disk.
pub fn read_anchor(capsule: &Value, capsule_digest: &str) -> Result<Anchor, AnchorRefusal> {
    let Some(raw) = capsule.get(ANCHOR_FIELD) else {
        return Err(AnchorRefusal::Missing);
    };
    let anchor: Anchor = serde_json::from_value(raw.clone()).map_err(|_| AnchorRefusal::Missing)?;
    if anchor.kind != ANCHOR_KIND_CRIU {
        return Err(AnchorRefusal::UnknownKind(anchor.kind));
    }
    if anchor.capsule_sha256 != capsule_digest {
        return Err(AnchorRefusal::CapsuleDigestMoved {
            expected: anchor.capsule_sha256.clone(),
            actual: capsule_digest.to_string(),
        });
    }
    Ok(anchor)
}

/// The second half of validation, which needs the filesystem: the image must
/// still be there and must still be the bytes the anchor was taken over.
pub fn validate_image(anchor: &Anchor) -> Result<(), AnchorRefusal> {
    let image = Path::new(&anchor.image);
    if !image.is_dir() {
        return Err(AnchorRefusal::ImageAbsent(anchor.image.clone()));
    }
    let actual =
        digest_directory(image).map_err(|_| AnchorRefusal::ImageAbsent(anchor.image.clone()))?;
    if actual != anchor.image_sha256 {
        return Err(AnchorRefusal::ImageDigestMoved {
            expected: anchor.image_sha256.clone(),
            actual,
        });
    }
    Ok(())
}

fn hex(bytes: impl AsRef<[u8]>) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(64);
    for byte in bytes.as_ref() {
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

pub fn digest_bytes(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("sha256:{}", hex(Sha256::digest(bytes)))
}

/// Digest a directory by folding each file's relative name and content in
/// sorted order, so the result is stable across filesystems and refuses a
/// missing or added file as loudly as an edited one.
pub fn digest_directory(root: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let mut files: Vec<PathBuf> = Vec::new();
    collect_files(root, &mut files)?;
    files.sort();
    let mut hasher = Sha256::new();
    for file in &files {
        let relative = file.strip_prefix(root).unwrap_or(file);
        hasher.update(relative.to_string_lossy().as_bytes());
        hasher.update([0u8]);
        let bytes = std::fs::read(file)
            .with_context(|| format!("read checkpoint image file {}", file.display()))?;
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(&bytes);
    }
    Ok(format!("sha256:{}", hex(hasher.finalize())))
}

fn collect_files(directory: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(directory)
        .with_context(|| format!("read directory {}", directory.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, out)?;
        } else if path.is_file() {
            out.push(path);
        }
    }
    Ok(())
}

fn read_capsule(path: &Path) -> Result<(Value, String)> {
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let value: Value =
        serde_json::from_slice(&bytes).context("file is not a process capsule payload")?;
    if value.get("format").and_then(Value::as_str) != Some("reproit-process-capsule") {
        bail!("{} is not a reproit-process-capsule", path.display());
    }
    // The digest binding an anchor to its capsule is taken over the capsule
    // WITHOUT its anchor field, so writing the anchor into the capsule does
    // not invalidate the digest the anchor just recorded.
    Ok((value.clone(), capsule_identity(&value)))
}

/// The capsule's identity for anchor binding: everything except the anchor
/// itself. Adding, changing, or removing an anchor must not move it, while any
/// change to the command, environment, envelope, outcome, or boundary log must.
pub fn capsule_identity(capsule: &Value) -> String {
    let mut copy = capsule.clone();
    if let Some(object) = copy.as_object_mut() {
        object.remove(ANCHOR_FIELD);
    }
    digest_bytes(serde_json::to_string(&copy).unwrap_or_default().as_bytes())
}

/// The shim library, from repo-local configuration only. A capsule can never
/// name a library to load, exactly as it can never supply a command.
fn shim_path() -> Result<String> {
    if let Ok(path) = std::env::var("REPROIT_PROCESS_SHIM") {
        if Path::new(&path).is_file() {
            return Ok(path);
        }
    }
    bail!(
        "no process shim is available. Build runners/process-shim for this platform and set \
         REPROIT_PROCESS_SHIM to the resulting library"
    )
}

fn preload_var() -> &'static str {
    if cfg!(target_os = "macos") {
        "DYLD_INSERT_LIBRARIES"
    } else {
        "LD_PRELOAD"
    }
}

fn criu_available() -> bool {
    std::process::Command::new("criu")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn env_block(capsule: &Value) -> Vec<(String, String)> {
    capsule
        .get("env")
        .and_then(Value::as_object)
        .map(|object| {
            object
                .iter()
                .filter_map(|(name, value)| {
                    value.as_str().map(|text| (name.clone(), text.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn replay_seed(capsule: &Value) -> String {
    capsule
        .get("envelope")
        .and_then(|envelope| envelope.get("replaySeed"))
        .and_then(Value::as_str)
        .unwrap_or("c0ffee00c0ffee00")
        .to_string()
}

/// Write the capsule's boundary log to a file the shim can read at replay.
fn write_replay_log(capsule: &Value, path: &Path) -> Result<()> {
    let entries: Vec<String> = capsule
        .get("entries")
        .and_then(Value::as_array)
        .map(|array| {
            array
                .iter()
                .filter_map(|entry| entry.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    std::fs::write(path, entries.join("\n") + "\n")?;
    Ok(())
}

/// `reproit process-anchor`: replay the capsule once, checkpoint the
/// replaying process when it reaches the trigger, and record the anchor in the
/// capsule. The slow replay is paid once so later ones are fast.
pub fn anchor(
    ctx: &Ctx,
    capsule_path: &Path,
    command: &str,
    image: &Path,
    after_lines: usize,
) -> Result<ExitCode> {
    if !criu_available() {
        bail!(
            "criu is not available on this machine, so a process checkpoint cannot be taken. \
             Anchoring is a Linux capability and needs criu plus the privileges to use it"
        );
    }
    let (capsule, identity) = read_capsule(capsule_path)?;
    let shim = shim_path()?;
    let log = temp_path("anchor-replay");
    write_replay_log(&capsule, Path::new(&log))?;
    std::fs::create_dir_all(image)
        .with_context(|| format!("create checkpoint image directory {}", image.display()))?;

    // The image references this file by path, so it must OUTLIVE the anchoring
    // run: a restored process reopens its own fd 1, and a deleted stdout makes
    // criu restore fail immediately. It lives beside the image rather than
    // inside it, because the restored tail appends to it and that would move
    // the image digest the anchor was taken over.
    let progress = format!("{}-stdout.log", image.display());
    let status_file = format!("{}-status", image.display());
    let _ = std::fs::remove_file(&status_file);
    let progress_file = std::fs::File::create(&progress)?;
    // MEASURED: criu restore exits 0 even when the restored task dies on a
    // fatal signal, so its exit code cannot be the oracle. Instead the
    // anchored process is a SHELL that writes its own child's status once the
    // child ends. The trailing command also forces sh to fork rather than
    // exec the program directly, so the checkpoint holds the shell as well and
    // something is still alive after the tail dies to record how it died.
    let wrapped = format!("{command}; echo $? > {status_file}");
    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg(&wrapped)
        .env_clear()
        .envs(env_block(&capsule))
        .env(preload_var(), &shim)
        .env("REPROIT_REPLAY_LOG", &log)
        .env("REPROIT_REPLAY_SEED", replay_seed(&capsule))
        .env("REPROIT_ENV_PINNED", "1")
        // MEASURED constraint, not a preference: criu refuses to dump a
        // process holding a seccomp notify descriptor ("Can't dump file 3 of
        // that type [600] (anon anon_inode:seccomp notify)"), which is exactly
        // what the completeness layer installs. Anchoring therefore runs on
        // the libc-only boundary, and the capsule must have been captured the
        // same way or the two boundaries would key their entries differently.
        .env("REPROIT_SECCOMP", "0")
        .stdout(Stdio::from(progress_file))
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("spawn --exec command {command:?}"))?;

    let reached = wait_for_lines(&progress, after_lines, TRIGGER_TIMEOUT);
    if !reached {
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_file(&log);
        let _ = std::fs::remove_file(&progress);
        bail!(
            "the replay did not reach {after_lines} lines of output within {}s, so there is no \
             anchor point to checkpoint",
            TRIGGER_TIMEOUT.as_secs()
        );
    }
    let observed_lines = count_lines(&progress);
    let dumped_pid = child.id();
    let dump = std::process::Command::new("criu")
        .arg("dump")
        .arg("-t")
        .arg(child.id().to_string())
        .arg("-D")
        .arg(image)
        .arg("--shell-job")
        .arg("-o")
        .arg("dump.log")
        .status();
    let dumped = matches!(dump, Ok(status) if status.success());
    if !dumped {
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_file(&log);
        let _ = std::fs::remove_file(&progress);
        bail!(
            "criu could not checkpoint the replaying process. Its log is at {}/dump.log. A \
             process holding an established TCP connection cannot be checkpointed, and neither \
             can one whose files live on a mount criu cannot resolve",
            image.display()
        );
    }

    let image_digest = digest_directory(image)?;
    let anchor = Anchor {
        kind: ANCHOR_KIND_CRIU.to_string(),
        image: image.display().to_string(),
        image_sha256: image_digest,
        capsule_sha256: identity,
        progress: json!({
            "stdoutLines": observed_lines,
            "requestedLines": after_lines,
            "stdout": progress,
            "status": status_file,
            "pid": dumped_pid,
            "note": "observable position only; the authoritative boundary cursor is inside the image",
        }),
        envelope: json!({
            "observedAtMs": chrono::Utc::now().timestamp_millis(),
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "replaySeed": replay_seed(&capsule),
        }),
    };

    let mut updated = capsule.clone();
    if let Some(object) = updated.as_object_mut() {
        object.insert(ANCHOR_FIELD.to_string(), serde_json::to_value(&anchor)?);
    }
    std::fs::write(capsule_path, serde_json::to_vec_pretty(&updated)?)?;
    let _ = std::fs::remove_file(&log);

    ctx.emit(&json!({
        "command": "process anchor",
        "capsule": capsule_path.display().to_string(),
        "anchor": anchor,
    }));
    ctx.say(format!(
        "Anchored {} at {} lines of output",
        capsule_path.display(),
        observed_lines
    ));
    ctx.say(format!("  image:  {}", anchor.image));
    ctx.say(format!("  digest: {}", anchor.image_sha256));
    ctx.say("  boundary: libc only, because a seccomp notify descriptor cannot be checkpointed");
    ctx.say("  an anchor accelerates investigating this failure; verifying a fix still replays from zero");
    Ok(ExitCode::SUCCESS)
}

fn count_lines(path: &str) -> usize {
    std::fs::read_to_string(path)
        .map(|text| text.lines().count())
        .unwrap_or(0)
}

fn wait_for_lines(path: &str, target: usize, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if count_lines(path) >= target {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

/// `reproit process-restore`: restore the capsule's anchor and let
/// the tail run, then judge the same four way verdict a full replay judges.
/// Refuses, loudly, anything it cannot restore faithfully.
pub fn restore(ctx: &Ctx, capsule_path: &Path) -> Result<ExitCode> {
    let (capsule, identity) = read_capsule(capsule_path)?;
    let anchor = match read_anchor(&capsule, &identity) {
        Ok(anchor) => anchor,
        Err(refusal) => return refuse(ctx, capsule_path, &refusal.reason()),
    };
    if let Err(refusal) = validate_image(&anchor) {
        return refuse(ctx, capsule_path, &refusal.reason());
    }
    if !criu_available() {
        return refuse(ctx, capsule_path, "criu is not available on this machine");
    }

    let status_path = anchor
        .progress
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if status_path.is_empty() {
        return refuse(
            ctx,
            capsule_path,
            "this anchor predates status recording and cannot report how its tail ended",
        );
    }
    // A stale status from an earlier restore would be read as this one's.
    let _ = std::fs::remove_file(&status_path);

    let started = Instant::now();
    let mut child = std::process::Command::new("criu")
        .arg("restore")
        .arg("-D")
        .arg(&anchor.image)
        .arg("--shell-job")
        // MEASURED: without this the restored tree is a child of criu, and
        // criu exiting takes the shell with it. The program itself survives as
        // an orphan and finishes, so its output looks complete while nothing
        // is left to record HOW it ended. Detaching keeps the whole tree alive
        // so the shell can publish its child's status.
        .arg("--restore-detached")
        .arg("-o")
        .arg("restore.log")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("spawn criu restore")?;

    let restore_status = wait_bounded(&mut child, RESTORE_TIMEOUT);
    let Some(restore_status) = restore_status else {
        let _ = child.kill();
        let _ = child.wait();
        return refuse(
            ctx,
            capsule_path,
            &format!(
                "criu restore did not return within {}s",
                RESTORE_TIMEOUT.as_secs()
            ),
        );
    };
    if !restore_status.success() {
        return refuse(
            ctx,
            capsule_path,
            "criu could not restore the checkpoint image",
        );
    }
    // The restored shell writes its child's status once the tail ends. Waiting
    // for that file is what makes the outcome observable at all, given criu
    // reports only its own success.
    let Some(observed_exit) = wait_for_status(&status_path, STATUS_TIMEOUT) else {
        return refuse(
            ctx,
            capsule_path,
            &format!(
                "the restored tail did not report an outcome within {}s. It resumes and runs, \
                 but criu reports only its own success and the shell that would publish the \
                 tail's status does not survive the restore in a reportable way",
                STATUS_TIMEOUT.as_secs()
            ),
        );
    };
    let elapsed = started.elapsed();

    let recorded_exit = capsule
        .get("outcome")
        .and_then(|outcome| outcome.get("exitCode"))
        .and_then(Value::as_i64)
        .map(|code| code as i32);
    let recorded_signal = capsule
        .get("outcome")
        .and_then(|outcome| outcome.get("signal"))
        .and_then(Value::as_i64)
        .map(|code| code as i32);
    let verdict = judge(recorded_exit, recorded_signal, Some(observed_exit));

    ctx.emit(&json!({
        "command": "process restore",
        "capsule": capsule_path.display().to_string(),
        "mode": "anchored",
        "verdict": verdict.as_str(),
        "elapsedMs": elapsed.as_millis() as u64,
        "anchor": anchor,
        "recordedOutcome": { "exitCode": recorded_exit, "signal": recorded_signal },
        "observedOutcome": { "exitCode": observed_exit },
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
        "restore capsule {} (anchored, {} ms)",
        capsule_path.display(),
        elapsed.as_millis()
    ));
    match verdict {
        HermeticVerdict::Reproduced => {
            ctx.say("  FAIL reproduced from the anchor, without replaying the head")
        }
        HermeticVerdict::Fixed => ctx.say("  PASS the restored tail exited cleanly"),
        _ => ctx.say("  INCONCLUSIVE the restored tail did not end how the recording did"),
    }
    Ok(match verdict {
        HermeticVerdict::Fixed => ExitCode::SUCCESS,
        HermeticVerdict::Reproduced => Exit::Regression.code(),
        _ => ExitCode::from(3),
    })
}

/// Every refusal takes this path, so a capsule that cannot be restored is
/// always reported as inconclusive with a named reason and never as a pass.
fn refuse(ctx: &Ctx, capsule_path: &Path, reason: &str) -> Result<ExitCode> {
    ctx.emit(&json!({
        "command": "process restore",
        "capsule": capsule_path.display().to_string(),
        "mode": "anchored",
        "verdict": HermeticVerdict::Inconclusive.as_str(),
        "reason": reason,
        "outcome": "stale",
        "exit": 3u8,
    }));
    ctx.say(format!(
        "restore capsule {} (anchored)",
        capsule_path.display()
    ));
    ctx.say(format!("  INCONCLUSIVE {reason}; failing closed"));
    Ok(ExitCode::from(3))
}

/// Wait for the restored shell to publish its child's exit status. A shell
/// reports a fatal signal as 128 plus the signal number, which `judge` folds
/// back so both spellings of one death compare equal.
fn wait_for_status(path: &str, timeout: Duration) -> Option<i32> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Ok(code) = text.trim().parse::<i32>() {
                return Some(code);
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    None
}

fn wait_bounded(
    child: &mut std::process::Child,
    timeout: Duration,
) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) => {
                if Instant::now() >= deadline {
                    return None;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => return None,
        }
    }
}

/// The same four way vocabulary a full replay uses. A restored tail that ends
/// differently from the recording is INCONCLUSIVE, never a reproduction.
pub fn judge(
    recorded_exit: Option<i32>,
    recorded_signal: Option<i32>,
    observed_exit: Option<i32>,
) -> HermeticVerdict {
    let canonical = |exit: Option<i32>, signal: Option<i32>| -> Option<i32> {
        if let Some(signal) = signal {
            return Some(signal);
        }
        match exit {
            Some(code) if (129..=192).contains(&code) => Some(code - 128),
            _ => None,
        }
    };
    let recorded_as_signal = canonical(recorded_exit, recorded_signal);
    let observed_as_signal = canonical(observed_exit, None);
    let same = match (recorded_as_signal, observed_as_signal) {
        (Some(left), Some(right)) => left == right,
        (None, None) => recorded_exit == observed_exit,
        _ => false,
    };
    if same {
        return HermeticVerdict::Reproduced;
    }
    match observed_exit {
        Some(0) => HermeticVerdict::Fixed,
        _ => HermeticVerdict::Inconclusive,
    }
}

fn temp_path(kind: &str) -> String {
    let stamp = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
    std::env::temp_dir()
        .join(format!(
            "reproit-checkpoint-{kind}-{}-{stamp}.log",
            std::process::id()
        ))
        .display()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let stamp = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
        let path = std::env::temp_dir().join(format!(
            "reproit-checkpoint-test-{name}-{}-{stamp}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn capsule_with(anchor: Option<Value>) -> Value {
        let mut capsule = json!({
            "format": "reproit-process-capsule",
            "version": 1,
            "command": ["./subject"],
            "entries": ["open\t/etc/config\t-\t0\t0\t0"],
            "outcome": { "exitCode": 3 },
        });
        if let Some(anchor) = anchor {
            capsule
                .as_object_mut()
                .unwrap()
                .insert(ANCHOR_FIELD.to_string(), anchor);
        }
        capsule
    }

    #[test]
    fn a_capsule_without_an_anchor_is_refused_not_guessed() {
        let capsule = capsule_with(None);
        let identity = capsule_identity(&capsule);
        let refusal = read_anchor(&capsule, &identity).unwrap_err();
        assert_eq!(refusal, AnchorRefusal::Missing);
        assert!(refusal.reason().contains("no anchor"));
    }

    #[test]
    fn writing_an_anchor_does_not_move_the_capsule_identity() {
        // The anchor records the digest of the capsule it was taken from, and
        // is then written INTO that capsule. If the anchor were part of the
        // identity, every anchor would invalidate itself the moment it landed.
        let bare = capsule_with(None);
        let identity = capsule_identity(&bare);
        let anchored = capsule_with(Some(json!({
            "kind": "criu",
            "image": "/tmp/img",
            "imageSha256": "sha256:aa",
            "capsuleSha256": identity,
            "progress": {},
            "envelope": {},
        })));
        assert_eq!(capsule_identity(&anchored), identity);
        assert!(read_anchor(&anchored, &identity).is_ok());
    }

    #[test]
    fn an_anchor_from_a_different_capsule_is_refused() {
        let anchored = capsule_with(Some(json!({
            "kind": "criu",
            "image": "/tmp/img",
            "imageSha256": "sha256:aa",
            "capsuleSha256": "sha256:someone-elses-capsule",
            "progress": {},
            "envelope": {},
        })));
        let identity = capsule_identity(&anchored);
        let refusal = read_anchor(&anchored, &identity).unwrap_err();
        assert!(matches!(refusal, AnchorRefusal::CapsuleDigestMoved { .. }));
        assert!(refusal.reason().contains("different capsule"));
    }

    #[test]
    fn an_edited_boundary_log_invalidates_its_anchor() {
        // A tampered capsule must not restore an anchor taken over the
        // untampered one, because the memory image and the log would then
        // describe two different runs.
        let bare = capsule_with(None);
        let identity = capsule_identity(&bare);
        let mut tampered = bare.clone();
        tampered.as_object_mut().unwrap().insert(
            "entries".to_string(),
            json!(["open\t/etc/other\t-\t0\t0\t0"]),
        );
        tampered.as_object_mut().unwrap().insert(
            ANCHOR_FIELD.to_string(),
            json!({
                "kind": "criu",
                "image": "/tmp/img",
                "imageSha256": "sha256:aa",
                "capsuleSha256": identity,
                "progress": {},
                "envelope": {},
            }),
        );
        let moved = capsule_identity(&tampered);
        assert_ne!(moved, identity);
        assert!(matches!(
            read_anchor(&tampered, &moved).unwrap_err(),
            AnchorRefusal::CapsuleDigestMoved { .. }
        ));
    }

    #[test]
    fn an_unknown_anchor_kind_is_refused_rather_than_attempted() {
        // An application level save survives a rebuild and a criu image does
        // not, so a build that cannot tell them apart must refuse.
        let capsule = capsule_with(Some(json!({
            "kind": "application-save",
            "image": "/tmp/img",
            "imageSha256": "sha256:aa",
            "capsuleSha256": "sha256:x",
            "progress": {},
            "envelope": {},
        })));
        let identity = capsule_identity(&capsule);
        let refusal = read_anchor(&capsule, &identity).unwrap_err();
        assert!(matches!(refusal, AnchorRefusal::UnknownKind(_)));
    }

    #[test]
    fn a_missing_or_edited_image_is_refused() {
        let directory = scratch("image");
        std::fs::write(directory.join("core-1.img"), b"checkpoint").unwrap();
        let digest = digest_directory(&directory).unwrap();
        let mut anchor = Anchor {
            kind: ANCHOR_KIND_CRIU.to_string(),
            image: directory.display().to_string(),
            image_sha256: digest.clone(),
            capsule_sha256: "sha256:x".to_string(),
            progress: json!({}),
            envelope: json!({}),
        };
        assert!(validate_image(&anchor).is_ok());

        // An edited image must be refused, not restored into an unknown state.
        std::fs::write(directory.join("core-1.img"), b"tampered").unwrap();
        assert!(matches!(
            validate_image(&anchor).unwrap_err(),
            AnchorRefusal::ImageDigestMoved { .. }
        ));

        // An added file moves the digest too, so a partially written image is
        // refused rather than half restored.
        std::fs::write(directory.join("core-1.img"), b"checkpoint").unwrap();
        assert!(validate_image(&anchor).is_ok());
        std::fs::write(directory.join("extra.img"), b"more").unwrap();
        assert!(validate_image(&anchor).is_err());

        anchor.image = directory.join("absent").display().to_string();
        assert!(matches!(
            validate_image(&anchor).unwrap_err(),
            AnchorRefusal::ImageAbsent(_)
        ));
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn the_restored_tail_is_judged_on_the_same_four_way_vocabulary() {
        // Reproduced: the tail died how the recording died.
        assert_eq!(judge(Some(3), None, Some(3)), HermeticVerdict::Reproduced);
        // A shell reports a fatal signal as 128 + n, and both spellings of one
        // death must compare equal.
        assert_eq!(judge(None, Some(6), Some(134)), HermeticVerdict::Reproduced);
        // Fixed: the tail now exits cleanly.
        assert_eq!(judge(Some(3), None, Some(0)), HermeticVerdict::Fixed);
        // A DIFFERENT failure is not this failure, so it fails closed.
        assert_eq!(judge(Some(3), None, Some(9)), HermeticVerdict::Inconclusive);
        assert_eq!(judge(None, Some(6), Some(3)), HermeticVerdict::Inconclusive);
    }
}
