//! Resolution of schema-first backend targets from a project configuration.

use crate::model::backend;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub(super) fn resolve(
    config_path: Option<&Path>,
) -> Result<Option<(PathBuf, backend::BackendConfig)>> {
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
    let schema = config
        .schemas
        .first()
        .context("backend.enabled is true but backend.schemas is empty")?;
    let target = path.parent().unwrap_or_else(|| Path::new(".")).join(schema);
    if !target.is_file() {
        anyhow::bail!("backend schema {} does not exist", target.display());
    }
    Ok(Some((target, config)))
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
