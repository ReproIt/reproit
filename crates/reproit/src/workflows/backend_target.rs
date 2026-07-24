//! Resolution of schema-first backend targets from a project configuration.

use crate::domain::backend;
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

/// A project whose reproit.yaml is backend-only: no `app` section and
/// `backend.enabled: true`. The schema may still be missing on disk; doctor
/// reports that as a failing check instead of erroring out.
pub(super) struct BackendProject {
    pub(super) root: PathBuf,
    pub(super) config: backend::BackendConfig,
}

impl BackendProject {
    /// The first declared schema, required to exist for scan/fuzz runs.
    pub(super) fn schema_path(&self) -> Result<PathBuf> {
        let schema = self
            .config
            .schemas
            .first()
            .context("backend.enabled is true but backend.schemas is empty")?;
        let target = self.root.join(schema);
        if !target.is_file() {
            bail!("backend schema {} does not exist", target.display());
        }
        Ok(target)
    }
}

/// Find the backend project configuration, if the effective reproit.yaml is a
/// backend one. App-platform configs and missing configs return None.
pub(super) fn find(config_path: Option<&Path>) -> Result<Option<BackendProject>> {
    let path = match config_path {
        Some(path) if path.is_file() => Some(path.to_path_buf()),
        Some(path) => anyhow::bail!("config file {} does not exist", path.display()),
        None => find_config()?,
    };
    let Some(path) = path else {
        return Ok(None);
    };
    let document: serde_yaml::Value = serde_yaml::from_slice(&std::fs::read(&path)?)?;
    if document.get("app").is_some() {
        return Ok(None);
    }
    let Some(backend) = document.get("backend") else {
        return Ok(None);
    };
    let config: backend::BackendConfig = serde_yaml::from_value(backend.clone())?;
    if !config.enabled {
        return Ok(None);
    }
    let root = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    Ok(Some(BackendProject { root, config }))
}

pub(super) fn resolve(
    config_path: Option<&Path>,
) -> Result<Option<(PathBuf, backend::BackendConfig)>> {
    let Some(project) = find(config_path)? else {
        return Ok(None);
    };
    let schema = project.schema_path()?;
    Ok(Some((schema, project.config)))
}

/// Pure backend target precedence: `--target` flag (a positional URL counts
/// as the flag) > `REPROIT_BACKEND_URL` > `backend.target` in reproit.yaml.
/// None falls through to the schema `servers` entry. Returns the winner and
/// its source label for reporting.
pub(super) fn pick_target<'a>(
    flag: Option<&'a str>,
    env: Option<&'a str>,
    config: Option<&'a str>,
) -> Option<(&'a str, &'static str)> {
    flag.map(|url| (url, "--target"))
        .or(env.map(|url| (url, "REPROIT_BACKEND_URL")))
        .or(config.map(|url| (url, "backend.target")))
}

/// Resolve the precedence against the live environment and plumb the winner
/// to the backend executor via `REPROIT_BACKEND_URL`. With no winner the
/// executor falls back to the schema `servers` entry as before.
pub(super) fn apply_target_precedence(
    flag: Option<&str>,
    config_target: Option<&str>,
) -> Result<()> {
    let env = std::env::var("REPROIT_BACKEND_URL").ok();
    if let Some((url, source)) = pick_target(flag, env.as_deref(), config_target) {
        validate_target_url(url).with_context(|| format!("backend target from {source}"))?;
        std::env::set_var("REPROIT_BACKEND_URL", url);
    }
    Ok(())
}

/// How a scan/fuzz invocation routes when the cwd config is a backend one.
#[derive(Debug, PartialEq)]
pub(super) enum BackendRoute {
    /// Run the configured backend schema; carries the positional URL (if any)
    /// as the target override, equivalent to `--target`.
    Backend(Option<String>),
    /// Not a backend run: no backend project, `--platform web`, or a non-URL
    /// positional (an alias scoped to an app config).
    No,
}

/// Route a positional scan/fuzz target in the presence (or not) of a backend
/// project. A URL positional inside a backend project is the backend service
/// target, never a zero-config browser run; `--platform web` is the escape
/// hatch for genuinely wanting a browser against that URL.
pub(super) fn route_positional(
    backend_project: bool,
    force_web: bool,
    positional: Option<&str>,
) -> BackendRoute {
    if !backend_project || force_web {
        return BackendRoute::No;
    }
    match positional {
        None => BackendRoute::Backend(None),
        Some(target) => match crate::interface::cli::target::target_as_url(target) {
            Some(url) => BackendRoute::Backend(Some(url)),
            None => BackendRoute::No,
        },
    }
}

pub(super) fn validate_target_url(value: &str) -> Result<()> {
    let url = value
        .parse::<reqwest::Url>()
        .with_context(|| format!("invalid backend service URL {value:?}"))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        bail!("backend service URL must be absolute HTTP or HTTPS: {value}");
    }
    Ok(())
}

fn find_config() -> Result<Option<PathBuf>> {
    let mut directory = std::env::current_dir()?;
    loop {
        let candidate = directory.join("reproit.yaml");
        if candidate.is_file() {
            return Ok(Some(candidate));
        }
        if !directory.pop() {
            return Ok(None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_precedence_is_flag_env_config_then_schema() {
        let flag = Some("http://flag:1");
        let env = Some("http://env:2");
        let config = Some("http://config:3");
        assert_eq!(
            pick_target(flag, env, config),
            Some(("http://flag:1", "--target"))
        );
        assert_eq!(
            pick_target(None, env, config),
            Some(("http://env:2", "REPROIT_BACKEND_URL"))
        );
        assert_eq!(
            pick_target(None, None, config),
            Some(("http://config:3", "backend.target"))
        );
        assert_eq!(pick_target(None, None, None), None);
    }

    #[test]
    fn positional_urls_route_to_backend_never_to_zero_config_web() {
        // The observed stumble: `reproit scan http://127.0.0.1:4477` with a
        // backend reproit.yaml ran Chromium against a JSON API. Pinned: a URL
        // positional in a backend project is the backend target.
        assert_eq!(
            route_positional(true, false, Some("http://127.0.0.1:4477")),
            BackendRoute::Backend(Some("http://127.0.0.1:4477".into()))
        );
        assert_eq!(
            route_positional(true, false, Some("localhost:4477")),
            BackendRoute::Backend(Some("http://localhost:4477".into()))
        );
        assert_eq!(
            route_positional(true, false, None),
            BackendRoute::Backend(None)
        );
        // The escape hatch and the non-backend cases stay on the web path.
        assert_eq!(
            route_positional(true, true, Some("http://127.0.0.1:4477")),
            BackendRoute::No
        );
        assert_eq!(
            route_positional(false, false, Some("http://127.0.0.1:4477")),
            BackendRoute::No
        );
        assert_eq!(
            route_positional(true, false, Some("login")),
            BackendRoute::No
        );
    }

    #[test]
    fn target_urls_must_be_absolute_http() {
        assert!(validate_target_url("http://127.0.0.1:4477").is_ok());
        assert!(validate_target_url("https://api.example.com").is_ok());
        assert!(validate_target_url("ftp://x").is_err());
        assert!(validate_target_url("/orders").is_err());
        assert!(validate_target_url("localhost:4477").is_err());
    }
}
