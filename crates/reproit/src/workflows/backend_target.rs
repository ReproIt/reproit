//! Resolution of schema-first backend targets from a project configuration.

use crate::domain::backend;
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

/// A project whose reproit.yaml is backend-only: no `app` section and
/// `backend.enabled: true`. The schema may still be missing on disk; doctor
/// reports that as a failing check instead of erroring out.
pub(crate) struct BackendProject {
    pub(super) root: PathBuf,
    pub(super) config: backend::BackendConfig,
}

impl BackendProject {
    /// Every declared schema, each required to exist for scan/fuzz runs. A
    /// backend contract may be split across several files; resolving only
    /// `.schemas.first()` silently dropped every operation past the first, so
    /// scan, fuzz, and doctor load the full list.
    pub(crate) fn schema_paths(&self) -> Result<Vec<PathBuf>> {
        if self.config.schemas.is_empty() {
            bail!(
                "backend.enabled is true but backend.schemas lists nothing yet; add \
                 your schema file(s) there, or run `reproit init` to derive a draft \
                 from source"
            );
        }
        let mut paths = Vec::with_capacity(self.config.schemas.len());
        for schema in &self.config.schemas {
            let target = self.root.join(schema);
            if !target.is_file() {
                bail!(
                    "backend schema {} is not on disk; fix its path under \
                     backend.schemas, or run `reproit init` to regenerate the draft",
                    target.display()
                );
            }
            paths.push(target);
        }
        Ok(paths)
    }
}

/// Find the backend project configuration, if the effective reproit.yaml is a
/// backend one. App-platform configs and missing configs return None.
pub(crate) fn find(config_path: Option<&Path>) -> Result<Option<BackendProject>> {
    let path = match config_path {
        Some(path) if path.is_file() => Some(path.to_path_buf()),
        Some(path) => anyhow::bail!("config file {} does not exist", path.display()),
        None => find_config()?,
    };
    let Some(path) = path else {
        return Ok(None);
    };
    let raw = std::fs::read_to_string(&path)?;
    let mut document: serde_yaml::Value = serde_yaml::from_str(&raw)?;
    if document.get("app").is_some() {
        return Ok(None);
    }
    if document.get("backend").is_none() {
        return Ok(None);
    }
    // A backend config: interpolate ${VAR}, ${VAR:-default}, and ${VAR:?required}
    // over the parsed document with the same loader the app config uses, so secrets
    // reach every field (login url/path/headers, not just bodies) and the syntax
    // matches the rest of reproit. Interpolating the tree, not the raw text, keeps a
    // substituted value a string (an env-supplied phone stays "+1555..." instead of
    // being re-read by YAML as an int) and leaves `${VAR}` in comments alone.
    crate::adapters::config::interpolate_value(&mut document)?;
    let backend = document
        .get("backend")
        .expect("backend key present before interpolation");
    let config: backend::BackendConfig = serde_yaml::from_value(backend.clone())?;
    if !config.enabled {
        return Ok(None);
    }
    // A bare-filename config (`reproit.yaml`) has an EMPTY parent, not None, so
    // the previous `.unwrap_or(".")` never fired and left the root blank (doctor
    // printed "backend project root " with nothing after it). Fall back to the
    // current directory and canonicalize so the root reads as an absolute path,
    // matching the app-platform "loaded project root" line.
    let root = backend_project_root(&path, &std::env::current_dir()?);
    Ok(Some(BackendProject { root, config }))
}

fn backend_project_root(path: &Path, current_directory: &Path) -> PathBuf {
    let declared_root = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| current_directory.to_path_buf());
    let absolute_root = if declared_root.is_absolute() {
        declared_root
    } else {
        current_directory.join(declared_root)
    };
    absolute_root.canonicalize().unwrap_or(absolute_root)
}

pub(super) fn resolve(
    config_path: Option<&Path>,
) -> Result<Option<(Vec<PathBuf>, backend::BackendConfig)>> {
    let Some(project) = find(config_path)? else {
        return Ok(None);
    };
    let schemas = project.schema_paths()?;
    Ok(Some((schemas, project.config)))
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

/// Resolve a live target for a backend project with zero flags, booting the
/// service when nothing names one: the same machinery bare `reproit init`
/// uses (a verified already-running server, else a bounded boot of the
/// package.json start script). Precedence is untouched: an explicit flag,
/// `REPROIT_BACKEND_URL`, `backend.target`, or a schema `servers` URL all
/// suppress the boot. Returns true when this call booted the process itself;
/// the caller must then call `boot::shutdown_process_reset()` on every exit
/// path. A booted process is also installed as the restart-reset mechanism so
/// stateful confirmation works without a reset URL.
pub(super) async fn ensure_live_target(
    ctx: &crate::interface::cli::context::Ctx,
    root: &Path,
    flag: Option<&str>,
    config_target: Option<&str>,
    schemas: &[PathBuf],
) -> Result<bool> {
    use crate::workflows::backend_learn::boot;

    let env = std::env::var("REPROIT_BACKEND_URL").ok();
    if pick_target(flag, env.as_deref(), config_target).is_some() {
        return Ok(false);
    }
    let surface = crate::workflows::backend_headless::schema_surface(schemas)?;
    if surface.declares_server_url {
        return Ok(false);
    }
    let Some(auto) = boot::auto_target(ctx, root, surface.probe_path.as_deref()).await else {
        return Ok(false);
    };
    std::env::set_var("REPROIT_BACKEND_URL", &auto.url);
    let Some(server) = auto.server else {
        // An already-running server is the user's own: it is a target, not a
        // process this run may restart as a reset.
        return Ok(false);
    };
    let ready_port = auto
        .url
        .parse::<reqwest::Url>()
        .ok()
        .and_then(|url| url.port())
        .context("booted target URL has no port")?;
    let ready_path = surface
        .probe_path
        .clone()
        .expect("auto_target requires a probe path");
    boot::install_process_reset(boot::RestartableServer::adopt(
        server, ready_port, ready_path,
    ))
    .await;
    Ok(true)
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

    #[test]
    fn bare_filename_backend_config_uses_absolute_current_directory() {
        let directory =
            std::env::temp_dir().join(format!("reproit-backend-root-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let root = backend_project_root(Path::new("reproit.yaml"), &directory);
        assert_eq!(root, directory.canonicalize().unwrap());
        std::fs::remove_dir_all(directory).unwrap();
    }
}
