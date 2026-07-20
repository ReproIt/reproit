//! Bounded project-state reset with explicit destructive confirmation.

use crate::cli::context::Ctx;
use crate::{config, init, layout, VERSION};
use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const GENERATED_NAMES: &[&str] = &["map", "runs", "recordings", "tmp", "tools"];

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum ResetMode {
    Generated,
    All,
}

#[derive(Debug)]
struct ResetPlan {
    root: PathBuf,
    mode: ResetMode,
    targets: Vec<PathBuf>,
}

#[derive(Debug)]
struct InitRequest {
    platform: Option<String>,
    web_url: Option<String>,
}

pub(super) fn run(
    explicit_config: Option<&Path>,
    ctx: &Ctx,
    all: bool,
    initialize: bool,
    platform: Option<&str>,
) -> Result<ExitCode> {
    let explicit_config = explicit_config.map(resolve_config_path).transpose()?;
    let root = project_root(explicit_config.as_deref(), initialize)?;
    let init_request = initialize
        .then(|| prepare_init(explicit_config.as_deref(), platform))
        .transpose()?;
    let plan = ResetPlan::new(root, all, explicit_config.as_deref())?;
    if all && !confirm_all(ctx, &plan) {
        bail!("reset --all cancelled; no files were removed");
    }
    let removed = plan.execute()?;
    if let Some(request) = init_request {
        initialize_project(&plan.root, request)?;
    }
    emit_result(ctx, &plan, &removed, initialize);
    Ok(ExitCode::SUCCESS)
}

impl ResetPlan {
    fn new(root: PathBuf, all: bool, explicit_config: Option<&Path>) -> Result<Self> {
        validate_root(&root)?;
        let mode = if all {
            ResetMode::All
        } else {
            ResetMode::Generated
        };
        let state = layout::reproit_dir(&root);
        let targets = if all {
            let config = explicit_config
                .map(Path::to_path_buf)
                .unwrap_or_else(|| root.join("reproit.yaml"));
            let mut targets = vec![state.clone()];
            if !config.starts_with(&state) {
                targets.push(config);
            }
            targets
        } else {
            GENERATED_NAMES
                .iter()
                .map(|name| state.join(name))
                .collect()
        };
        Ok(Self {
            root,
            mode,
            targets,
        })
    }

    fn execute(&self) -> Result<Vec<PathBuf>> {
        let mut removed = Vec::new();
        for target in &self.targets {
            if remove_owned_path(target)? {
                removed.push(target.clone());
            }
        }
        Ok(removed)
    }
}

fn resolve_config_path(config: &Path) -> Result<PathBuf> {
    let absolute = if config.is_absolute() {
        config.to_path_buf()
    } else {
        std::env::current_dir()?.join(config)
    };
    let file_name = absolute.file_name().context("config file has no name")?;
    let parent = absolute.parent().context("config file has no parent")?;
    let parent = parent
        .canonicalize()
        .with_context(|| format!("resolving config directory {}", parent.display()))?;
    let resolved = parent.join(file_name);
    std::fs::metadata(&resolved)
        .with_context(|| format!("resolving config {}", resolved.display()))?;
    Ok(resolved)
}

fn project_root(explicit_config: Option<&Path>, allow_current: bool) -> Result<PathBuf> {
    if let Some(config) = explicit_config {
        let parent = config.parent().context("config file has no parent")?;
        let root = if parent.file_name().is_some_and(|name| name == ".reproit") {
            parent.parent().context(".reproit has no project parent")?
        } else {
            parent
        };
        return Ok(root.to_path_buf());
    }

    let current = std::env::current_dir()?.canonicalize()?;
    for directory in current.ancestors() {
        if directory.join("reproit.yaml").is_file()
            || directory.join(".reproit").symlink_metadata().is_ok()
        {
            return Ok(directory.to_path_buf());
        }
    }
    if allow_current {
        return Ok(current);
    }
    bail!("no Reproit project state found in the current directory or its ancestors")
}

fn validate_root(root: &Path) -> Result<()> {
    if !root.is_absolute() || !root.is_dir() || root.parent().is_none() {
        bail!(
            "refusing to reset an invalid project root {}",
            root.display()
        );
    }
    Ok(())
}

fn remove_owned_path(path: &Path) -> Result<bool> {
    let metadata = match path.symlink_metadata() {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).with_context(|| format!("inspecting {}", path.display())),
    };
    if metadata.file_type().is_symlink() || metadata.is_file() {
        std::fs::remove_file(path).with_context(|| format!("removing {}", path.display()))?;
    } else if metadata.is_dir() {
        std::fs::remove_dir_all(path).with_context(|| format!("removing {}", path.display()))?;
    } else {
        bail!(
            "refusing to remove unsupported filesystem object {}",
            path.display()
        );
    }
    Ok(true)
}

fn confirm_all(ctx: &Ctx, plan: &ResetPlan) -> bool {
    if !ctx.quiet && !ctx.json {
        eprintln!("  reset --all will permanently remove:");
        for target in &plan.targets {
            eprintln!("    {}", target.display());
        }
        eprintln!("  Application source and journeys are preserved.");
    }
    if ctx.yes {
        return true;
    }
    if !std::io::stdin().is_terminal() {
        eprintln!("  Refusing without confirmation. Re-run with --yes to proceed.");
        return false;
    }
    eprint!("  Reset all Reproit project state? [y/N] ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

fn prepare_init(
    explicit_config: Option<&Path>,
    override_platform: Option<&str>,
) -> Result<InitRequest> {
    if let Some(platform) = override_platform {
        return Ok(InitRequest {
            platform: Some(init_platform(platform)?.to_string()),
            web_url: None,
        });
    }
    let loaded = config::load(explicit_config)
        .context("reset --all --init needs a valid existing config or an explicit --platform")?;
    let platform = init_platform(&loaded.config.app.platform)?.to_string();
    let web_url = (platform == "web")
        .then(|| loaded.config.app.url.clone())
        .flatten();
    Ok(InitRequest {
        platform: Some(platform),
        web_url,
    })
}

fn init_platform(platform: &str) -> Result<&'static str> {
    match platform {
        "flutter" => Ok("flutter"),
        "web" => Ok("web"),
        "rn" | "react-native" => Ok("rn"),
        "android" => Ok("android"),
        "backend" => Ok("backend"),
        _ => bail!(
            "platform {platform:?} cannot be initialized automatically; pass --platform with one \
             of flutter, web, rn, android, or backend"
        ),
    }
}

fn initialize_project(root: &Path, request: InitRequest) -> Result<()> {
    if request.platform.as_deref() == Some("web") {
        if let Some(url) = request.web_url {
            let runner = config::ensure_web_runner_dir(VERSION, &|message| println!("{message}"))?;
            return init::init_web_url(root, &url, &runner, false);
        }
    }
    init::init(root, request.platform.as_deref(), false)
}

fn emit_result(ctx: &Ctx, plan: &ResetPlan, removed: &[PathBuf], initialized: bool) {
    if ctx.json {
        ctx.emit(&serde_json::json!({
            "command": "reset",
            "mode": plan.mode,
            "root": plan.root,
            "removed": removed,
            "initialized": initialized,
        }));
        return;
    }
    ctx.say(format!(
        "reproit reset: removed {} path(s){}",
        removed.len(),
        if initialized {
            " and initialized again"
        } else {
            ""
        }
    ));
    if plan.mode == ResetMode::Generated {
        ctx.say("  preserved: config, repros, captures, findings, capsules, and secrets");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn project() -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("reproit-reset-{}-{suffix}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "fixture").unwrap();
    }

    #[test]
    fn generated_reset_preserves_durable_and_human_state() {
        let root = project();
        for name in GENERATED_NAMES {
            touch(&root.join(".reproit").join(name).join("entry"));
        }
        for path in [
            ".reproit/.gitignore",
            ".reproit/repros/rep/example",
            ".reproit/captures/cap/example",
            ".reproit/findings/fnd/example",
            ".reproit/capsules/capsule/example",
            ".reproit/secrets.vault",
            "reproit.yaml",
        ] {
            touch(&root.join(path));
        }

        ResetPlan::new(root.clone(), false, None)
            .unwrap()
            .execute()
            .unwrap();

        for name in GENERATED_NAMES {
            assert!(!root.join(".reproit").join(name).exists());
        }
        for path in [
            ".reproit/.gitignore",
            ".reproit/repros/rep/example",
            ".reproit/captures/cap/example",
            ".reproit/findings/fnd/example",
            ".reproit/capsules/capsule/example",
            ".reproit/secrets.vault",
            "reproit.yaml",
        ] {
            assert!(root.join(path).exists(), "missing preserved {path}");
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn all_reset_removes_only_project_state_and_config() {
        let root = project();
        touch(&root.join(".reproit/repros/rep/example"));
        touch(&root.join("reproit.yaml"));
        touch(&root.join("journeys/login.yaml"));
        touch(&root.join("lib/main.dart"));

        ResetPlan::new(root.clone(), true, None)
            .unwrap()
            .execute()
            .unwrap();

        assert!(!root.join(".reproit").exists());
        assert!(!root.join("reproit.yaml").exists());
        assert!(root.join("journeys/login.yaml").exists());
        assert!(root.join("lib/main.dart").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn all_reset_removes_an_explicit_custom_config() {
        let root = project();
        let config = root.join("custom.yaml");
        touch(&root.join(".reproit/repros/rep/example"));
        touch(&config);
        touch(&root.join("reproit.yaml"));

        ResetPlan::new(root.clone(), true, Some(&config))
            .unwrap()
            .execute()
            .unwrap();

        assert!(!root.join(".reproit").exists());
        assert!(!config.exists());
        assert!(root.join("reproit.yaml").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn reset_unlinks_a_state_symlink_without_following_it() {
        use std::os::unix::fs::symlink;

        let root = project();
        let outside = project();
        touch(&outside.join("keep"));
        std::fs::create_dir_all(root.join(".reproit")).unwrap();
        symlink(&outside, root.join(".reproit/runs")).unwrap();

        ResetPlan::new(root.clone(), false, None)
            .unwrap()
            .execute()
            .unwrap();

        assert!(outside.join("keep").exists());
        assert!(!root.join(".reproit/runs").exists());
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(outside).unwrap();
    }
}
