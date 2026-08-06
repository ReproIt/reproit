use super::*;

/// How a cloud-pulled session replayed. The key distinction `reproduce` must
/// make: "replayed without reproducing" (the bug did NOT fire, so it is likely
/// data-dependent) is NOT the same as "could not replay" (the app drifted since
/// the session, so this run is no verdict on the bug at all). The old code
/// collapsed both into "not_reproduced" and also counted any process failure as
/// reproduced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReproVerdict {
    Reproduced,
    NotReproduced,
    Stale,
    Flaky,
    CouldNotReplay,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PullContinuation {
    ReplayFollows,
    SavedOnly,
}

fn pull_next_step(as_name: &str, continuation: PullContinuation) -> Option<String> {
    match continuation {
        PullContinuation::ReplayFollows => None,
        PullContinuation::SavedOnly => Some(format!(
            "Next: run `reproit @{as_name}` to execute the saved replay against this target."
        )),
    }
}

pub(crate) fn print_pull_next_step(as_name: &str, json: bool, continuation: PullContinuation) {
    if json {
        return;
    }
    if let Some(message) = pull_next_step(as_name, continuation) {
        println!("\n{message}");
    }
}

fn candidate_fix_next_step(as_name: &str) -> String {
    format!("Next: test a candidate fix by running `reproit @{as_name}` against the fixed target.")
}

/// Classify a reproduce run from `reproit check`'s deterministic verdict (its
/// `--json` `outcome`), falling back to its exit code (1 fail / 2 flaky / 3
/// stale / 0 pass) if the JSON is unreadable.
pub(crate) fn classify_repro(outcome: Option<&str>, exit_code: Option<i32>) -> ReproVerdict {
    match outcome {
        Some("fail") => ReproVerdict::Reproduced,
        Some("pass") => ReproVerdict::NotReproduced,
        Some("stale") => ReproVerdict::Stale,
        Some("flaky") => ReproVerdict::Flaky,
        _ => match exit_code {
            Some(1) => ReproVerdict::Reproduced,
            Some(2) => ReproVerdict::Flaky,
            Some(3) => ReproVerdict::Stale,
            Some(0) => ReproVerdict::NotReproduced,
            _ => ReproVerdict::CouldNotReplay,
        },
    }
}

/// Spawn the private single-repro route, read its deterministic verdict, print
/// a human reproduction summary, and return the classification (so callers can
/// report it back to the cloud). Used by `reproduce_bucket`, where `<target>`
/// is the just-pulled repro's alias.
pub(crate) struct ReplayEvidence {
    pub(crate) verdict: ReproVerdict,
    pub(crate) cell_receipt: Option<reproit_protocol::CellReceipt>,
}

fn run_check_and_classify(
    root: &std::path::Path,
    target: &str,
    context_hint: Option<&Value>,
    record_video: bool,
    flicker: bool,
) -> Result<ReplayEvidence> {
    println!("\nRunning the replay ({target})...");
    let exe = std::env::current_exe()?;
    let mut check_args = vec!["check", "--repro-id", target, "--json"];
    if record_video {
        check_args.push("--record-video");
    }
    if flicker {
        check_args.push("--flicker");
    }
    let out = std::process::Command::new(exe)
        .args(check_args)
        // Reproduction may have been launched from any directory with
        // `--config /path/to/app/reproit.yaml`. Run the private check from the
        // loaded app root so it resolves that same config and local artifacts.
        .current_dir(root)
        .output()
        .context("spawning reproit check")?;
    let log = String::from_utf8_lossy(&out.stdout);
    // Use `check`'s deterministic verdict (its --json `outcome`) rather than
    // grepping, so "replayed without reproducing" and "could not replay" are distinct.
    let result_json = log
        .find('{')
        .zip(log.rfind('}'))
        .filter(|(i, j)| j > i)
        .and_then(|(i, j)| serde_json::from_str::<serde_json::Value>(&log[i..=j]).ok());
    let outcome = result_json
        .as_ref()
        .and_then(|value| value["outcome"].as_str().map(String::from));
    let cell_receipt = result_json
        .as_ref()
        .and_then(|value| value["runs"].as_array())
        .and_then(|runs| {
            runs.iter().find_map(|run| {
                serde_json::from_value::<reproit_protocol::CellReceipt>(
                    run.get("cellReceipt")?.clone(),
                )
                .ok()
            })
        })
        .filter(|receipt| receipt.cleanup == reproit_protocol::CleanupStatus::Verified);
    let marker = log
        .lines()
        .find(|l| l.contains("EXCEPTION CAUGHT"))
        .unwrap_or("");
    // A real `check` run always emits its JSON verdict (even on pass) or an
    // EXCEPTION marker. NEITHER present means the replay never started -- e.g.
    // `check` could not resolve the repro/journey and exited 1 during setup.
    // Without this guard, classify_repro's exit-code fallback reads that setup
    // exit-1 as `Reproduced` and prints a FALSE "REPRODUCED" though nothing ran.
    if outcome.is_none() && marker.is_empty() {
        println!(
            "COULD NOT RUN the replay: `check {target}` produced no verdict (exit {:?}); this is \
             a setup error (the repro/journey did not resolve), not a reproduction.",
            out.status.code()
        );
        return Ok(ReplayEvidence {
            verdict: ReproVerdict::CouldNotReplay,
            cell_receipt: None,
        });
    }
    let verdict = classify_repro(outcome.as_deref(), out.status.code());
    match &verdict {
        ReproVerdict::Reproduced => {
            println!("REPRODUCED: the replay re-triggered the failure in this build. {marker}");
        }
        ReproVerdict::NotReproduced => {
            println!(
                "NOT REPRODUCED: the replay completed, but the failure did not fire. This \
                 attempt is not proof of a fix; compare the captured production context."
            );
            if let Some(ctx) = context_hint {
                println!("  -> synthesize from context: {ctx}");
            }
        }
        ReproVerdict::Stale => {
            println!(
                "COULD NOT REPLAY (stale): the app changed since this session, so a targeted \
                 control is gone. This is NOT a verdict on the bug. Retry so reproit refreshes \
                 its internal model; the bug may also be fixed by the UI change."
            );
        }
        ReproVerdict::Flaky => {
            println!(
                "FLAKY: the failure reproduced inconsistently across replays (an app race), not a \
                 successful non-reproduction."
            );
        }
        ReproVerdict::CouldNotReplay => {
            println!("Could not classify the replay (no verdict from `reproit check`).");
        }
    }
    Ok(ReplayEvidence {
        verdict,
        cell_receipt,
    })
}

/// Bucket-first production reproduction, the ONE pull -> save -> confirm
/// spelling shared by `reproit bkt_...` (`app` None: account-global bucket
/// resolution), the MCP reproduce dispatch (`__cloud-internal
/// __replay-dispatch --run`, `app` known), and `cloud pull` (`run` false:
/// save only, no confirmation replay). It materializes the content-addressed
/// bucket as a first-class LOCAL repro named `as_name`, then (with `run`)
/// `check`s it, so the pulled repro carries its property-matched fixture and
/// replays exactly as a kept one.
///
/// A `run` verdict is reported back to the cloud (POST .../replay-results):
/// that is the trust loop the bucket package's `howto` promises, and it is what
/// flips the bucket's reproduction state in the dashboard. `run_id` carries a
/// hosted dispatch's ledger id back so the cloud_runs row completes (CI runs
/// pass it).
#[allow(clippy::too_many_arguments)]
pub async fn reproduce_bucket(
    root: &std::path::Path,
    app: Option<&str>,
    bucket: &str,
    as_name: &str,
    run: bool,
    run_id: Option<i64>,
    record_video: bool,
    flicker: bool,
    json: bool,
    cloud: Option<String>,
    key: Option<String>,
) -> Result<ReproVerdict> {
    // Pull is the ONE cloud boundary: it writes .reproit/repros/<id>/{meta,replay}
    // (fixture folded in) and prints the save summary.
    let app = pull_and_save(root, app, bucket, as_name, json, cloud.clone(), key.clone()).await?;
    let continuation = if run {
        PullContinuation::ReplayFollows
    } else {
        PullContinuation::SavedOnly
    };
    print_pull_next_step(as_name, json, continuation);
    report_reproduction(
        root,
        &app,
        bucket,
        as_name,
        run,
        run_id,
        record_video,
        flicker,
        cloud,
        key,
    )
    .await
}

/// Pull and replay a tester capture without changing its Cloud confirmation
/// state. The caller reports only after shrinking and a final deterministic
/// validation, so an intermediate replay can never enter the confirmed feed.
#[allow(clippy::too_many_arguments)]
pub async fn verify_tester_capture(
    root: &std::path::Path,
    app: &str,
    bucket: &str,
    as_name: &str,
    json: bool,
    cloud: Option<String>,
    key: Option<String>,
) -> Result<ReplayEvidence> {
    pull_and_save(root, Some(app), bucket, as_name, json, cloud, key).await?;
    print_pull_next_step(as_name, json, PullContinuation::ReplayFollows);
    run_check_and_classify(root, as_name, None, false, false)
}

/// Publish the final tester-capture verdict after local verification is done.
#[allow(clippy::too_many_arguments)]
pub async fn report_tester_capture(
    app: &str,
    bucket: &str,
    local_repro_id: &str,
    verdict: ReproVerdict,
    runs: u64,
    cell_receipt: Option<&reproit_protocol::CellReceipt>,
    cloud: Option<String>,
    key: Option<String>,
) -> Result<bool> {
    let Some(cell_receipt) = cell_receipt else {
        return Ok(false);
    };
    let status = match verdict {
        ReproVerdict::Reproduced => "reproduced",
        ReproVerdict::NotReproduced => "not_reproduced",
        ReproVerdict::Stale => "stale",
        ReproVerdict::Flaky => "flaky",
        ReproVerdict::CouldNotReplay => return Ok(false),
    };
    let body = serde_json::json!({
        "mode": "authoritative",
        "status": status,
        "runs": runs,
        "failures": if status == "reproduced" { runs } else { 0 },
        "localReproId": local_repro_id,
        "where": "local",
        "cellReceipt": cell_receipt,
    });
    Cloud::new(cloud, key)
        .post(
            &format!("/v1/apps/{app}/buckets/{bucket}/replay-results"),
            &body,
        )
        .await?;
    Ok(true)
}

pub(crate) async fn report_diagnostic_session(
    cloud_base: &str,
    app: &str,
    bucket: &str,
    occurrence_id: &str,
    cell_receipt: &reproit_protocol::CellReceipt,
    diagnostic_receipt: &reproit_protocol::DiagnosticReceipt,
) -> Result<()> {
    let (configured_cloud, key) = matching_cloud_origin(cloud_base)?;
    let body = serde_json::json!({
        "mode": "diagnostic",
        "status": "stale",
        "runs": 0,
        "failures": 0,
        "localReproId": occurrence_id,
        "where": "local",
        "cellReceipt": cell_receipt,
        "diagnosticReceipt": diagnostic_receipt,
    });
    Cloud::new(Some(configured_cloud), key)
        .post(
            &format!("/v1/apps/{app}/buckets/{bucket}/replay-results"),
            &body,
        )
        .await?;
    Ok(())
}

pub(crate) async fn report_plan_run(
    cloud_base: &str,
    app: &str,
    bucket: &str,
    occurrence_id: &str,
    run: &crate::adapters::execution::PlanRun,
) -> Result<()> {
    use crate::domain::execution::ExecutionVerdict;
    let (status, runs, failures) = match run.verdict {
        ExecutionVerdict::Reproduced => ("reproduced", 1, 1),
        ExecutionVerdict::NotReproduced => ("not_reproduced", 1, 0),
        ExecutionVerdict::Flaky => ("flaky", 1, 0),
        ExecutionVerdict::Stale
        | ExecutionVerdict::Incomplete
        | ExecutionVerdict::Unsupported
        | ExecutionVerdict::DifferentFailure
        | ExecutionVerdict::InfrastructureFailed => ("stale", 0, 0),
    };
    if !run.authoritative {
        anyhow::bail!("diagnostic plan run cannot be reported as authoritative");
    }
    let cell_receipt = run
        .cell_receipt
        .as_ref()
        .context("authoritative plan run did not produce a cell receipt")?;
    if cell_receipt.cleanup != reproit_protocol::CleanupStatus::Verified {
        anyhow::bail!("cell cleanup was not verified, so no authoritative result can be uploaded");
    }
    let (configured_cloud, key) = matching_cloud_origin(cloud_base)?;
    let body = serde_json::json!({
        "mode": "authoritative",
        "status": status,
        "runs": runs,
        "failures": failures,
        "localReproId": occurrence_id,
        "where": "local",
        "cellReceipt": cell_receipt,
    });
    Cloud::new(Some(configured_cloud), key)
        .post(
            &format!("/v1/apps/{app}/buckets/{bucket}/replay-results"),
            &body,
        )
        .await?;
    Ok(())
}

fn matching_cloud_origin(cloud_base: &str) -> Result<(String, Option<String>)> {
    let (configured_cloud, key) = crate::workflows::cloud::cloud_creds(None, None);
    let configured_cloud = configured_cloud.unwrap_or_else(|| "https://cloud.reproit.com".into());
    if configured_cloud.trim_end_matches('/') != cloud_base.trim_end_matches('/') {
        anyhow::bail!(
            "occurrence came from a different Cloud origin; refusing to send credentials"
        );
    }
    Ok((configured_cloud, key))
}

#[allow(clippy::too_many_arguments)]
async fn report_reproduction(
    root: &std::path::Path,
    app: &str,
    bucket: &str,
    as_name: &str,
    run: bool,
    run_id: Option<i64>,
    record_video: bool,
    flicker: bool,
    cloud: Option<String>,
    key: Option<String>,
) -> Result<ReproVerdict> {
    if !run {
        return Ok(ReproVerdict::CouldNotReplay);
    }
    // Reuse the standard local verification by alias; no context hint (the pulled
    // repro carries its own fixture, so a CLEAN verdict is a genuine no-repro).
    let evidence = run_check_and_classify(root, as_name, None, record_video, flicker)?;
    let status = match evidence.verdict {
        ReproVerdict::Reproduced => "reproduced",
        ReproVerdict::NotReproduced => "not_reproduced",
        ReproVerdict::Stale => "stale",
        ReproVerdict::Flaky => "flaky",
        // No verdict = nothing to report; the run never happened.
        ReproVerdict::CouldNotReplay => return Ok(ReproVerdict::CouldNotReplay),
    };
    let Some(cell_receipt) = evidence.cell_receipt.as_ref() else {
        println!(
            "The local verdict was not uploaded because this replay did not produce a verified \
             execution-cell receipt."
        );
        return Ok(evidence.verdict);
    };
    let mut body = serde_json::json!({
        "mode": "authoritative",
        "status": status,
        "runs": 1,
        "failures": if status == "reproduced" { 1 } else { 0 },
        "localReproId": as_name,
        "where": if run_id.is_some() { "ci" } else { "local" },
        "cellReceipt": cell_receipt,
    });
    if let Some(id) = run_id {
        body["runId"] = serde_json::json!(id);
    }
    let c = Cloud::new(cloud, key);
    match c
        .post(
            &format!("/v1/apps/{app}/buckets/{bucket}/replay-results"),
            &body,
        )
        .await
    {
        Ok(_) => println!("Reported the verdict to the cloud: {status} (bucket {bucket})."),
        // Best-effort: the local reproduction stands even if the report fails.
        Err(e) => println!("Could not report the verdict to the cloud: {e}"),
    }
    if matches!(&evidence.verdict, ReproVerdict::Reproduced) {
        println!("\n{}", candidate_fix_next_step(as_name));
    }
    Ok(evidence.verdict)
}

/// What a pulled cloud package materializes into LOCALLY: the same on-disk
/// artifacts `keep` writes (`meta.json` + `replay.json`), so a pulled repro is
/// byte-identical in SHAPE to a kept one and `check` reads it unchanged. This
/// is the pure core of production materialization: a replay-package JSON in, a
/// `Meta` + action sequence + property-matched fixture out, with no network and
/// no filesystem. The boundary is one explicit verb; once materialized, the
/// repro is local-first-class.
///
/// The `fixture` carries the property-matched replay data (tier 3) synthesized
/// from the package's `fixtureSpec`: the locale + per-field concrete values a
/// data-dependent prod bug needs. `build_replay_json` folds it into replay.json
/// so it flows through `check` to the runner, NOT just sits in meta.
pub struct PulledRepro {
    pub meta: repro::Meta,
    pub actions: Vec<String>,
    pub fixture: crate::domain::fixture::Fixture,
    pub capsule: Option<crate::domain::capsule::Capsule>,
    pub plan: Option<reproit_protocol::ReproductionPlan>,
    pub package: Option<reproit_protocol::ReproductionPackage>,
}

/// Build the replay.json a pulled (or kept) repro stores on disk, in the EXACT
/// shape `check_repro` reads and forwards verbatim to the runner's fuzz config:
/// `{ "seed", "replay", [inputs], [locale] }`. The `inputs`/`locale` keys are
/// the property-matched fixture (`Fixture::to_config`), spread at the TOP LEVEL
/// so the web/RN/native runners read them per-seed (they read `inputs` off each
/// seed config; `check_repro` resolves a top-level `locale` to
/// `REPROIT_LOCALE`). This is the SAME shape `reproduce` writes into
/// `.reproit/tmp/fuzz_config.json`, so a pulled repro and a `reproduce`d one
/// drive the runner identically.
pub fn build_replay_json(
    seed: u64,
    actions: &[String],
    fixture: &crate::domain::fixture::Fixture,
) -> Value {
    let mut m = serde_json::Map::new();
    m.insert("seed".to_string(), serde_json::json!(seed));
    m.insert("replay".to_string(), serde_json::json!(actions));
    if !fixture.is_empty() {
        // Spread the fixture's `inputs`/`locale` at the top level, matching the
        // shape `reproduce` builds for the fuzz config (so the runner consumes
        // them the same way on a pulled repro as on a `reproduce`d one).
        if let Some(obj) = fixture.to_config().as_object() {
            for (k, v) in obj {
                m.insert(k.clone(), v.clone());
            }
        }
    }
    Value::Object(m)
}

/// Materialize a cloud replay package into a local saved repro, EXACTLY as
/// `keep` would write one.
///
/// Field mapping (faithful to `keep_repro` in main.rs):
///   - `replay`      -> the action sequence (PII-safe
///     `tap:`/`key:`/`type:<sel>=<class>`).
///   - `seed`        -> the package's `seed` if present, else 0 (cloud sessions
///     are deterministic replays, not seeded fuzz runs).
///   - `id`          -> the content hash over (seed + normalized actions), the
///     SAME `repro_id` `keep` uses (self-deduping across machines).
///   - `alias`       -> the explicit `--as <name>`.
///   - `trigger_index` -> the replay length (the finding fired after performing
///     all of them), mirroring `keep`.
///   - `trigger_sig` -> the package's `crashSig` (or `startSig` fallback) when
///     present, so `check` can re-confirm the same finding.
///   - `oracle`      -> the package finding identity or stored oracle category.
///   - `status`      -> quarantined (a fresh save, like a fresh keep).
pub fn materialize_pull(pkg: &Value, as_name: &str, created: &str) -> Result<PulledRepro> {
    let typed_package: Option<reproit_protocol::ReproductionPackage> = pkg
        .get("reproductionPackage")
        .filter(|value| value.is_object())
        .map(|value| serde_json::from_value(value.clone()))
        .transpose()
        .context("cloud package contains an invalid typed reproduction package")?;
    if let Some(package) = &typed_package {
        package
            .validate()
            .map_err(|error| anyhow::anyhow!("cloud reproduction package is invalid: {error}"))?;
        if package.assessment.status != reproit_protocol::AssessmentStatus::Eligible {
            let blockers = package
                .assessment
                .unresolved
                .iter()
                .map(|unresolved| {
                    format!(
                        "{}: {}",
                        unresolved.requirement_id,
                        unresolved.detail.trim()
                    )
                })
                .collect::<Vec<_>>();
            anyhow::bail!(
                "occurrence {} is {:?}; missing reproduction input: {}",
                package.occurrence.occurrence_id,
                package.assessment.status,
                blockers.join("; ")
            );
        }
    }
    let oracle = pkg["findingIdentity"]["oracle"]
        .as_str()
        .or_else(|| pkg["context"]["oracle"].as_str())
        .unwrap_or("crash")
        .to_string();
    let capsule_value = typed_package
        .as_ref()
        .and_then(|package| package.capsule.as_ref())
        .or_else(|| pkg.get("capsule"));
    let mut capsule: Option<crate::domain::capsule::Capsule> = capsule_value
        .filter(|value| value.is_object())
        .map(|value| serde_json::from_value(value.clone()))
        .transpose()
        .context("cloud package contains an invalid causal capsule")?;
    if let Some(capsule) = &mut capsule {
        crate::domain::capsule::redact_capsule(
            capsule,
            &crate::domain::capsule::RedactionPolicy::default(),
        );
        capsule.finalize_id()?;
        let missing = capsule.missing_required_capabilities();
        if !missing.is_empty() {
            anyhow::bail!(
                "cloud capsule is incomplete; missing captured capability: {}",
                missing.join(", ")
            );
        }
        let missing_replay = capsule.missing_required_replay_capabilities();
        if !missing_replay.is_empty() {
            anyhow::bail!(
                "cloud capsule is not hermetically replayable; missing capability: {}",
                missing_replay.join(", ")
            );
        }
    }
    let mut actions: Vec<String> = typed_package
        .as_ref()
        .and_then(|package| package.legacy.as_ref())
        .map(|legacy| legacy.actions.clone())
        .or_else(|| {
            pkg["replay"].as_array().map(|actions| {
                actions
                    .iter()
                    .filter_map(|action| action.as_str().map(String::from))
                    .collect()
            })
        })
        .unwrap_or_default();
    if actions.is_empty() {
        if let Some(capsule) = &capsule {
            actions = capsule
                .actions
                .iter()
                .map(|action| action.action.clone())
                .collect();
        }
    }
    let plan = typed_package
        .as_ref()
        .and_then(|package| package.plan.clone());
    if actions.is_empty() && plan.is_none() && oracle != "tester-capture" {
        anyhow::bail!(
            "the cloud package has neither a trusted reproduction plan nor legacy replay actions"
        );
    }
    let seed = pkg["seed"].as_u64().unwrap_or(0);
    // A plan-backed pull is identified by the occurrence it preserves, so the
    // guard survives a mechanism re-pin; an action-replay pull keeps its
    // seed+actions identity.
    // Anchor an action-less guard on the occurrence it preserves, so it
    // survives a mechanism re-pin. A tester capture carries no typed package
    // and no plan, so it keeps the cloud's own occurrence reference; only a
    // pull with neither has nothing stable to be identified by.
    let id = if actions.is_empty() {
        let occurrence_id = typed_package
            .as_ref()
            .map(|package| package.occurrence.occurrence_id.clone())
            .or_else(|| {
                pkg["occurrenceId"]
                    .as_str()
                    .or_else(|| pkg["bucketId"].as_str())
                    .map(str::to_string)
            })
            .context("an action-less pull carries no occurrence to identify its guard by")?;
        repro::guard_repro_id(&occurrence_id)
    } else {
        repro::repro_id(seed, &actions)
    };
    // The crash signature re-confirms the SAME finding on replay; fall back to the
    // session's start sig, then None (the trigger_index does the work alone).
    let typed_legacy = typed_package
        .as_ref()
        .and_then(|package| package.legacy.as_ref());
    let trigger_sig = typed_legacy
        .and_then(|legacy| legacy.crash_signature.as_deref())
        .or_else(|| typed_legacy.and_then(|legacy| legacy.start_signature.as_deref()))
        .or_else(|| pkg["crashSig"].as_str())
        .or_else(|| pkg["startSig"].as_str())
        .map(String::from)
        .filter(|s| !s.is_empty());
    let meta = repro::Meta {
        id,
        alias: Some(as_name.to_string()),
        status: repro::Status::Quarantined,
        seed,
        created: created.to_string(),
        last_checked: None,
        last_result: None,
        trigger_index: Some(repro::normalize_actions(&actions).len()),
        trigger_sig,
        trigger_selector: None,
        trigger_fingerprint: None,
        oracle: Some(oracle),
        record_url: None,
        record_action: None,
        requires: None,
    };
    // Property-matched replay (tier 3): synthesize the concrete locale + per-field
    // values from the cloud's `fixtureSpec`, the SAME way `reproduce` does, so a
    // data-dependent prod bug (a 312-char unicode name, an RTL field, a specific
    // locale/role/plan) actually reproduces under a later `check`. Empty spec ->
    // empty fixture (a path-only repro), so this is inert for non-data bugs.
    let fixture_value = typed_legacy
        .map(|legacy| &legacy.fixture)
        .unwrap_or(&pkg["fixtureSpec"]);
    let fixture = crate::domain::fixture::synthesize(fixture_value);
    Ok(PulledRepro {
        meta,
        actions,
        fixture,
        capsule,
        plan,
        package: typed_package,
    })
}

/// Download a cloud bucket as a first-class local repro; the ONE pull -> save
/// spelling behind every reproduce path.
///
/// This is the ONE cloud boundary in the check loop: it fetches the bucket's
/// replay package (app-scoped when the caller knows the app, account-global
/// resolution otherwise), materializes it the way `keep` does, and writes
/// `.reproit/repros/<id>/{meta,replay}.json`. After this, `reproit check
/// <name>` runs the STANDARD local, network-free verification and `reproit
/// repros` lists it -- indistinguishable from a locally found repro. Returns
/// the owning app id.
async fn pull_and_save(
    root: &std::path::Path,
    app: Option<&str>,
    bucket: &str,
    as_name: &str,
    json: bool,
    cloud: Option<String>,
    key: Option<String>,
) -> Result<String> {
    let (app, pkg) = fetch_package(app, bucket, cloud, key).await?;
    persist_pulled_package(root, &app, bucket, as_name, json, &pkg)?;
    Ok(app)
}

/// Fetch one bucket package without persisting it. With a known `app`, the
/// content-addressed app route is authoritative (its error propagates);
/// otherwise resolve globally via `fetch_bucket_package`.
async fn fetch_package(
    app: Option<&str>,
    bucket: &str,
    cloud: Option<String>,
    key: Option<String>,
) -> Result<(String, Value)> {
    let Some(app) = app else {
        return fetch_bucket_package(bucket, cloud, key).await;
    };
    let pkg = Cloud::new(cloud, key)
        .get(&format!("/v1/apps/{app}/buckets/{bucket}"))
        .await?;
    Ok((app.to_string(), pkg))
}

/// Fetch one production bucket package (selected-project route first, global
/// fallback) without persisting it. Backend inspection uses this to look for a
/// `context.reproitCapture` payload before the UI pull path materializes a
/// repro.
pub async fn fetch_bucket_package(
    bucket: &str,
    cloud: Option<String>,
    key: Option<String>,
) -> Result<(String, Value)> {
    let c = Cloud::new(cloud, key);
    let selected = crate::adapters::cloud_profile::load_cloud_app(
        &crate::adapters::cloud_profile::token_path(),
    );
    if let Some(app) = selected {
        if let Ok(pkg) = c.get(&format!("/v1/apps/{app}/buckets/{bucket}")).await {
            return Ok((app, pkg));
        }
    }
    let pkg = c.get(&format!("/v1/buckets/{bucket}")).await?;
    let app = pkg["appId"]
        .as_str()
        .context("cloud bucket package omitted appId")?
        .to_string();
    Ok((app, pkg))
}

fn persist_pulled_package(
    root: &std::path::Path,
    app: &str,
    bucket: &str,
    as_name: &str,
    json: bool,
    pkg: &Value,
) -> Result<()> {
    let source = format!("bucket {bucket}");
    let expected = pkg["expectedError"]
        .as_str()
        .or_else(|| pkg["message"].as_str())
        .map(first_line)
        .unwrap_or("(unknown)");

    let pulled = materialize_pull(pkg, as_name, &chrono::Local::now().to_rfc3339())?;
    let meta = &pulled.meta;

    // Write the SAME two artifacts `keep` writes, so `check` reads it unchanged:
    // replay.json for the action sequence (PLUS the property-matched fixture's
    // inputs/locale when the bug is data-dependent, so it flows through `check` to
    // the runner), meta.json for the identity + trigger context + alias.
    let dir = repro::repro_dir(root, &meta.id);
    std::fs::create_dir_all(&dir)?;
    let replay = build_replay_json(meta.seed, &pulled.actions, &pulled.fixture);
    std::fs::write(
        dir.join("replay.json"),
        serde_json::to_string_pretty(&replay)?,
    )
    .with_context(|| format!("writing {}", dir.join("replay.json").display()))?;
    repro::save_meta(root, meta)?;
    std::fs::write(
        dir.join("cloud.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "appId": app,
            "bucketId": bucket,
            "bugId": pkg.get("bugId"),
            "expectedError": expected,
            "crashSig": pkg.get("crashSig"),
        }))?,
    )
    .with_context(|| format!("writing {}", dir.join("cloud.json").display()))?;
    if let Some(mut capsule) = pulled.capsule.clone() {
        let capsule_dir = capsule.persist(root)?;
        std::fs::write(dir.join("capsule-id"), &capsule.id)?;
        if !capsule_dir.join("capsule.enc").is_file() {
            anyhow::bail!("failed to materialize cloud causal capsule");
        }
    }
    if let Some(plan) = &pulled.plan {
        std::fs::write(dir.join("plan.json"), serde_json::to_string_pretty(plan)?)
            .with_context(|| format!("writing {}", dir.join("plan.json").display()))?;
    }
    if let Some(package) = &pulled.package {
        std::fs::write(
            dir.join("package.json"),
            serde_json::to_string_pretty(package)?,
        )
        .with_context(|| format!("writing {}", dir.join("package.json").display()))?;
    }

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "command": "production bucket pull",
                "app": app,
                "bucket": bucket,
                "bugId": pkg.get("bugId"),
                "id": repro::display_repro_id(&meta.id),
                "kind": "repro",
                "alias": as_name,
                "status": meta.status.as_str(),
                "expected": expected,
                "signature": meta.trigger_sig,
                "actions": pulled.actions,
                "plan": pulled.plan.as_ref().map(|plan| &plan.id),
                "fixture": (!pulled.fixture.is_empty()).then(|| pulled.fixture.summary()),
                "dir": dir.to_string_lossy(),
            }))?
        );
        return Ok(());
    }
    println!("Pulled {source} from '{app}' as a local repro.");
    if let Some(bug_id) = pkg["bugId"].as_str() {
        println!("  structural bug: {bug_id}");
    }
    println!("  expected:  {expected}");
    if let Some(sig) = &meta.trigger_sig {
        println!("  signature: {sig}");
    }
    if let Some(plan) = &pulled.plan {
        println!("  plan:      {} ({:?})", plan.id, plan.destination);
    }
    if !pulled.actions.is_empty() {
        println!("  replay:    {}", pulled.actions.join(" -> "));
    }
    if !pulled.fixture.is_empty() {
        println!("  fixture:   {}", pulled.fixture.summary());
    }
    println!(
        "  saved:     {} ({}, alias {})",
        repro::display_repro_id(&meta.id),
        meta.status.as_str(),
        as_name
    );
    println!("  files:     {}", dir.join("meta.json").display());
    Ok(())
}

#[cfg(test)]
mod messaging_tests {
    use super::*;

    #[test]
    fn combined_bucket_run_does_not_tell_the_user_to_run_again() {
        assert_eq!(
            pull_next_step("bkt_checkout", PullContinuation::ReplayFollows),
            None
        );
    }

    #[test]
    fn pull_only_guidance_runs_the_saved_replay_without_assuming_source_access() {
        let message = pull_next_step("bkt_checkout", PullContinuation::SavedOnly).unwrap();
        assert_eq!(
            message,
            "Next: run `reproit @bkt_checkout` to execute the saved replay against this target."
        );
        assert!(!message.contains("commit"));
        assert!(!message.contains("with the fix"));
    }

    #[test]
    fn reproduced_guidance_points_to_a_future_fixed_target() {
        let message = candidate_fix_next_step("bkt_checkout");
        assert_eq!(
            message,
            "Next: test a candidate fix by running `reproit @bkt_checkout` against the fixed target."
        );
        assert!(!message.contains("commit"));
    }
}
