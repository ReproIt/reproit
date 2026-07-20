//! Safe CLI updates and a non-blocking cached release notice.

use anyhow::{bail, Context, Result};
use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::OpenOptions;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

const RELEASE_API: &str = "https://api.github.com/repos/ReproIt/reproit/releases/latest";
const CHECK_INTERVAL_SECS: u64 = 24 * 60 * 60;
const MAX_ASSET_BYTES: usize = 256 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize)]
struct Release {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Clone, Debug, Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct UpdateCache {
    checked_at: u64,
    #[serde(default)]
    notified_at: u64,
    latest: Option<String>,
    release_url: Option<String>,
}

/// Print an already-cached notice immediately and refresh stale cache data in a
/// detached child. The command being run never waits for the network.
pub fn notice_and_schedule(current: &str, quiet: bool, json: bool) {
    if quiet || json || update_checks_disabled() {
        return;
    }
    let mut cache = read_cache().unwrap_or_default();
    if cache
        .latest
        .as_deref()
        .is_some_and(|latest| version_is_newer(latest, current))
        && now().saturating_sub(cache.notified_at) >= CHECK_INTERVAL_SECS
    {
        eprintln!(
            "ReproIt {} is available. Run `reproit update`.",
            cache.latest.as_deref().unwrap_or_default()
        );
        cache.notified_at = now();
        let _ = write_cache(&cache);
    }
    if now().saturating_sub(cache.checked_at) < CHECK_INTERVAL_SECS {
        return;
    }
    let lock = cache_path().with_extension("lock");
    if let Some(parent) = lock.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock)
        .is_err()
    {
        return;
    }
    let spawned = std::env::current_exe().and_then(|exe| {
        Command::new(exe)
            .arg("__update-check")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map(|_| ())
    });
    if spawned.is_err() {
        let _ = std::fs::remove_file(lock);
    }
}

pub async fn refresh_cache(current: &str) -> Result<()> {
    let lock = cache_path().with_extension("lock");
    let result = async {
        let release = latest_release(current).await?;
        write_cache(&UpdateCache {
            checked_at: now(),
            notified_at: 0,
            latest: Some(clean_version(&release.tag_name).to_string()),
            release_url: Some(release.html_url),
        })
    }
    .await;
    if result.is_err() {
        let _ = write_cache(&UpdateCache {
            checked_at: now(),
            ..UpdateCache::default()
        });
    }
    let _ = std::fs::remove_file(lock);
    result
}

pub async fn run(current: &str, check_only: bool) -> Result<()> {
    let release = latest_release(current).await?;
    let latest = clean_version(&release.tag_name);
    if !version_is_newer(latest, current) {
        println!("ReproIt {current} is current.");
        return Ok(());
    }
    if check_only {
        println!("ReproIt {latest} is available: {}", release.html_url);
        return Ok(());
    }

    let executable = std::env::current_exe().context("locating the current reproit binary")?;
    if cfg!(debug_assertions) || current.contains("-dirty") {
        bail!(
            "this is a development build; update the checkout and rebuild instead of replacing it"
        );
    }
    if installed_by_cargo(&executable) {
        println!("Updating ReproIt {current} to {latest} with Cargo...");
        let status = Command::new("cargo")
            .args([
                "install",
                "--git",
                "https://github.com/ReproIt/reproit",
                "--tag",
                &release.tag_name,
                "--locked",
                "--force",
                "reproit",
            ])
            .status()
            .context("starting cargo install")?;
        if !status.success() {
            bail!("Cargo could not update ReproIt");
        }
        println!("Updated ReproIt to {latest}.");
        return Ok(());
    }
    if installed_by_homebrew(&executable) {
        println!("Updating ReproIt {current} to {latest} with Homebrew...");
        let status = Command::new("brew")
            .args(["upgrade", "reproit"])
            .status()
            .context("starting brew upgrade reproit")?;
        if !status.success() {
            bail!("Homebrew could not update ReproIt");
        }
        println!("Updated ReproIt to {latest}.");
        return Ok(());
    }

    install_standalone(&release, latest, &executable).await?;
    println!("Updated ReproIt to {latest}.");
    Ok(())
}

async fn latest_release(current: &str) -> Result<Release> {
    let response = reqwest::Client::new()
        .get(RELEASE_API)
        .header("User-Agent", format!("reproit/{current}"))
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .context("checking GitHub for the latest ReproIt release")?;
    let status = response.status();
    if !status.is_success() {
        if status == reqwest::StatusCode::NOT_FOUND {
            bail!("no ReproIt release is published yet; publish 0.1 before using `reproit update`");
        }
        bail!("GitHub release check failed with HTTP {status}");
    }
    response
        .json()
        .await
        .context("reading the latest ReproIt release")
}

async fn install_standalone(release: &Release, latest: &str, executable: &Path) -> Result<()> {
    let asset = select_binary_asset(&release.assets).with_context(|| {
        format!(
            "release {latest} has no binary for {}; install it from {}",
            platform_id(),
            release.html_url
        )
    })?;
    let checksum_asset = release
        .assets
        .iter()
        .find(|candidate| candidate.name == format!("{}.sha256", asset.name))
        .or_else(|| {
            release
                .assets
                .iter()
                .find(|candidate| candidate.name.eq_ignore_ascii_case("SHA256SUMS"))
        })
        .context("release is missing its checksum asset")?;
    let archive = download(&asset.browser_download_url, latest).await?;
    let checksums = download(&checksum_asset.browser_download_url, latest).await?;
    let checksums = String::from_utf8(checksums).context("checksum asset is not UTF-8")?;
    let expected = checksum_for(&checksums, &asset.name).context("binary checksum is missing")?;
    let actual = Sha256::digest(&archive)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    if actual != expected.to_ascii_lowercase() {
        bail!("downloaded update failed checksum verification");
    }
    let binary = extract_binary(&asset.name, &archive)?;
    let parent = executable
        .parent()
        .context("the current executable has no parent directory")?;
    let staged = parent.join(format!(
        ".reproit-update-{}{}",
        std::process::id(),
        exe_suffix()
    ));
    std::fs::write(&staged, binary)
        .with_context(|| format!("writing staged update {}", staged.display()))?;
    make_executable(&staged)?;
    verify_binary(&staged, latest)?;
    apply_update(executable, &staged)?;
    let _ = write_cache(&UpdateCache {
        checked_at: now(),
        notified_at: now(),
        latest: Some(latest.to_string()),
        release_url: Some(release.html_url.clone()),
    });
    Ok(())
}

async fn download(url: &str, version: &str) -> Result<Vec<u8>> {
    let response = reqwest::Client::new()
        .get(url)
        .header("User-Agent", format!("reproit/{version}"))
        .send()
        .await
        .with_context(|| format!("downloading {url}"))?;
    if !response.status().is_success() {
        bail!("update download failed with HTTP {}", response.status());
    }
    let bytes = response.bytes().await.context("reading update download")?;
    if bytes.len() > MAX_ASSET_BYTES {
        bail!("update asset exceeds the 256 MiB safety limit");
    }
    Ok(bytes.to_vec())
}

fn select_binary_asset(assets: &[Asset]) -> Option<&Asset> {
    let id = platform_id();
    let raw_name = format!("reproit-{id}{}", exe_suffix());
    assets
        .iter()
        .filter(|asset| {
            let name = asset.name.to_ascii_lowercase();
            name.starts_with("reproit-")
                && name.contains(&id)
                && (name.ends_with(".tar.gz") || name.ends_with(".zip") || name == raw_name)
                && !name.ends_with(".sha256")
        })
        .min_by_key(|asset| asset.name.len())
}

fn platform_id() -> String {
    let arch = std::env::consts::ARCH;
    let os = match std::env::consts::OS {
        "macos" => "apple-darwin",
        "linux" => "unknown-linux-gnu",
        "windows" => "pc-windows-msvc",
        other => other,
    };
    format!("{arch}-{os}")
}

fn checksum_for<'a>(contents: &'a str, asset: &str) -> Option<&'a str> {
    contents.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let hash = fields.next()?;
        let name = fields.next()?.trim_start_matches('*');
        (name == asset && hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()))
            .then_some(hash)
    })
}

fn extract_binary(name: &str, bytes: &[u8]) -> Result<Vec<u8>> {
    if name.ends_with(".tar.gz") {
        let mut archive = tar::Archive::new(GzDecoder::new(Cursor::new(bytes)));
        for entry in archive.entries().context("reading update archive")? {
            let mut entry = entry?;
            if entry
                .path()?
                .file_name()
                .is_some_and(|name| name == binary_name())
            {
                let mut binary = Vec::new();
                entry.read_to_end(&mut binary)?;
                return Ok(binary);
            }
        }
        bail!("update archive does not contain {}", binary_name());
    }
    if name.ends_with(".zip") {
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes))?;
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index)?;
            if Path::new(entry.name()).file_name() == Some(binary_name().as_ref()) {
                let mut binary = Vec::new();
                entry.read_to_end(&mut binary)?;
                return Ok(binary);
            }
        }
        bail!("update archive does not contain {}", binary_name());
    }
    Ok(bytes.to_vec())
}

fn verify_binary(path: &Path, expected: &str) -> Result<()> {
    let output = Command::new(path)
        .arg("--version")
        .output()
        .context("starting the downloaded ReproIt binary")?;
    if !output.status.success()
        || !String::from_utf8_lossy(&output.stdout).contains(clean_version(expected))
    {
        let _ = std::fs::remove_file(path);
        bail!("downloaded binary did not report ReproIt version {expected}");
    }
    Ok(())
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(not(windows))]
fn apply_update(current: &Path, staged: &Path) -> Result<()> {
    let backup = current.with_extension("reproit-old");
    let _ = std::fs::remove_file(&backup);
    std::fs::rename(current, &backup).context("preserving the current ReproIt binary")?;
    if let Err(error) = std::fs::rename(staged, current) {
        let _ = std::fs::rename(&backup, current);
        return Err(error).context("installing the new ReproIt binary");
    }
    let _ = std::fs::remove_file(backup);
    Ok(())
}

#[cfg(windows)]
fn apply_update(current: &Path, staged: &Path) -> Result<()> {
    let pid = std::process::id().to_string();
    let script = format!(
        "$p={pid}; while(Get-Process -Id $p -ErrorAction SilentlyContinue){{Start-Sleep \
         -Milliseconds 100}}; Move-Item -Force -LiteralPath '{}' -Destination '{}'",
        staged.display().to_string().replace('\'', "''"),
        current.display().to_string().replace('\'', "''")
    );
    Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("scheduling the Windows binary replacement")?;
    Ok(())
}

fn installed_by_cargo(path: &Path) -> bool {
    std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo")))
        .is_some_and(|home| path.starts_with(home.join("bin")))
}

fn installed_by_homebrew(path: &Path) -> bool {
    let value = path.to_string_lossy();
    value.contains("/Cellar/") || value.contains("/homebrew/") || value.contains("/linuxbrew/")
}

fn update_checks_disabled() -> bool {
    std::env::var_os("CI").is_some()
        || std::env::var_os("REPROIT_NO_UPDATE_CHECK").is_some()
        || cfg!(debug_assertions)
}

fn cache_path() -> PathBuf {
    let base = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Library/Caches"))
    } else {
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
    };
    base.unwrap_or_else(|| PathBuf::from("."))
        .join("reproit/update.json")
}

fn read_cache() -> Result<UpdateCache> {
    Ok(serde_json::from_slice(&std::fs::read(cache_path())?)?)
}

fn write_cache(cache: &UpdateCache) -> Result<()> {
    let path = cache_path();
    let parent = path.parent().context("update cache has no parent")?;
    std::fs::create_dir_all(parent)?;
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&temporary, serde_json::to_vec(cache)?)?;
    std::fs::rename(temporary, path)?;
    Ok(())
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn clean_version(version: &str) -> &str {
    version.trim().trim_start_matches('v')
}

fn numeric_version(version: &str) -> Option<[u64; 3]> {
    let core = clean_version(version)
        .split(['-', ' ', '('])
        .next()
        .unwrap_or_default();
    let mut parts = core.split('.');
    Some([
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    ])
}

fn version_is_newer(candidate: &str, current: &str) -> bool {
    numeric_version(candidate)
        .zip(numeric_version(current))
        .is_some_and(|(candidate, current)| candidate > current)
}

fn exe_suffix() -> &'static str {
    if cfg!(windows) {
        ".exe"
    } else {
        ""
    }
}

fn binary_name() -> &'static str {
    if cfg!(windows) {
        "reproit.exe"
    } else {
        "reproit"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_release_versions_without_treating_dev_suffixes_as_newer() {
        assert!(version_is_newer("v0.2.0", "0.1.9-4-gabc-dirty"));
        assert!(!version_is_newer("v0.1.9", "0.1.9-4-gabc-dirty"));
        assert!(!version_is_newer("invalid", "0.1.9"));
    }

    #[test]
    fn reads_only_the_checksum_for_the_selected_asset() {
        let contents = format!(
            "{}  other.tar.gz\n{} *wanted.tar.gz\n",
            "a".repeat(64),
            "b".repeat(64)
        );
        let expected = "b".repeat(64);
        assert_eq!(
            checksum_for(&contents, "wanted.tar.gz"),
            Some(expected.as_str())
        );
        assert_eq!(checksum_for(&contents, "missing.tar.gz"), None);
    }

    #[test]
    fn selects_the_current_platform_archive_and_not_its_checksum() {
        let id = platform_id();
        let assets = vec![
            Asset {
                name: format!("reproit-{id}.tar.gz.sha256"),
                browser_download_url: String::new(),
            },
            Asset {
                name: format!("reproit-{id}.tar.gz"),
                browser_download_url: String::new(),
            },
        ];
        let expected = format!("reproit-{id}.tar.gz");
        assert_eq!(
            select_binary_asset(&assets).map(|asset| asset.name.as_str()),
            Some(expected.as_str())
        );
    }
}
