use super::*;

/// How a cloud-pulled session replayed. The key distinction `reproduce` must
/// make: "replayed clean" (the bug did NOT fire, so it is likely
/// data-dependent) is NOT the same as "could not replay" (the app drifted since
/// the session, so this run is no verdict on the bug at all). The old code
/// collapsed both into "clean" and also counted any process failure as
/// reproduced.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ReproVerdict {
    Reproduced,
    Clean,
    Stale,
    Flaky,
    Unknown,
}

/// Classify a reproduce run from `reproit check`'s deterministic verdict (its
/// `--json` `outcome`), falling back to its exit code (1 fail / 2 flaky / 3
/// stale / 0 pass) if the JSON is unreadable.
pub(crate) fn classify_repro(outcome: Option<&str>, exit_code: Option<i32>) -> ReproVerdict {
    match outcome {
        Some("fail") => ReproVerdict::Reproduced,
        Some("pass") => ReproVerdict::Clean,
        Some("stale") => ReproVerdict::Stale,
        Some("flaky") => ReproVerdict::Flaky,
        _ => match exit_code {
            Some(1) => ReproVerdict::Reproduced,
            Some(2) => ReproVerdict::Flaky,
            Some(3) => ReproVerdict::Stale,
            Some(0) => ReproVerdict::Clean,
            _ => ReproVerdict::Unknown,
        },
    }
}

/// Spawn the private single-repro route, read its deterministic verdict, print
/// a human reproduction summary, and return the classification (so callers can
/// report it back to the cloud). Used by `reproduce_bucket`, where `<target>`
/// is the just-pulled repro's alias.
fn run_check_and_classify(
    root: &std::path::Path,
    target: &str,
    context_hint: Option<&Value>,
) -> Result<ReproVerdict> {
    println!("\nRunning the replay ({target})...");
    let exe = std::env::current_exe()?;
    let out = std::process::Command::new(exe)
        .args(["check", "--repro-id", target, "--json"])
        // Reproduction may have been launched from any directory with
        // `--config /path/to/app/reproit.yaml`. Run the private check from the
        // loaded app root so it resolves that same config and local artifacts.
        .current_dir(root)
        .output()
        .context("spawning reproit check")?;
    let log = String::from_utf8_lossy(&out.stdout);
    // Use `check`'s deterministic verdict (its --json `outcome`) rather than
    // grepping, so "replayed clean" and "could not replay" are distinct.
    let outcome = log
        .find('{')
        .zip(log.rfind('}'))
        .filter(|(i, j)| j > i)
        .and_then(|(i, j)| serde_json::from_str::<serde_json::Value>(&log[i..=j]).ok())
        .and_then(|v| v["outcome"].as_str().map(String::from));
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
        return Ok(ReproVerdict::Unknown);
    }
    let verdict = classify_repro(outcome.as_deref(), out.status.code());
    match &verdict {
        ReproVerdict::Reproduced => {
            println!("REPRODUCED: the replay re-triggered the failure in this build. {marker}");
        }
        ReproVerdict::Clean => {
            println!(
                "NOT reproduced: the path replayed CLEAN (the bug did not fire). Likely \
                 data-dependent (the production session carried data this replay does not)."
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
                 clean reproduction."
            );
        }
        ReproVerdict::Unknown => {
            println!("Could not classify the replay (no verdict from `reproit check`).");
        }
    }
    Ok(verdict)
}

/// Bucket-first production reproduction: materialize the content-addressed
/// bucket as a first-class LOCAL repro named `as_name`, then (with `run`)
/// `check` it. This is the one-step "show me this prod bug locally" verb; it
/// REUSES the existing pull
/// + check code paths (no duplicated materialize/replay logic), so the pulled
/// repro carries its property-matched fixture and replays exactly as a kept
/// one.
///
/// A `run` verdict is reported back to the cloud (POST .../replay-results):
/// that is the trust loop the bucket package's `howto` promises, and it is what
/// flips the bucket's reproduction state in the dashboard. `run_id` carries a
/// hosted dispatch's ledger id back so the cloud_runs row completes (CI runs
/// pass it).
#[allow(clippy::too_many_arguments)]
pub async fn reproduce_bucket(
    root: &std::path::Path,
    app: &str,
    bucket: &str,
    as_name: &str,
    run: bool,
    run_id: Option<i64>,
    json: bool,
    cloud: Option<String>,
    key: Option<String>,
) -> Result<ReproVerdict> {
    // Pull is the ONE cloud boundary: it writes .reproit/repros/<id>/{meta,replay}
    // (fixture folded in) and prints the save summary + the `check` hint.
    pull(root, app, bucket, as_name, json, cloud.clone(), key.clone()).await?;
    report_reproduction(root, app, bucket, as_name, run, run_id, cloud, key).await
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
) -> Result<ReproVerdict> {
    pull(root, app, bucket, as_name, json, cloud, key).await?;
    run_check_and_classify(root, as_name, None)
}

/// Publish the final tester-capture verdict after local verification is done.
pub async fn report_tester_capture(
    app: &str,
    bucket: &str,
    local_repro_id: &str,
    verdict: ReproVerdict,
    runs: u64,
    cloud: Option<String>,
    key: Option<String>,
) -> Result<()> {
    let status = match verdict {
        ReproVerdict::Reproduced => "reproduced",
        ReproVerdict::Clean => "clean",
        ReproVerdict::Stale => "stale",
        ReproVerdict::Flaky => "flaky",
        ReproVerdict::Unknown => return Ok(()),
    };
    let body = serde_json::json!({
        "status": status,
        "runs": runs,
        "failures": if status == "reproduced" { runs } else { 0 },
        "localReproId": local_repro_id,
    });
    Cloud::new(cloud, key)
        .post(
            &format!("/v1/apps/{app}/buckets/{bucket}/replay-results"),
            &body,
        )
        .await?;
    Ok(())
}

/// Resolve a production bucket across the projects visible to the signed-in
/// account, materialize it locally, and report the replay verdict to its owning
/// project. This is the normal human path behind `reproit bkt_...`.
#[allow(clippy::too_many_arguments)]
pub async fn reproduce_bucket_global(
    root: &std::path::Path,
    bucket: &str,
    as_name: &str,
    run: bool,
    run_id: Option<i64>,
    json: bool,
    cloud: Option<String>,
    key: Option<String>,
) -> Result<ReproVerdict> {
    let app = pull_global(root, bucket, as_name, json, cloud.clone(), key.clone()).await?;
    report_reproduction(root, &app, bucket, as_name, run, run_id, cloud, key).await
}

#[allow(clippy::too_many_arguments)]
async fn report_reproduction(
    root: &std::path::Path,
    app: &str,
    bucket: &str,
    as_name: &str,
    run: bool,
    run_id: Option<i64>,
    cloud: Option<String>,
    key: Option<String>,
) -> Result<ReproVerdict> {
    if !run {
        return Ok(ReproVerdict::Unknown);
    }
    // Reuse the standard local verification by alias; no context hint (the pulled
    // repro carries its own fixture, so a CLEAN verdict is a genuine no-repro).
    let verdict = run_check_and_classify(root, as_name, None)?;
    let status = match verdict {
        ReproVerdict::Reproduced => "reproduced",
        ReproVerdict::Clean => "clean",
        ReproVerdict::Stale => "stale",
        ReproVerdict::Flaky => "flaky",
        // No verdict = nothing to report; the run never happened.
        ReproVerdict::Unknown => return Ok(ReproVerdict::Unknown),
    };
    let mut body = serde_json::json!({
        "status": status,
        "runs": 1,
        "failures": if status == "reproduced" { 1 } else { 0 },
        "localReproId": as_name,
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
    Ok(verdict)
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
    pub fixture: crate::model::fixture::Fixture,
    pub capsule: Option<crate::capsule::Capsule>,
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
    fixture: &crate::model::fixture::Fixture,
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
    let oracle = pkg["findingIdentity"]["oracle"]
        .as_str()
        .or_else(|| pkg["context"]["oracle"].as_str())
        .unwrap_or("crash")
        .to_string();
    let mut capsule: Option<crate::capsule::Capsule> = pkg
        .get("capsule")
        .filter(|value| value.is_object())
        .map(|value| serde_json::from_value(value.clone()))
        .transpose()
        .context("cloud package contains an invalid causal capsule")?;
    if let Some(capsule) = &mut capsule {
        crate::capsule::redact_capsule(capsule, &crate::capsule::RedactionPolicy::default());
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
    let mut actions: Vec<String> = pkg["replay"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
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
    if actions.is_empty() && oracle != "tester-capture" {
        anyhow::bail!(
            "the cloud package has no executable replay actions (its `replay` is empty); there is \
             nothing to reproduce locally"
        );
    }
    let seed = pkg["seed"].as_u64().unwrap_or(0);
    let id = repro::repro_id(seed, &actions);
    // The crash signature re-confirms the SAME finding on replay; fall back to the
    // session's start sig, then None (the trigger_index does the work alone).
    let trigger_sig = pkg["crashSig"]
        .as_str()
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
        oracle: Some(oracle),
        record_url: None,
        record_action: None,
    };
    // Property-matched replay (tier 3): synthesize the concrete locale + per-field
    // values from the cloud's `fixtureSpec`, the SAME way `reproduce` does, so a
    // data-dependent prod bug (a 312-char unicode name, an RTL field, a specific
    // locale/role/plan) actually reproduces under a later `check`. Empty spec ->
    // empty fixture (a path-only repro), so this is inert for non-data bugs.
    let fixture = crate::model::fixture::synthesize(&pkg["fixtureSpec"]);
    Ok(PulledRepro {
        meta,
        actions,
        fixture,
        capsule,
    })
}

/// Download a cloud bucket as a first-class local repro.
///
/// This is the ONE cloud boundary in the check loop: it fetches the bucket's
/// replay package (the content-addressed `GET /v1/apps/:app/buckets/:bucket`),
/// materializes it the way `keep` does, and writes
/// `.reproit/repros/<id>/{meta,replay}.json`. After this, `reproit check
/// <name>` runs the STANDARD local, network-free verification and `reproit
/// repros` lists it -- indistinguishable from a locally found repro.
pub async fn pull(
    root: &std::path::Path,
    app: &str,
    bucket: &str,
    as_name: &str,
    json: bool,
    cloud: Option<String>,
    key: Option<String>,
) -> Result<()> {
    let c = Cloud::new(cloud, key);
    // The content-addressed bucket endpoint (matches the content-hash model).
    let pkg = c.get(&format!("/v1/apps/{app}/buckets/{bucket}")).await?;
    persist_pulled_package(root, app, bucket, as_name, json, &pkg)
}

/// Pull a bucket without asking the user for its app id. The authenticated
/// global endpoint returns the owning app with the portable replay package.
pub async fn pull_global(
    root: &std::path::Path,
    bucket: &str,
    as_name: &str,
    json: bool,
    cloud: Option<String>,
    key: Option<String>,
) -> Result<String> {
    let c = Cloud::new(cloud, key);
    let selected = crate::crosscut::load_cloud_app(&crate::crosscut::token_path());
    let (app, pkg) = if let Some(app) = selected {
        match c.get(&format!("/v1/apps/{app}/buckets/{bucket}")).await {
            Ok(pkg) => (app, pkg),
            Err(_) => {
                let pkg = c.get(&format!("/v1/buckets/{bucket}")).await?;
                let app = pkg["appId"]
                    .as_str()
                    .context("cloud bucket package omitted appId")?
                    .to_string();
                (app, pkg)
            }
        }
    } else {
        let pkg = c.get(&format!("/v1/buckets/{bucket}")).await?;
        let app = pkg["appId"]
            .as_str()
            .context("cloud bucket package omitted appId")?
            .to_string();
        (app, pkg)
    };
    persist_pulled_package(root, &app, bucket, as_name, json, &pkg)?;
    Ok(app)
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

    let expected = pkg["expectedError"]
        .as_str()
        .or_else(|| pkg["message"].as_str())
        .map(first_line)
        .unwrap_or("(unknown)");
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
    println!("  replay:    {}", pulled.actions.join(" -> "));
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
    println!(
        "\nnow run: reproit check {as_name}\ncommit {} with the fix so CI can verify it",
        dir.display()
    );
    Ok(())
}
