//! Web runner discovery, provisioning, integrity checks, and browser setup.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

/// GitHub repo whose releases carry the prebuilt `reproit-web-runner.tar.gz`.
const RELEASE_REPO: &str = "ReproIt/reproit";

/// Where the self-healed web runner lives: the OS data dir,
/// `<data>/reproit/web`. Linux: `$XDG_DATA_HOME` or `~/.local/share`; macOS:
/// `~/Library/Application Support`; Windows: `%LOCALAPPDATA%`. `install.sh`
/// provisions into this same path, so a scripted install and a runtime
/// self-heal converge on one location and never need `REPROIT_WEB_RUNNER_DIR`.
pub fn web_runner_data_dir() -> PathBuf {
    let base = if cfg!(target_os = "windows") {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"))
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
    };
    base.unwrap_or_else(|| PathBuf::from("."))
        .join("reproit/web")
}

/// The web runner's JS logic, EMBEDDED in the binary at compile time. This is
/// the fix for runner/binary skew: the heavy `node_modules` (Playwright) is
/// downloaded once, but the runner SCRIPTS always come from the binary, so the
/// runner logic is in lock-step with the binary no matter how it was installed
/// (`cargo install`, brew, install.sh) -- no stale cache, no
/// `REPROIT_WEB_RUNNER_DIR` needed.
const WEB_RUNNER_FILES: &[(&str, &str)] = &[
    (
        "runner.mjs",
        include_str!("../../../../runners/web/runner.mjs"),
    ),
    (
        "flicker-oracle.mjs",
        include_str!("../../../../runners/web/flicker-oracle.mjs"),
    ),
    (
        "choice-oracle.mjs",
        include_str!("../../../../runners/web/choice-oracle.mjs"),
    ),
    (
        "hygiene-oracles.mjs",
        include_str!("../../../../runners/web/hygiene-oracles.mjs"),
    ),
    (
        "probe.mjs",
        include_str!("../../../../runners/web/probe.mjs"),
    ),
    (
        "pw-capture.mjs",
        include_str!("../../../../runners/web/pw-capture.mjs"),
    ),
    (
        "annotate.mjs",
        include_str!("../../../../runners/web/annotate.mjs"),
    ),
    (
        "jank-oracle.mjs",
        include_str!("../../../../runners/web/jank-oracle.mjs"),
    ),
    ("jank.mjs", include_str!("../../../../runners/web/jank.mjs")),
    (
        "differential.mjs",
        include_str!("../../../../runners/web/differential.mjs"),
    ),
    (
        "box-overlay.mjs",
        include_str!("../../../../runners/web/box-overlay.mjs"),
    ),
    (
        "a2ui-runner.mjs",
        include_str!("../../../../runners/web/a2ui-runner.mjs"),
    ),
    (
        "a2ui-host.jsx",
        include_str!("../../../../runners/web/a2ui-host.jsx"),
    ),
    (
        "package.json",
        include_str!("../../../../runners/web/package.json"),
    ),
    (
        "package-lock.json",
        include_str!("../../../../runners/web/package-lock.json"),
    ),
];

/// Write the binary's embedded runner scripts into `dir`, overwriting any stale
/// copies, so the runner logic matches this binary exactly.
fn write_embedded_runner(dir: &Path) -> Result<()> {
    for (name, contents) in WEB_RUNNER_FILES {
        std::fs::write(dir.join(name), contents)
            .with_context(|| format!("writing embedded runner {name}"))?;
    }
    Ok(())
}

/// A DEV runner used verbatim (live edits, no embed sync): an explicit
/// `$REPROIT_WEB_RUNNER_DIR`, a source checkout at `./runners/web`, or the
/// binary's sibling. Must already carry `node_modules`. `None` falls through to
/// the self-provisioned data-dir runner.
fn find_dev_runner_dir() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(d) = std::env::var("REPROIT_WEB_RUNNER_DIR") {
        if !d.trim().is_empty() {
            candidates.push(PathBuf::from(d));
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("runners/web"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(p) = exe.parent() {
            candidates.push(p.join("runners/web"));
        }
    }
    candidates
        .into_iter()
        .find(|c| c.join("node_modules").is_dir())
}

/// Return a ready-to-use web runner dir. A dev/source runner is used verbatim;
/// otherwise the self-provisioned data-dir runner is used, downloading the
/// heavy `node_modules` (Playwright) once and then ALWAYS writing the binary's
/// embedded runner scripts over it. So a fresh `cargo install` / `brew install`
/// / scripted install all reach a working `reproit fuzz <url>` with the runner
/// logic in lock-step with the binary -- no `REPROIT_WEB_RUNNER_DIR`, no
/// stale-cache skew. `version` pins the matching release asset for the one-time
/// `node_modules` download; `log` receives human progress lines.
pub fn ensure_web_runner_dir(version: &str, log: &dyn Fn(&str)) -> Result<PathBuf> {
    // Dev/source checkout: used as-is so edits are live without a reinstall.
    if let Some(d) = find_dev_runner_dir() {
        if ensure_web_runner_dependencies(&d, log)? {
            ensure_web_browser(&d, log)?;
        }
        return Ok(d);
    }
    let dir = web_runner_data_dir();
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating web runner dir {}", dir.display()))?;
    // Provision the heavy deps (Playwright + node_modules + browser) ONCE.
    if !dir.join("node_modules").is_dir() {
        // Node is the one external prerequisite for the web fuzzer (it drives
        // Playwright). Check up front so the failure is actionable, not a cryptic
        // spawn error deep in the drive loop.
        let node_ok = std::process::Command::new("node")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !node_ok {
            bail!(
                "reproit's web fuzzer needs Node.js (18+), which was not found. Install it \
                 (https://nodejs.org or `brew install node`), then re-run."
            );
        }
        log("web runner not found; provisioning it (one-time)...");
        download_and_extract_runner(version, &dir, log)?;
    }
    // ALWAYS sync the runner scripts to THIS binary, so a binary update is never
    // paired with a stale runner (the cause of clips silently failing).
    write_embedded_runner(&dir)?;
    if ensure_web_runner_dependencies(&dir, log)? {
        ensure_web_browser(&dir, log)?;
    }
    Ok(dir)
}

fn ensure_web_runner_dependencies(dir: &Path, log: &dyn Fn(&str)) -> Result<bool> {
    use sha2::{Digest, Sha256};

    let lock = include_bytes!("../../../../runners/web/package-lock.json");
    let expected: String = Sha256::digest(lock)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let marker = dir.join(".reproit-package-lock.sha256");
    let current = std::fs::read_to_string(&marker).unwrap_or_default();
    let required = dir.join("node_modules/@a2ui/web_core").is_dir()
        && dir.join("node_modules/esbuild").is_dir()
        && dir.join("node_modules/playwright").is_dir();
    if required && current.trim() == expected {
        return Ok(false);
    }

    log("  syncing web and A2UI runner dependencies (one-time)...");
    let npm = if cfg!(windows) { "npm.cmd" } else { "npm" };
    let status = std::process::Command::new(npm)
        .args(["ci", "--omit=dev"])
        .current_dir(dir)
        .status()
        .context("running `npm ci --omit=dev` for the web runner")?;
    if !status.success() {
        bail!(
            "failed to install the web runner dependencies under {}",
            dir.display()
        );
    }
    std::fs::write(&marker, format!("{expected}\n"))
        .with_context(|| format!("writing dependency marker {}", marker.display()))?;
    Ok(true)
}

/// Release asset URL for `asset`. A clean release version (e.g. "0.1.2") pins
/// the matching tag; a dev build (e.g. "0.1.2-3-gabc-dirty") has no asset of
/// its own, so it falls back to the latest release.
fn release_asset_url(version: &str, asset: &str) -> String {
    let is_release =
        version.split('.').count() == 3 && version.chars().all(|c| c.is_ascii_digit() || c == '.');
    if is_release {
        format!("https://github.com/{RELEASE_REPO}/releases/download/v{version}/{asset}")
    } else {
        format!("https://github.com/{RELEASE_REPO}/releases/latest/download/{asset}")
    }
}

/// Download the prebuilt runner bundle and extract it flat into `dir` (so
/// `dir/runner.mjs` and `dir/node_modules` land in place). Shells out to `curl`
/// and `tar`, which ship on macOS, Linux, and Windows 10+, to avoid pulling a
/// TLS/archive stack into the binary just for this one-time provisioning.
fn download_and_extract_runner(version: &str, dir: &Path, log: &dyn Fn(&str)) -> Result<()> {
    let url = release_asset_url(version, "reproit-web-runner.tar.gz");
    let tmp = std::env::temp_dir().join("reproit-web-runner.tar.gz");
    log(&format!("  downloading {url}"));
    let st = std::process::Command::new("curl")
        .args(["-fsSL", "-o"])
        .arg(&tmp)
        .arg(&url)
        .status()
        .context("running curl to download the web runner (is curl installed?)")?;
    if !st.success() {
        bail!("failed to download the web runner bundle from {url}");
    }

    // Integrity check: the binary downloads and executes this bundle, so verify
    // it against the SHA-256 the release publishes alongside it before trusting
    // it. We fetch the sibling `.sha256` asset and compare.
    //
    // Transition safety: older releases predate the checksum asset, so an ABSENT
    // checksum logs a warning and proceeds (a hard failure would brick installs
    // of already-published releases). Whenever the checksum IS present, a
    // mismatch is fatal (fail closed) -- we delete the temp file and bail.
    //
    // TODO: once every live release publishes `reproit-web-runner.tar.gz.sha256`
    // (the release workflow now does), make a MISSING checksum fatal too.
    let sum_url = release_asset_url(version, "reproit-web-runner.tar.gz.sha256");
    match fetch_text(&sum_url)? {
        Some(sum_body) => match parse_sha256_hex(&sum_body) {
            Some(expected) => {
                let actual = sha256_file_hex(&tmp)
                    .with_context(|| format!("hashing downloaded bundle {}", tmp.display()))?;
                if !actual.eq_ignore_ascii_case(&expected) {
                    let _ = std::fs::remove_file(&tmp);
                    bail!(
                        "web runner checksum mismatch (expected {expected}, got {actual}); \
                         refusing to use a tampered or corrupt bundle"
                    );
                }
                log("  checksum verified.");
            }
            None => {
                let _ = std::fs::remove_file(&tmp);
                bail!("web runner checksum asset {sum_url} is malformed");
            }
        },
        None => {
            log(
                "  WARNING: no checksum asset published for this release; skipping integrity \
                 verification (older release).",
            );
        }
    }

    log("  extracting...");
    // Harden extraction: refuse absolute paths and `..` traversal, and don't
    // restore archived ownership. Flags are chosen to work on GNU tar (Linux),
    // bsdtar (macOS, Windows 10+). `-P` is OFF by default on all three, so a
    // leading `/` is already stripped; we additionally validate every entry path
    // ourselves below so a malicious `..` can never escape `dir` even if a tar
    // build ignored a flag.
    validate_tar_entries(&tmp)?;
    let st = std::process::Command::new("tar")
        .arg("--no-same-owner")
        .arg("-xzf")
        .arg(&tmp)
        .arg("-C")
        .arg(dir)
        .status()
        .context("running tar to extract the web runner (is tar installed?)")?;
    let _ = std::fs::remove_file(&tmp);
    if !st.success() {
        bail!("failed to extract the web runner bundle");
    }
    if !dir.join("node_modules").is_dir() {
        bail!(
            "web runner bundle extracted but node_modules is missing under {}",
            dir.display()
        );
    }
    Ok(())
}

/// Fetch a small text asset (the checksum file) over curl. Returns `Ok(None)`
/// only when the asset is absent (HTTP 404), so network errors and server
/// failures do not silently downgrade checksum verification.
fn fetch_text(url: &str) -> Result<Option<String>> {
    let tmp = std::env::temp_dir().join(format!(
        "reproit-checksum-{}-{}.txt",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let out = std::process::Command::new("curl")
        .args(["-sS", "-L", "--output"])
        .arg(&tmp)
        .args(["--write-out", "%{http_code}", url])
        .output()
        .context("running curl to fetch the web runner checksum")?;
    if !out.status.success() {
        let _ = std::fs::remove_file(&tmp);
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        bail!("failed to fetch web runner checksum {url}: {stderr}");
    }
    let code_text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let code = code_text.parse::<u16>().unwrap_or(0);
    if code == 404 {
        let _ = std::fs::remove_file(&tmp);
        return Ok(None);
    }
    if !(200..300).contains(&code) {
        let _ = std::fs::remove_file(&tmp);
        bail!("failed to fetch web runner checksum {url}: HTTP {code}");
    }
    let body = std::fs::read_to_string(&tmp)
        .with_context(|| format!("reading checksum response {}", tmp.display()))?;
    let _ = std::fs::remove_file(&tmp);
    Ok(Some(body))
}

/// Pull the 64-hex-char SHA-256 out of a `shasum`/`sha256sum`-style line, which
/// is `<hex>  <filename>` (the hex may also stand alone). Returns None if no
/// well-formed 64-char hex token is present.
fn parse_sha256_hex(body: &str) -> Option<String> {
    let tok = body.split_whitespace().next()?;
    if tok.len() == 64 && tok.bytes().all(|b| b.is_ascii_hexdigit()) {
        Some(tok.to_ascii_lowercase())
    } else {
        None
    }
}

/// SHA-256 of a file, lowercase hex.
fn sha256_file_hex(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(path)?;
    let mut h = Sha256::new();
    h.update(&bytes);
    Ok(hex_lower(&h.finalize()))
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Defense-in-depth path validation: list the tarball entries and reject the
/// whole archive if any entry is absolute or contains a `..` component, so a
/// crafted bundle can never write outside the target dir regardless of how the
/// platform `tar` treats traversal. Cross-platform: `tar -tzf` lists on GNU tar
/// and bsdtar alike.
fn validate_tar_entries(tarball: &Path) -> Result<()> {
    let out = std::process::Command::new("tar")
        .arg("-tzf")
        .arg(tarball)
        .output()
        .context("listing the web runner tarball entries (is tar installed?)")?;
    if !out.status.success() {
        bail!("failed to list the web runner bundle (corrupt download?)");
    }
    let listing = String::from_utf8_lossy(&out.stdout);
    for entry in listing.lines() {
        let e = entry.trim();
        if e.is_empty() {
            continue;
        }
        // Absolute paths (unix `/...`, Windows `C:\...` or `\...`).
        let abs =
            e.starts_with('/') || e.starts_with('\\') || (e.len() >= 2 && e.as_bytes()[1] == b':');
        if abs {
            bail!("web runner bundle contains an absolute path entry ({e}); refusing to extract");
        }
        // `..` traversal in any path component (handle both separators).
        if e.split(['/', '\\']).any(|c| c == "..") {
            bail!("web runner bundle contains a `..` traversal entry ({e}); refusing to extract");
        }
    }
    Ok(())
}

/// Ensure the headless browser the runner drives is installed (Playwright
/// caches it outside the runner dir, so this is a no-op when already present).
fn ensure_web_browser(dir: &Path, log: &dyn Fn(&str)) -> Result<()> {
    let cli = dir.join("node_modules/playwright/cli.js");
    if !cli.exists() {
        return Ok(());
    }
    log("  ensuring the headless browser (chromium)...");
    let st = std::process::Command::new("node")
        .arg(&cli)
        .args(["install", "chromium"])
        .status()
        .context("running `playwright install chromium`")?;
    if !st.success() {
        bail!("failed to install the chromium browser for the web runner");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{parse_sha256_hex, sha256_file_hex, WEB_RUNNER_FILES};

    #[test]
    fn parse_sha256_hex_accepts_shasum_lines_and_rejects_junk() {
        let hex = "e".repeat(64);
        assert_eq!(parse_sha256_hex(&hex).as_deref(), Some(hex.as_str()));
        let line = format!("{hex}  reproit-web-runner.tar.gz\n");
        assert_eq!(parse_sha256_hex(&line).as_deref(), Some(hex.as_str()));
        assert_eq!(
            parse_sha256_hex(&"A".repeat(64)).as_deref(),
            Some("a".repeat(64).as_str())
        );
        assert!(parse_sha256_hex("deadbeef").is_none());
        assert!(parse_sha256_hex(&"z".repeat(64)).is_none());
        assert!(parse_sha256_hex("").is_none());
    }

    #[test]
    fn sha256_file_hex_matches_known_vector() {
        let dir = std::env::temp_dir().join(format!("reproit_sha_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("data");
        std::fs::write(&file, b"abc").unwrap();
        assert_eq!(
            sha256_file_hex(&file).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let upper = sha256_file_hex(&file).unwrap().to_ascii_uppercase();
        assert!(upper.eq_ignore_ascii_case(&sha256_file_hex(&file).unwrap()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Pull every `./<name>.mjs` referenced by a static `from` or dynamic
    /// `import(...)` out of a runner script.
    fn local_mjs_imports(src: &str) -> Vec<String> {
        let mut out = Vec::new();
        for marker in ["from './", "from \"./", "import('./", "import(\"./"] {
            let mut rest = src;
            while let Some(index) = rest.find(marker) {
                rest = &rest[index + marker.len()..];
                let end = rest.find(['\'', '"']).unwrap_or(rest.len());
                let module = &rest[..end];
                if module.ends_with(".mjs") {
                    out.push(module.to_string());
                }
                rest = &rest[end..];
            }
        }
        out
    }

    #[test]
    fn web_runner_files_are_import_closed() {
        let shipped: std::collections::HashSet<&str> =
            WEB_RUNNER_FILES.iter().map(|(name, _)| *name).collect();
        for (name, contents) in WEB_RUNNER_FILES {
            if !name.ends_with(".mjs") {
                continue;
            }
            for import in local_mjs_imports(contents) {
                assert!(
                    shipped.contains(import.as_str()),
                    "{name} imports './{import}' but it is missing from WEB_RUNNER_FILES (the \
                     embedded sync won't write it, breaking installs on upgrade)"
                );
            }
        }
    }
}
