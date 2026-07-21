//! Configuration discovery, interpolation, parsing, and validation.

use super::Config;
use anyhow::{bail, Context, Result};
use regex::Regex;
use std::path::{Path, PathBuf};

pub struct Loaded {
    pub config: Config,
    /// Directory of the config file; relative paths resolve from here.
    pub root: PathBuf,
}

pub fn load(explicit: Option<&Path>) -> Result<Loaded> {
    let file = match explicit {
        Some(path) => path.to_path_buf(),
        None => find_config(&std::env::current_dir()?).context(
            "no reproit.yaml found in cwd or ancestors; pass --config or copy \
             examples/reproit.example.yaml",
        )?,
    };
    let raw =
        std::fs::read_to_string(&file).with_context(|| format!("reading {}", file.display()))?;
    let canonical = file.canonicalize()?;
    let parent = canonical
        .parent()
        .context("config file has no parent directory")?;
    // Persisted zero-config runs live under `.reproit`, but every relative path
    // is rooted at the project directory rather than the state directory.
    let root = if parent.file_name().is_some_and(|name| name == ".reproit") {
        parent
            .parent()
            .context("`.reproit` config has no parent directory")?
            .to_path_buf()
    } else {
        parent.to_path_buf()
    };
    parse_str(&raw, root).with_context(|| format!("parsing {}", file.display()))
}

/// Parse config YAML, interpolate its environment references, and validate all
/// platform and backend schema boundaries.
pub fn parse_str(raw: &str, root: PathBuf) -> Result<Loaded> {
    let raw = interpolate_env(raw)?;
    let mut config: Config = serde_yaml::from_str(&raw)?;
    if crate::adapters::platform::resolve(&config.app.platform).is_none() {
        bail!(
            "unsupported platform {:?}; known: {}",
            config.app.platform,
            crate::adapters::platform::known_ids()
        );
    }
    if config.journeys.done_markers.is_empty() {
        bail!("journeys.doneMarkers must not be empty");
    }
    crate::domain::route_access::validate(&config.route_access, &config.auth.accounts)?;
    for account in &config.auth.accounts {
        if let Some(route) = account
            .validate
            .as_ref()
            .and_then(|validate| validate.route.as_deref())
        {
            crate::domain::route_access::validate_route_path(route, "auth validate.route")?;
        }
    }
    config.backend.load_schemas(&root)?;
    Ok(Loaded { config, root })
}

fn find_config(from: &Path) -> Option<PathBuf> {
    let mut directory = from.to_path_buf();
    loop {
        let project = directory.join("reproit.yaml");
        if project.exists() {
            return Some(project);
        }
        let synthesized = crate::runtime::project_layout::config_path(&directory);
        if synthesized.exists() {
            return Some(synthesized);
        }
        if !directory.pop() {
            return None;
        }
    }
}

/// Interpolate the supported shell parameter-expansion subset across the whole
/// configuration and report every missing required variable together.
pub(super) fn interpolate_env(raw: &str) -> Result<String> {
    let regex = Regex::new(r"\$\{(\w+)(?::(-|\?)([^}]*))?\}").unwrap();
    let mut missing = Vec::new();
    let output = regex
        .replace_all(raw, |captures: &regex::Captures| {
            let name = &captures[1];
            let value = std::env::var(name).ok().filter(|value| !value.is_empty());
            match captures.get(2).map(|value| value.as_str()) {
                Some("-") => value.unwrap_or_else(|| captures[3].to_string()),
                Some("?") => value.unwrap_or_else(|| {
                    let message = captures[3].trim();
                    missing.push(if message.is_empty() {
                        format!("required config variable {name} is not set")
                    } else {
                        format!("{name}: {message}")
                    });
                    String::new()
                }),
                _ => value.unwrap_or_default(),
            }
        })
        .into_owned();
    if !missing.is_empty() {
        bail!("unresolved config variables:\n  {}", missing.join("\n  "));
    }
    Ok(output)
}
