//! Configuration discovery, interpolation, parsing, and validation.

use super::{Config, CONFIG_SCHEMA_VERSION};
use anyhow::{bail, Context, Result};
use regex::Regex;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub struct Loaded {
    pub config: Config,
    /// Directory of the config file; relative paths resolve from here.
    pub root: PathBuf,
}

pub fn load(explicit: Option<&Path>) -> Result<Loaded> {
    let file = match explicit {
        Some(path) => path.to_path_buf(),
        None => find_config(&std::env::current_dir()?).context(
            "no reproit.yaml found in cwd or ancestors; run `reproit init`, pass --config, \
             or copy docs/examples/reproit.yaml",
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
    let mut value: serde_yaml::Value = serde_yaml::from_str(raw)?;
    interpolate_value(&mut value)?;
    let mut config: Config = serde_yaml::from_value(value)?;
    if config.schema_version != CONFIG_SCHEMA_VERSION {
        bail!(
            "reproit config schemaVersion {} cannot be read; this binary reads version {}; \
             migrate the config fields to version {} before loading it",
            config.schema_version,
            CONFIG_SCHEMA_VERSION,
            CONFIG_SCHEMA_VERSION
        );
    }
    if crate::adapters::platform::resolve(&config.app.platform).is_none() {
        bail!(
            "app.platform {:?} is not one reproit knows; set it to one of: {}",
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

fn env_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r"\$\{(\w+)(?::(-|\?)([^}]*))?\}").unwrap())
}

/// Expand the supported shell parameter-expansion subset in a single scalar,
/// accumulating any missing required variables.
fn expand_scalar(raw: &str, missing: &mut Vec<String>) -> String {
    env_regex()
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
        .into_owned()
}

fn expand_tree(value: &mut serde_yaml::Value, missing: &mut Vec<String>) {
    match value {
        serde_yaml::Value::String(text) => *text = expand_scalar(text, missing),
        serde_yaml::Value::Sequence(items) => {
            for item in items {
                expand_tree(item, missing);
            }
        }
        serde_yaml::Value::Mapping(map) => {
            for (_key, entry) in map.iter_mut() {
                expand_tree(entry, missing);
            }
        }
        _ => {}
    }
}

/// Interpolate the supported shell parameter-expansion subset across a PARSED
/// config tree, reporting every missing required variable together. Substituting
/// into scalars *after* the YAML parse (not into the raw text) keeps a substituted
/// value a string regardless of its shape: `phone: ${PHONE}` becomes the string
/// "+15551230001", never the int 15551230001 that unquoted YAML would coerce and a
/// downstream `Json<String>` extractor would reject with an opaque 422. It also
/// means a `${VAR}` written inside a config comment is never touched, since
/// comments are gone by the time the tree exists.
pub(crate) fn interpolate_value(value: &mut serde_yaml::Value) -> Result<()> {
    let mut missing = Vec::new();
    expand_tree(value, &mut missing);
    if !missing.is_empty() {
        bail!("unresolved config variables:\n  {}", missing.join("\n  "));
    }
    Ok(())
}
