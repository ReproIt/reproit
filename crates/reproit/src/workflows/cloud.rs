//! Cloud credential, project-selection, and command workflows.

use super::*;
use crate::workflows::fuzz;

/// Resolve the effective cloud (url, key) for a cloud subcommand. Precedence:
///   url:  --cloud flag > $REPROIT_CLOUD_URL > the persisted login url.
///   key:  --key flag > $REPROIT_CLOUD_KEY (the project key, sk_live_...) >
///         the persisted login key.
/// This is the single place the persisted login is read so every cloud command
/// honors it.
pub(super) fn cloud_creds(
    cloud: Option<String>,
    key: Option<String>,
) -> (Option<String>, Option<String>) {
    let persisted =
        crate::adapters::cloud_profile::load_token(&crate::adapters::cloud_profile::token_path());
    let url = cloud
        .or_else(|| std::env::var("REPROIT_CLOUD_URL").ok())
        .or_else(|| persisted.as_ref().and_then(|(_, u)| u.clone()));
    let key = key
        .or_else(|| std::env::var("REPROIT_CLOUD_KEY").ok())
        .or_else(|| persisted.as_ref().map(|(t, _)| t.clone()));
    (url, key)
}

/// Resolve the selected cloud project. Explicit flag and environment override
/// the profile written by setup; no command should make users repeatedly paste
/// an app id after they selected it once.
pub(super) fn cloud_app_id(app: Option<String>) -> Result<String> {
    app.or_else(|| std::env::var("REPROIT_CLOUD_APP").ok())
        .or_else(|| {
            crate::adapters::cloud_profile::load_cloud_app(
                &crate::adapters::cloud_profile::token_path(),
            )
        })
        .ok_or_else(|| {
            anyhow::anyhow!("no project selected: run `reproit login` and choose a project")
        })
}

pub(super) fn choose_cloud_project(
    projects: &[triage::CloudProject],
    requested: Option<&str>,
    interactive: bool,
) -> Result<Option<String>> {
    if let Some(want) = requested {
        let matches: Vec<_> = projects
            .iter()
            .filter(|p| p.app_id == want || p.name.eq_ignore_ascii_case(want))
            .collect();
        return match matches.as_slice() {
            [project] => Ok(Some(project.app_id.clone())),
            [] => anyhow::bail!("project `{want}` is not in this organization"),
            _ => anyhow::bail!("project name `{want}` is ambiguous; use its app id"),
        };
    }
    match projects {
        [] => Ok(None),
        [project] => Ok(Some(project.app_id.clone())),
        many if interactive => {
            println!("Projects:");
            for (index, project) in many.iter().enumerate() {
                println!("  {}. {} ({})", index + 1, project.name, project.app_id);
            }
            let answer = auth_prompt("Select project number", false)?;
            let index: usize = answer.trim().parse().context("enter a project number")?;
            many.get(index.saturating_sub(1))
                .map(|project| Some(project.app_id.clone()))
                .context("project number is out of range")
        }
        _ => Ok(None),
    }
}

/// Dispatch the `cloud` subcommands onto the existing triage::*/deliver::*
/// handlers. `login` persists the cloud/project key; every other command
/// resolves the key via `cloud_creds` and uses it as a bearer. Network failures
/// surface as a clear message (the triage layer bails rather than panicking).
pub(super) async fn cloud_cmd(
    config_path: Option<&std::path::Path>,
    action: CloudAction,
    json: bool,
    yes: bool,
) -> Result<()> {
    match action {
        CloudAction::Login { cloud, key, app } => {
            let url = cloud
                .or_else(|| std::env::var("REPROIT_CLOUD_URL").ok())
                .unwrap_or_else(|| "https://cloud.reproit.com".into());
            // Explicit/env keys remain available for CI. Interactive developers
            // use the browser device flow and receive an account token plus the
            // projects in their active organization.
            let injected = key.or_else(|| std::env::var("REPROIT_CLOUD_KEY").ok());
            if injected.is_none() {
                if json {
                    anyhow::bail!("browser login is interactive; omit --json or pass --key for CI");
                }
                let grant = triage::device_login(&url, true).await?;
                let interactive = std::io::IsTerminal::is_terminal(&std::io::stdin()) && !yes;
                let selected = choose_cloud_project(&grant.projects, app.as_deref(), interactive)?;
                let path = crate::adapters::cloud_profile::token_path();
                crate::adapters::cloud_profile::save_cloud_profile(
                    &path,
                    &grant.token,
                    &url,
                    selected.as_deref(),
                )?;
                println!("Signed in to organization {}.", grant.org_id);
                println!("Token stored in {}.", path.display());
                match selected {
                    Some(project) => println!("Project selected: {project}"),
                    None if grant.projects.is_empty() => {
                        println!("No projects yet. Create one in Cloud.")
                    }
                    None => println!(
                        "No project selected. Run `reproit login` in a terminal and choose one."
                    ),
                }
                return Ok(());
            }
            let token = injected.unwrap();
            // Validate BEFORE persisting: a login that stores an unusable key is a
            // worse failure mode than failing loudly now. With --app, validate
            // against the app's buckets; otherwise against /v1/me. A 401/403 fails
            // clearly (bad key); a transient network error is a soft warning (the
            // key may still be fine, so we store it and let the user retry).
            match triage::validate_login(&url, &token, app.as_deref()).await {
                Ok(desc) => {
                    let path = crate::adapters::cloud_profile::token_path();
                    crate::adapters::cloud_profile::save_cloud_profile(
                        &path,
                        &token,
                        &url,
                        app.as_deref(),
                    )?;
                    println!("cloud url:     {url}");
                    println!(
                        "cloud key:     stored ({} chars) in {}",
                        token.len(),
                        path.display()
                    );
                    println!("validated:     ok ({desc})");
                    if let Some(app) = &app {
                        println!("project:       {app} selected");
                    }
                    Ok(())
                }
                Err(e) => anyhow::bail!(
                    "login failed and no credential was stored: {e}. ReproIt only saves a key \
                     after the cloud verifies it"
                ),
            }
        }
        CloudAction::Setup {
            app,
            key,
            cloud,
            dispatch_token,
            repo,
            workflow_path,
            no_workflow,
        } => {
            // Root at the git repo top (where `.github/workflows` must live),
            // independent of any reproit.yaml (which may be nested or absent);
            // fall back to cwd when not in a git repo. The platform hint for the
            // SDK line is a best-effort read of a local config, and must NOT
            // decide the root (a config found by climbing ancestors would write
            // the workflow to the wrong tree).
            let root = triage::git_toplevel()
                .map(Ok)
                .unwrap_or_else(std::env::current_dir)?;
            let platform = config::load(config_path)
                .ok()
                .map(|l| l.config.app.platform);
            triage::setup(
                &root,
                &app,
                cloud,
                key,
                dispatch_token,
                repo,
                workflow_path,
                !no_workflow,
                platform,
            )
            .await
        }
        CloudAction::Fuzz {
            app,
            journey,
            pr,
            cloud,
            bucket,
        } => {
            // Run through the existing local fuzz engine with Cloud delivery:
            // set --cloud + --app, then post the PR comment when linked.
            let loaded = config::load(config_path)?;
            let cloud = cloud.or_else(|| std::env::var("REPROIT_CLOUD_URL").ok());
            if let Some(pr) = pr {
                // PR linking is automatic in the delivery pipeline; record it.
                println!("  linking to PR #{pr}");
            }
            let args = fuzz::FuzzArgs {
                journey,
                seed: 1,
                runs: 3,
                budget: 40,
                shrink: false,
                // Cloud buckets findings server-side; the local run delivers one.
                all: false,
                frontier: false,
                uniform: false,
                seeds_file: None,
                batch: 0,
                profile_timing: false,
                sim: false,
                confirm_on_sim: false,
                cloud,
                app: Some(app),
                app_bucket: bucket,
                post_comment: pr.is_some(),
                json: false,
                locales: Vec::new(),
                oracle_filter: crate::domain::oracle::OracleFilter::all(),
                from_prefix: None,
            };
            fuzz::fuzz(&loaded.config, &loaded.root, &args)
                .await
                .map(|_| ())
        }
        CloudAction::Buckets {
            app,
            query,
            cloud,
            key,
        } => {
            let (cloud, key) = cloud_creds(cloud, key);
            triage::buckets(&app, query.as_deref(), json, cloud, key).await
        }
        CloudAction::Findings {
            app,
            query,
            export,
            cloud,
            key,
        } => {
            let (cloud, key) = cloud_creds(cloud, key);
            if export {
                // Raw findings JSON straight from GET /v1/errors/:app, with
                // the same message filter as the rendered view.
                let v = triage::raw(&app, "", cloud, key).await?;
                let v = triage::filter_errors(v, query.as_deref());
                println!("{}", serde_json::to_string_pretty(&v)?);
                Ok(())
            } else {
                triage::find(&app, query.as_deref(), cloud, key).await
            }
        }
        CloudAction::BlastRadius {
            app,
            bucket,
            sig,
            export,
            cloud,
            key,
        } => {
            let (cloud, key) = cloud_creds(cloud, key);
            if export {
                // Raw cohorts JSON from GET /v1/errors/:app/cohorts.
                let v = triage::raw(&app, "/cohorts", cloud, key).await?;
                println!("{}", serde_json::to_string_pretty(&v)?);
                Ok(())
            } else {
                triage::explain(&app, bucket.as_deref(), sig.as_deref(), cloud, key).await
            }
        }
        CloudAction::ReplayDispatch {
            app,
            bucket,
            as_name,
            run,
            run_id,
            cloud,
            key,
        } => {
            let (cloud, key) = cloud_creds(cloud, key);
            // Bucket-first: pull -> check, in one step. Reuses the pull + check
            // code paths so the saved repro carries its fixture.
            let loaded = config::load(config_path)?;
            triage::reproduce_bucket(
                &loaded.root,
                Some(&app),
                &bucket,
                &as_name,
                run,
                run_id,
                false,
                false,
                json,
                cloud,
                key,
            )
            .await
            .map(|_| ())
        }
        CloudAction::Pull {
            app,
            bucket,
            top,
            as_name,
            cloud,
            key,
        } => {
            // Resolve the local repro store root so the pulled repro lands as a
            // first-class saved repro under .reproit/repros/, just like `keep`.
            let loaded = config::load(config_path)?;
            let (cloud, key) = cloud_creds(cloud, key);
            let bucket = match (bucket, top) {
                (Some(bucket), false) => bucket,
                (None, true) => triage::top_bucket_id(&app, cloud.clone(), key.clone()).await?,
                (None, false) => {
                    anyhow::bail!("missing bucket: pass --bucket <bkt_...> or use --top")
                }
                (Some(_), true) => unreachable!("clap conflicts_with prevents --bucket + --top"),
            };
            // The shared pull -> save -> confirm path with `run` false: save
            // only, print the saved-only next step, no confirmation replay.
            triage::reproduce_bucket(
                &loaded.root,
                Some(&app),
                &bucket,
                &as_name,
                false,
                None,
                false,
                false,
                json,
                cloud,
                key,
            )
            .await
            .map(|_| ())
        }
        CloudAction::Triage {
            app,
            bucket,
            status,
            fixed_in_build,
            assignee,
            cloud,
            key,
        } => {
            let (cloud, key) = cloud_creds(cloud, key);
            triage::triage(
                &app,
                &bucket,
                status.as_deref(),
                fixed_in_build.as_deref(),
                assignee,
                json,
                cloud,
                key,
            )
            .await
        }
        CloudAction::ResolutionEvents { app, cloud, key } => {
            let (cloud, key) = cloud_creds(cloud, key);
            triage::resolution_events(&app, json, cloud, key).await
        }
        CloudAction::Timeline {
            app,
            bucket,
            cloud,
            key,
        } => {
            let (cloud, key) = cloud_creds(cloud, key);
            triage::timeline(&app, &bucket, json, cloud, key).await
        }
        CloudAction::Diagnose {
            app,
            report,
            run,
            cloud,
            key,
        } => {
            let (cloud, key) = cloud_creds(cloud, key);
            triage::diagnose(&app, &report, run, cloud, key).await
        }
        CloudAction::Query {
            app,
            query,
            export,
            cloud,
            key,
        } => {
            // Bucket-first data out for your own analysis: GET
            // /v1/apps/:app/buckets, filtered by --query when given. With
            // --export, emit the raw JSON; otherwise render the same list as
            // `cloud buckets`.
            let (cloud, key) = cloud_creds(cloud, key);
            if export {
                let v = triage::raw_buckets(&app, cloud, key).await?;
                let v = triage::filter_buckets(v, query.as_deref());
                println!("{}", serde_json::to_string_pretty(&v)?);
                Ok(())
            } else {
                triage::buckets(&app, query.as_deref(), false, cloud, key).await
            }
        }
    }
}
