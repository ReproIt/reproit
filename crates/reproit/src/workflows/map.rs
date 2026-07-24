//! App-map lifecycle and explicit debug-map workflows.

use super::*;
use crate::domain::map;

pub(super) async fn ensure_app_map(
    ctx: &Ctx,
    loaded: &config::Loaded,
    journey: &str,
) -> Result<()> {
    let replace = match map::map_freshness(&loaded.root)? {
        map::MapFreshness::Current => return Ok(()),
        map::MapFreshness::Missing => {
            ctx.say("  learning app structure (first run)...");
            false
        }
        map::MapFreshness::Stale(reasons) => {
            ctx.say(format!(
                "  app model changed ({}); refreshing automatically...",
                reasons.join(", ")
            ));
            true
        }
    };
    rebuild_app_map(loaded, journey, None, false, None, replace).await
}

/// Decide whether a scan result may update the committed map. A complete scan
/// can create or replace a snapshot; an incomplete scan may only merge into an
/// already-current graph, never create a misleading partial map or displace the
/// last good stale snapshot. The boolean is the replacement mode.
pub(super) fn scan_map_commit(freshness: &map::MapFreshness, complete: bool) -> Option<bool> {
    match freshness {
        map::MapFreshness::Current => Some(false),
        map::MapFreshness::Missing if complete => Some(false),
        map::MapFreshness::Stale(_) if complete => Some(true),
        map::MapFreshness::Missing | map::MapFreshness::Stale(_) => None,
    }
}

pub(super) async fn rebuild_app_map(
    loaded: &config::Loaded,
    journey: &str,
    budget: Option<u32>,
    label: bool,
    from: Option<&Path>,
    replace: bool,
) -> Result<()> {
    let run_dir = acquire_map_run(loaded, journey, budget, from).await?;
    let result = map::commit_map_run(&loaded.config, &loaded.root, &run_dir, label, replace).await;
    if result.is_ok() && replace && !map::appmap_path(&loaded.root).is_file() {
        return Err(anyhow::anyhow!(
            "could not refresh the internal app model; the app was not reachable"
        ));
    }
    result
}

async fn acquire_map_run(
    loaded: &config::Loaded,
    journey: &str,
    budget: Option<u32>,
    from: Option<&Path>,
) -> Result<std::path::PathBuf> {
    if let Some(path) = from {
        return Ok(if path.is_absolute() {
            path.to_path_buf()
        } else {
            loaded.root.join(path)
        });
    }

    let mut extra_defines = Vec::new();
    if let Some(budget) = budget {
        let config_path = layout::fuzz_config_path(&loaded.root);
        let parent = config_path
            .parent()
            .context("fuzz configuration path has no parent")?;
        std::fs::create_dir_all(parent)?;
        std::fs::write(
            &config_path,
            serde_json::json!({ "seed": 0, "budget": budget }).to_string(),
        )?;
        extra_defines.push((
            "REPROIT_FUZZ_CONFIG".to_string(),
            config_path.to_string_lossy().into_owned(),
        ));
    }

    let outcome = orchestrator::run_journey(
        &loaded.config,
        &loaded.root,
        journey,
        &orchestrator::RunOpts {
            devices: 1,
            extra_defines: &extra_defines,
            ..Default::default()
        },
    )
    .await?;
    if !outcome.passed {
        eprintln!("  note: exploration run did not pass cleanly; mapping what was observed");
    }
    Ok(outcome.run_dir)
}

/// Execute the advanced map diagnostics exposed under debug map.
pub(super) async fn debug_map(
    config_path: Option<&Path>,
    action: Option<MapAction>,
    ctx: &Ctx,
) -> Result<ExitCode> {
    // Bare `debug map` forces a full structural rebuild.
    let action = action.unwrap_or(MapAction::Structural {
        journey: "explore".to_string(),
        budget: None,
        label: false,
        from: None,
        platform: None,
        force: false,
    });
    match action {
        MapAction::Structural {
            journey,
            budget,
            label,
            from,
            platform,
            force,
        } => {
            // Scaffold the repo first if there's no config yet (folds in
            // the old `init`); then crawl + assemble the graph.
            if config::load(config_path).is_err() {
                project_scaffold::init(&std::env::current_dir()?, platform.as_deref(), force)?;
            }
            let loaded = config::load(config_path)?;
            rebuild_app_map(&loaded, &journey, budget, label, from.as_deref(), true).await?;
            if ctx.json {
                let m = map::load_map(&loaded.root, &loaded.config)?;
                ctx.emit(&serde_json::json!({
                    "command": "debug map structural",
                    "states": m.states.len(),
                    "transitions": m.transitions.len(),
                    "budget": budget,
                    "map_path": map::appmap_path(&loaded.root).to_string_lossy(),
                }));
            }
            Ok(ExitCode::SUCCESS)
        }
        MapAction::Show {
            format,
            out,
            map_path,
        } => {
            let path = match map_path {
                Some(p) => p,
                None => {
                    let loaded = config::load(config_path)?;
                    ensure_app_map(ctx, &loaded, "explore").await?;
                    map::appmap_path(&loaded.root)
                }
            };
            graph::render(&path, &format, out.as_deref())?;
            Ok(ExitCode::SUCCESS)
        }
        MapAction::Accessibility {
            state,
            kind,
            format,
            baseline,
            map_path,
        } => {
            // `root` is the project to attribute selectors into (file:
            // line). With an explicit --map-path we have no project tree
            // to scan, so attribution is skipped (None).
            let (m, root) = match map_path {
                Some(p) => {
                    let txt = std::fs::read_to_string(&p)?;
                    (serde_json::from_str::<appmap::AppMap>(&txt)?, None)
                }
                None => {
                    let loaded = config::load(config_path)?;
                    ensure_app_map(ctx, &loaded, "explore").await?;
                    let m = map::load_map(&loaded.root, &loaded.config)?;
                    (m, Some(loaded.root))
                }
            };
            // --baseline: regression gate. Diff the current map's gaps
            // against the baseline's and exit 1 if any new gap appeared.
            if let Some(bpath) = baseline {
                let btxt = std::fs::read_to_string(&bpath)?;
                let bmap = serde_json::from_str::<appmap::AppMap>(&btxt)?;
                let regressed = accessibility::regression(&bmap, &m, ctx);
                return Ok(if regressed {
                    ExitCode::from(1)
                } else {
                    ExitCode::SUCCESS
                });
            }
            accessibility::report(
                &m,
                root.as_deref(),
                state.as_deref(),
                kind.as_deref(),
                format == "md",
                ctx,
            );
            Ok(ExitCode::SUCCESS)
        }
        MapAction::Verify => {
            let loaded = config::load(config_path)?;
            let report = journey::verify_map(&loaded, ctx.json || ctx.quiet).await?;
            let code = if report.is_clean() { 0u8 } else { 3u8 };
            Ok(ExitCode::from(code))
        }
        MapAction::Semantic => {
            let loaded = config::load(config_path)?;
            ensure_app_map(ctx, &loaded, "explore").await?;
            let cm = mapplan::plan(&loaded, ctx.quiet).await?;
            if ctx.json {
                let mut v = mapplan::coverage_json(&cm);
                v["command"] = "debug map semantic".into();
                ctx.emit(&v);
            }
            Ok(ExitCode::SUCCESS)
        }
        MapAction::Coverage => {
            let loaded = config::load(config_path)?;
            ensure_app_map(ctx, &loaded, "explore").await?;
            mapplan::cover(&loaded, ctx.json)?;
            Ok(ExitCode::SUCCESS)
        }
        MapAction::Converge => {
            let loaded = config::load(config_path)?;
            ensure_app_map(ctx, &loaded, "explore").await?;
            mapplan::converge_cmd(&loaded, ctx.json)?;
            Ok(ExitCode::SUCCESS)
        }
        MapAction::Model => {
            let loaded = config::load(config_path)?;
            ensure_app_map(ctx, &loaded, "explore").await?;
            let (app_map, _) = map::load_snapshot(&loaded.root, &loaded.config)?;
            let output = serde_json::to_value(map::shadow_model(&app_map)?)?;
            ctx.emit(&output);
            Ok(ExitCode::SUCCESS)
        }
        MapAction::Budget { base } => {
            let loaded = config::load(config_path)?;
            ensure_app_map(ctx, &loaded, "explore").await?;
            let (app_map, visits) = map::load_snapshot(&loaded.root, &loaded.config)?;
            let output = serde_json::to_value(map::budget_advice(&app_map, &visits, base))?;
            ctx.emit(&output);
            Ok(ExitCode::SUCCESS)
        }
        MapAction::SuggestContracts => {
            let loaded = config::load(config_path)?;
            ensure_app_map(ctx, &loaded, "explore").await?;
            let (app_map, _) = map::load_snapshot(&loaded.root, &loaded.config)?;
            let output = serde_json::to_value(map::contract_drafts(&app_map)?)?;
            ctx.emit(&output);
            Ok(ExitCode::SUCCESS)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_commit_policy_preserves_last_good_and_rejects_partial_bootstrap() {
        let stale = map::MapFreshness::Stale(vec!["application source changed"]);
        assert_eq!(
            scan_map_commit(&map::MapFreshness::Current, false),
            Some(false)
        );
        assert_eq!(scan_map_commit(&map::MapFreshness::Missing, false), None);
        assert_eq!(scan_map_commit(&stale, false), None);
        assert_eq!(
            scan_map_commit(&map::MapFreshness::Missing, true),
            Some(false)
        );
        assert_eq!(scan_map_commit(&stale, true), Some(true));
    }
}
