use serde_json::Value;
use std::path::PathBuf;

/// The path the cloud/project key is persisted to: `~/.reproit/token`.
/// Falls back to `.reproit/token` under cwd when there is no home directory.
pub fn token_path() -> PathBuf {
    if let Some(home) = home_dir() {
        home.join(".reproit").join("token")
    } else {
        PathBuf::from(".reproit").join("token")
    }
}

/// Best-effort home directory from `$HOME` (unix) / `$USERPROFILE` (windows).
/// Avoids a new dependency for one lookup.
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
}

/// Persist a cloud/project key (+ base URL) to `path`. Written as JSON so the
/// URL travels with the token. Creates parent dirs as needed. On unix the file
/// is written 0600 (it holds a credential).
pub fn save_token(path: &std::path::Path, token: &str, url: &str) -> anyhow::Result<()> {
    save_cloud_profile(path, token, url, None)
}

/// Persist cloud credentials plus the currently selected project. Keeping the
/// app beside the validated token makes `reproit bugs` and direct `reproit
/// bkt_...` truly argument-free after setup. Older token files remain readable.
pub fn save_cloud_profile(
    path: &std::path::Path,
    token: &str,
    url: &str,
    app: Option<&str>,
) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_string_pretty(&serde_json::json!({
        "token": token,
        "url": url,
        "app": app,
    }))?;
    std::fs::write(path, body)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// Selected cloud app from the persisted profile, if setup has bound one.
pub fn load_cloud_app(path: &std::path::Path) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    v.get("app")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// Load a persisted token (token, url) from `path`. Returns None when the file
/// is absent or unparseable.
pub fn load_token(path: &std::path::Path) -> Option<(String, Option<String>)> {
    let raw = std::fs::read_to_string(path).ok()?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    let token = v.get("token").and_then(Value::as_str)?.to_string();
    let url = v
        .get("url")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(String::from);
    Some((token, url))
}
