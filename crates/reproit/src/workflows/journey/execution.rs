use super::*;

#[derive(Debug, Default)]
pub struct MultiFuzzSummary {
    pub confirmed: usize,
    pub candidates: usize,
}

/// Fuzz outward from an authored multi-user checkpoint. The authored prefix is
/// immutable; only the seeded actor/action suffix is minimized. This preserves
/// business prerequisites while letting reproit own scheduling, confirmation,
/// exact finding identity, and the final compact reproduction.
pub async fn fuzz_multi_checkpoint(
    loaded: &config::Loaded,
    name_or_path: &str,
    first_seed: u64,
    runs: u32,
    budget: u32,
    confirm: bool,
) -> Result<MultiFuzzSummary> {
    let j = load_target(&loaded.root, name_or_path)?;
    if j.actors.is_empty() && !j.steps.iter().any(|s| s.actor.is_some()) {
        bail!("`{name_or_path}` is not a multi-actor journey");
    }
    let map = load_map(&loaded.root)?.ok_or_else(|| {
        anyhow::anyhow!("multi-user fuzzing needs a verified app map; run `reproit scan` first")
    })?;
    let (actors, checkpoint) = build_scenario(&loaded.root, Some(&map), &j)?;
    warn_uncovered_reset_accounts(&loaded.config, &j);

    // Never invent values or take destructive/unverified-irreversible actions.
    // The map is the structural action vocabulary shared by every backend.
    let mut transitions: Vec<(String, String, String)> = map
        .transitions
        .iter()
        .filter(|t| {
            !matches!(
                t.reversibility,
                crate::domain::appmap::Reversibility::Destructive
                    | crate::domain::appmap::Reversibility::VerifiedIrreversible
                    | crate::domain::appmap::Reversibility::ProposedIrreversible
            )
        })
        .filter(|t| !matches!(t.action, crate::domain::appmap::Action::Type { .. }))
        .map(|t| (t.from.clone(), action_str(&t.action), t.to.clone()))
        .filter(|(_, action, _)| !action.is_empty())
        .collect();
    transitions.sort();
    transitions.dedup();
    if transitions.is_empty() {
        bail!("the verified map has no safe structural actions to fuzz from this checkpoint");
    }

    // Reconstruct each actor's graph position after the authored checkpoint.
    // Exact from-state matches win; if the checkpoint entered the map through a
    // business-specific edge, its structurally matching action still anchors the
    // destination. This is guidance only. The live runner remains authoritative.
    let entry =
        entry_state(&map).ok_or_else(|| anyhow::anyhow!("the app map has no entry state"))?;
    let mut checkpoint_states: BTreeMap<String, String> =
        actors.iter().map(|a| (a.clone(), entry.clone())).collect();
    for (actor, action) in &checkpoint {
        let current = checkpoint_states
            .get(actor)
            .cloned()
            .unwrap_or_else(|| entry.clone());
        let edge = transitions
            .iter()
            .find(|(from, a, _)| from == &current && a == action)
            .or_else(|| transitions.iter().find(|(_, a, _)| a == action));
        if let Some((_, _, to)) = edge {
            checkpoint_states.insert(actor.clone(), to.clone());
        }
    }

    let mut summary = MultiFuzzSummary::default();
    let mut seen_schedules = BTreeSet::new();
    for run in 0..runs.max(1) {
        let seed = first_seed.wrapping_add(run as u64);
        let mut rng = seed as u32;
        if rng == 0 {
            rng = 0x9e37_79b9;
        }
        let mut suffix = Vec::<(String, String)>::new();
        let mut actor_states = checkpoint_states.clone();
        for _ in 0..budget {
            rng ^= rng << 13;
            rng ^= rng >> 17;
            rng ^= rng << 5;
            let actor = actors[(rng as usize) % actors.len()].clone();
            rng ^= rng << 13;
            rng ^= rng >> 17;
            rng ^= rng << 5;
            let current = actor_states.get(&actor).unwrap_or(&entry);
            let outgoing: Vec<&(String, String, String)> = transitions
                .iter()
                .filter(|(from, _, _)| from == current)
                .collect();
            if outgoing.is_empty() {
                continue;
            }
            let edge = outgoing[(rng as usize) % outgoing.len()];
            suffix.push((actor.clone(), edge.1.clone()));
            actor_states.insert(actor, edge.2.clone());
        }
        suffix = super::schedule::canonicalize(&suffix, &j.independent_actions)?;
        if !seen_schedules.insert(suffix.clone()) {
            continue;
        }
        let (log, completed, _) =
            run_tagged_scenario_once(loaded, &actors, &checkpoint, &suffix, None).await?;
        if !completed {
            continue;
        }
        let observed = crate::workflows::fuzz::finding_signatures_for_log(&loaded.config, &log);
        if observed.is_empty() {
            continue;
        }
        summary.candidates += observed.len();

        for signature in observed {
            let mut minimal = suffix.clone();
            if confirm {
                let (confirm_log, done, _) =
                    run_tagged_scenario_once(loaded, &actors, &checkpoint, &minimal, None).await?;
                if !done
                    || !crate::workflows::fuzz::finding_signatures_for_log(
                        &loaded.config,
                        &confirm_log,
                    )
                    .contains(&signature)
                {
                    continue;
                }
                // Greedy exact-identity shrink. The checkpoint is never touched.
                let mut i = 0;
                while i < minimal.len() {
                    let mut candidate = minimal.clone();
                    candidate.remove(i);
                    let (candidate_log, done, _) =
                        run_tagged_scenario_once(loaded, &actors, &checkpoint, &candidate, None)
                            .await?;
                    if done
                        && crate::workflows::fuzz::finding_signatures_for_log(
                            &loaded.config,
                            &candidate_log,
                        )
                        .contains(&signature)
                    {
                        minimal = candidate;
                    } else {
                        i += 1;
                    }
                }
            }
            let (capture_log, captured, run_dir) =
                run_tagged_scenario_once(loaded, &actors, &checkpoint, &minimal, None).await?;
            if !captured
                || !crate::workflows::fuzz::finding_signatures_for_log(&loaded.config, &capture_log)
                    .contains(&signature)
            {
                continue;
            }
            let capsule = build_multi_capsule(
                loaded,
                seed,
                &actors,
                &checkpoint,
                &minimal,
                &signature,
                &run_dir,
            )?;
            let capsule =
                shrink_multi_capsule(loaded, &actors, &checkpoint, &minimal, &signature, capsule)
                    .await?;
            persist_multi_finding(
                loaded,
                name_or_path,
                seed,
                &actors,
                &checkpoint,
                &minimal,
                &signature,
                &capsule.id,
                &j.independent_actions,
            )?;
            summary.confirmed += 1;
            println!(
                "  confirmed multi-user bug: seed {seed}, {} generated step(s)",
                minimal.len()
            );
        }
    }
    Ok(summary)
}

/// Classify one journey replay. A crash on the way is a real failure (Broke); a
/// step that could not be performed (`FUZZ:MISS`) or an assertion that no
/// longer holds (`FUZZ:ASSERT fail`) means the app diverged from what the
/// journey assumed, which is stale, not a bug (CouldNotReplay); a clean run is
/// Green.
pub(super) fn classify_run(log: &str, drive_passed: bool) -> repro::RunVerdict {
    let app_exception = log
        .lines()
        .any(|l| l.contains("EXCEPTION CAUGHT BY") && !l.contains("TEST FRAMEWORK"));
    if !drive_passed || app_exception {
        return repro::RunVerdict::Broke;
    }
    if log.contains("FUZZ:MISS ") || log.contains("FUZZ:ASSERT fail") {
        return repro::RunVerdict::CouldNotReplay;
    }
    repro::RunVerdict::Green
}

pub(super) fn journey_contract_violations(
    journey: &Journey,
    log: &str,
    actors: &[String],
) -> Vec<crate::domain::contracts::ContractViolation> {
    let observations = crate::domain::observation::from_runner_log(log, actors);
    crate::domain::contracts::evaluate_all(&journey.contracts, &observations)
}

/// Run a journey `times` times and aggregate to pass/fail/flaky/stale. Replays
/// the journey's actions through the same execution tier `check` uses for
/// repros.
pub async fn run(
    loaded: &config::Loaded,
    name: &str,
    times: u32,
    quiet: bool,
) -> Result<repro::CheckResult> {
    let j = load(&loaded.root, name)?;
    if !j.actors.is_empty() || j.steps.iter().any(|s| s.actor.is_some()) {
        return run_scenario(loaded, name, &j, times, quiet).await;
    }
    let map = load_map(&loaded.root)?;
    let plan = build_plan(&loaded.root, map.as_ref(), &j)?;
    if plan.actions.is_empty() {
        bail!("journey `{name}` has no actions to run");
    }

    // Resolve ${REPROIT_SECRET_*} placeholders host-side, so the runner types the
    // real value with no vault/secret code of its own (framework-agnostic).
    let secrets = crate::adapters::credentials::secret_env(&loaded.config.auth, &loaded.root)
        .unwrap_or_default();
    let actions: Vec<String> = plan
        .actions
        .iter()
        .map(|a| crate::adapters::credentials::resolve_placeholders(a, &secrets))
        .collect();

    // Scripted journeys are E2E: run them on the sim tier unless explicitly
    // opted into headless.
    let sim = sim_tier(&j);
    let total = times.max(1);
    let mut verdicts = Vec::new();
    for i in 1..=total {
        let (log, passed) = run_replay(loaded, &actions, i > 1, sim).await?;
        let violations = journey_contract_violations(&j, &log, &[]);
        let verdict = if violations.is_empty() {
            classify_run(&log, passed)
        } else {
            repro::RunVerdict::Broke
        };
        if !quiet {
            println!("  run {i}/{total}: {}", verdict.as_str());
            for violation in &violations {
                println!(
                    "    contract {} failed at observation {} ({})",
                    violation.contract_id, violation.boundary_index, violation.fingerprint
                );
            }
        }
        verdicts.push(verdict);
    }
    Ok(repro::CheckResult::from_verdicts(&verdicts))
}

/// Verify one configured account from a clean application run without reading
/// or rebuilding the exploration map. The login journey itself is the authored
/// contract and must use explicit actions rather than map-dependent `goto`s.
pub async fn verify_account(loaded: &config::Loaded, account: &str) -> Result<repro::CheckResult> {
    let actions = account_setup_actions(loaded, account)?;
    if actions.is_empty() {
        bail!("authentication contract for `{account}` has no actions");
    }
    let (log, passed) = run_replay(loaded, &actions, false, true).await?;
    let result = repro::CheckResult::from_verdicts(&[classify_run(&log, passed)]);
    Ok(result)
}

/// Scripted journeys are E2E by default: real sim + backend, not the in-process
/// headless tier (no backend, no sim, can't satisfy login or multi-actor).
/// `tier: headless` opts a pure-widget journey out.
pub(super) fn sim_tier(j: &Journey) -> bool {
    j.tier.as_deref() != Some("headless")
}

// ---- multi-actor ---------------------------------------------------------

/// Resolve one step to its replay action(s), for multi-actor where there is no
/// per-actor map: only `do`, `fill`, and `expect: text`/`count` are supported
/// (`goto` and `expect: state` need a single-actor map). `secret:` fills
/// resolve against the step's actor account (bound via the actor's
/// `login`/`auth`).
pub(super) fn resolve_scenario_step(
    step: &Step,
    n: usize,
    account: Option<&str>,
) -> Result<Vec<String>> {
    match (&step.do_action, &step.goto, &step.expect, &step.fill) {
        (Some(a), None, None, None) => Ok(vec![a.clone()]),
        (None, Some(_), None, None) => {
            bail!(
                "step {n}: `goto` is single-actor only (it needs the map); use explicit `do` \
                 actions"
            )
        }
        (None, None, Some(e), None) => {
            if e.state.is_some() {
                bail!("step {n}: `expect: state` is single-actor only; use `text` or `count`");
            }
            let mut out = Vec::new();
            if let Some(text) = &e.text {
                out.push(format!("assert:text={text}"));
            }
            if let Some(counts) = &e.count {
                for (finder, want) in counts {
                    out.push(format!("assert:count:{finder}={want}"));
                }
            }
            if out.is_empty() {
                bail!("step {n}: `expect` needs `text` or `count`");
            }
            Ok(out)
        }
        (None, None, None, Some(fields)) => fields
            .iter()
            .map(|(finder, value)| {
                let v = resolve_fill_value(value, account)
                    .with_context(|| format!("step {n}: fill `{finder}`"))?;
                Ok(format!("type:{finder}={v}"))
            })
            .collect(),
        _ => bail!("step {n}: a step takes exactly one of `do`/`expect`/`fill`"),
    }
}

/// Build an actor's auth prelude actions, reusing the single-actor machinery:
/// `auth(acct)` emits the `auth:<acct>` session-restore action; `login(acct)`
/// resolves the shared `login` journey with that account bound (so its
/// `secret:` fills point at the actor's credentials).
pub(super) fn actor_prelude(
    root: &Path,
    map: Option<&AppMap>,
    a: &ActorAuth,
) -> Result<Vec<String>> {
    match a.kind {
        SetupKind::Auth => Ok(vec![format!("auth:{}", a.account)]),
        SetupKind::Login => {
            let account_login = format!("login-{}", a.account);
            let login_name = if exists(root, &account_login) {
                account_login.as_str()
            } else {
                "login"
            };
            if !exists(root, login_name) {
                bail!(
                    "actor login({}) needs a verified login journey; run `reproit auth {}`",
                    a.account,
                    a.account
                );
            }
            let login = load(root, login_name)
                .with_context(|| format!("loading `login` journey for account {}", a.account))?;
            if login.setup.is_some() {
                bail!("the `login` journey must not itself declare `setup` (would recurse)");
            }
            if !login.actors.is_empty() {
                bail!("the `login` journey must be single-actor");
            }
            Ok(resolve(map, &login, Some(&a.account))?.actions)
        }
    }
}

/// A compiled multi-actor plan: the actor list plus the ordered actions, each
/// tagged with the actor that performs it. The runner drives them in order.
pub(super) type Scenario = (Vec<String>, Vec<(String, String)>);

/// Compile a multi-actor journey to (actor list, tagged actions). Each tagged
/// action is `(actor, action)`; the runner drives them in this exact order:
/// every actor's auth prelude first (in actor order), then the interleaved
/// steps. The conductor enforces the global ordering at run time.
pub(super) fn build_scenario(root: &Path, map: Option<&AppMap>, j: &Journey) -> Result<Scenario> {
    if j.setup.is_some() {
        bail!(
            "multi-actor journeys bind auth per actor (e.g. `actors: {{alice: {{login: \
             alice}}}}`), not via top-level `setup`"
        );
    }
    // Declared actors, in order, plus a name->account map for `secret:` binding.
    let mut names: Vec<String> = j.actors.entries().iter().map(|(n, _)| n.clone()).collect();
    let accounts: BTreeMap<String, String> = j
        .actors
        .entries()
        .iter()
        .filter_map(|(n, a)| a.as_ref().map(|x| (n.clone(), x.account.clone())))
        .collect();

    let mut tagged: Vec<(String, String)> = Vec::new();

    // 1. Per-actor auth preludes, tagged to their actor.
    for (name, auth) in j.actors.entries() {
        if let Some(a) = auth {
            for action in actor_prelude(root, map, a)? {
                tagged.push((name.clone(), action));
            }
        }
    }

    // 2. The interleaved steps. A step may name an actor not in the `actors`
    // list (a participant with no auth prelude); register it on first use.
    for (i, step) in j.steps.iter().enumerate() {
        let n = i + 1;
        let actor = step
            .actor
            .clone()
            .ok_or_else(|| anyhow::anyhow!("step {n}: every step needs an `actor:`"))?;
        if !names.contains(&actor) {
            names.push(actor.clone());
        }
        let account = accounts.get(&actor).map(String::as_str);
        for action in resolve_scenario_step(step, n, account)? {
            tagged.push((actor.clone(), action));
        }
    }
    if names.is_empty() {
        bail!("multi-actor journey declares no actors");
    }
    if tagged.is_empty() {
        bail!("multi-actor journey has no steps");
    }
    Ok((names, tagged))
}

/// Warn when a scenario uses an account whose data no reset step clears: the
/// classic footgun where a passing run leaves state that breaks the next one.
/// Coverage means the reset references the account's `userId` literally or by
/// `${account.<name>...}` template.
pub(super) fn warn_uncovered_reset_accounts(cfg: &config::Config, j: &Journey) {
    use crate::adapters::config::ResetStep;
    let used: BTreeSet<String> = j
        .actors
        .entries()
        .iter()
        .filter_map(|(_, a)| a.as_ref().map(|x| x.account.clone()))
        .collect();
    for name in &used {
        let Some(acct) = cfg.auth.accounts.iter().find(|a| &a.name == name) else {
            continue;
        };
        let Some(uid) = acct.user_id.as_deref() else {
            continue;
        };
        let mentions = |s: &str| s.contains(uid) || s.contains(&format!("account.{name}"));
        let covered = cfg.reset.steps.iter().any(|step| match step {
            ResetStep::Command { run, .. } => mentions(run),
            ResetStep::Http { url, body, .. } => {
                mentions(url) || body.as_deref().is_some_and(mentions)
            }
        });
        if !covered {
            eprintln!(
                "  warn: scenario uses account `{name}` (userId {uid}) but no reset step clears \
                 it; stale data may leak between runs"
            );
        }
    }
}

/// Run a multi-actor journey universally: launch the host conductor (the strict
/// step-order barrier) and N device runners (one per actor, any backend), each
/// pulling its own actions from the conductor. Classification is the same
/// log-based contract as single-actor, aggregated across every device log.
pub(super) async fn run_scenario(
    loaded: &config::Loaded,
    _name: &str,
    j: &Journey,
    times: u32,
    quiet: bool,
) -> Result<repro::CheckResult> {
    // Fail fast if this backend has no multi-actor client: otherwise we'd boot N
    // devices that never pull from the conductor and just sit there until the
    // journey times out.
    let platform = &loaded.config.app.platform;
    if !crate::adapters::platform::speaks_barrier(platform) {
        bail!(
            "platform `{platform}` has no multi-actor runner yet; authored multi-user scenarios \
             currently run on: {}",
            crate::adapters::platform::barrier_ids()
        );
    }
    let map = load_map(&loaded.root)?;
    let (actors, tagged) = build_scenario(&loaded.root, map.as_ref(), j)?;
    warn_uncovered_reset_accounts(&loaded.config, j);
    let n = actors.len();
    // Map each actor to its launch-order device index (0=`a`, 1=`b`, ...).
    let idx: BTreeMap<&str, usize> = actors
        .iter()
        .enumerate()
        .map(|(i, a)| (a.as_str(), i))
        .collect();
    // Resolve ${REPROIT_SECRET_*} placeholders host-side before the conductor
    // serves them, so each actor's runner types real values with no secret code.
    let secrets = crate::adapters::credentials::secret_env(&loaded.config.auth, &loaded.root)
        .unwrap_or_default();
    let script: Vec<(usize, String)> = tagged
        .iter()
        .map(|(actor, action)| {
            (
                idx[actor.as_str()],
                crate::adapters::credentials::resolve_placeholders(action, &secrets),
            )
        })
        .collect();

    let total = times.max(1);
    let mut verdicts = Vec::new();
    for i in 1..=total {
        // A fresh conductor per run: it owns ordering for this run only.
        let conductor = crate::workflows::barrier::Conductor::start(script.clone(), n).await?;
        let defines = vec![("REPROIT_SCENARIO_BARRIER".to_string(), conductor.url())];
        let outcome = orchestrator::run_journey_tier(
            &loaded.config,
            &loaded.root,
            "explore",
            &orchestrator::RunOpts {
                devices: n,
                // Never warm across runs: the conductor URL is fresh each run and
                // reaches a Flutter runner as a compile-time define, so a warm
                // (--no-build) run would carry the previous run's stale port. Web
                // reads it from env so it wouldn't care, but a uniform cold run
                // keeps the gate (times>1) correct on every backend.
                warm: false,
                extra_defines: &defines,
                ..Default::default()
            },
            // Multi-actor is always the sim tier: it launches N real sims (one
            // per actor) and the barrier client drives each. The in-process
            // headless tier is a single runner and can't be N devices.
            true,
        )
        .await?;
        let completed = conductor.is_done();
        let stall = conductor.diagnose();
        drop(conductor); // stop the barrier server
        let log = read_device_logs(&outcome.run_dir, n);
        let mut verdict = classify_run(&log, outcome.passed);
        let violations = journey_contract_violations(j, &log, &actors);
        if !violations.is_empty() {
            verdict = repro::RunVerdict::Broke;
        }
        // If the script never ran to completion (a device died, or got stuck
        // before its turn), that's not a clean pass: the journey couldn't replay.
        if verdict == repro::RunVerdict::Green && !completed {
            verdict = repro::RunVerdict::CouldNotReplay;
        }
        // Turn an anonymous timeout into a named diagnosis: who never joined, or
        // which actor's action never completed.
        if !completed && !quiet {
            let who = |i: usize| actors.get(i).map(String::as_str).unwrap_or("?");
            match stall {
                crate::workflows::barrier::Stage::AwaitingJoin(missing) => {
                    let names: Vec<String> = missing
                        .iter()
                        .map(|&i| format!("{} ({})", crate::workflows::barrier::letter(i), who(i)))
                        .collect();
                    println!(
                        "  scenario stalled: actor(s) never joined: {}",
                        names.join(", ")
                    );
                }
                crate::workflows::barrier::Stage::Stalled { dev, action, secs } => {
                    println!(
                        "  scenario stalled: actor {} ({}) never completed `{action}` after \
                         {secs}s",
                        crate::workflows::barrier::letter(dev),
                        who(dev),
                    );
                }
                _ => {}
            }
        }
        if !quiet {
            println!("  run {i}/{total}: {}", verdict.as_str());
            for violation in &violations {
                println!(
                    "    contract {} failed at observation {} ({})",
                    violation.contract_id, violation.boundary_index, violation.fingerprint
                );
            }
        }
        verdicts.push(verdict);
    }
    Ok(repro::CheckResult::from_verdicts(&verdicts))
}

/// Concatenate every device's drive log (`drive-a.log`, `drive-b.log`, ...) so
/// classification sees a crash or missed step on ANY actor.
pub(super) fn read_device_logs(run_dir: &Path, n: usize) -> String {
    (0..n)
        .map(|i| (b'a' + i as u8) as char)
        .filter_map(|label| {
            std::fs::read_to_string(run_dir.join(format!("drive-{label}.log"))).ok()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) async fn run_tagged_scenario_once(
    loaded: &config::Loaded,
    actors: &[String],
    checkpoint: &[(String, String)],
    suffix: &[(String, String)],
    capsule: Option<&Path>,
) -> Result<(String, bool, std::path::PathBuf)> {
    let idx: BTreeMap<&str, usize> = actors
        .iter()
        .enumerate()
        .map(|(i, actor)| (actor.as_str(), i))
        .collect();
    let secrets = crate::adapters::credentials::secret_env(&loaded.config.auth, &loaded.root)
        .unwrap_or_default();
    let script: Vec<(usize, String)> = checkpoint
        .iter()
        .chain(suffix.iter())
        .map(|(actor, action)| {
            Ok((
                *idx.get(actor.as_str())
                    .ok_or_else(|| anyhow::anyhow!("unknown actor `{actor}`"))?,
                crate::adapters::credentials::resolve_placeholders(action, &secrets),
            ))
        })
        .collect::<Result<_>>()?;
    let conductor = crate::workflows::barrier::Conductor::start(script, actors.len()).await?;
    let mut defines = vec![("REPROIT_SCENARIO_BARRIER".to_string(), conductor.url())];
    if let Some(path) = capsule {
        defines.push(("REPROIT_CAPSULE".to_string(), path.display().to_string()));
    }
    let outcome = orchestrator::run_journey_tier(
        &loaded.config,
        &loaded.root,
        "explore",
        &orchestrator::RunOpts {
            devices: actors.len(),
            warm: false,
            extra_defines: &defines,
            ..Default::default()
        },
        true,
    )
    .await?;
    let completed = conductor.is_done();
    drop(conductor);
    Ok((
        read_device_logs(&outcome.run_dir, actors.len()),
        completed,
        outcome.run_dir,
    ))
}

pub(super) fn build_multi_capsule(
    loaded: &config::Loaded,
    seed: u64,
    actors: &[String],
    checkpoint: &[(String, String)],
    suffix: &[(String, String)],
    signature: &str,
    run_dir: &Path,
) -> Result<crate::domain::capsule::Capsule> {
    let finding = crate::domain::capsule::FindingIdentity {
        oracle: "multi_actor".into(),
        invariant: signature.into(),
        kind: "multi_actor".into(),
        message: signature.into(),
        frame: String::new(),
        trigger: signature.into(),
        boundary: None,
    };
    let app = if !loaded.config.app.bundle_id.is_empty() {
        loaded.config.app.bundle_id.clone()
    } else {
        loaded
            .config
            .app
            .url
            .clone()
            .or_else(|| loaded.config.app.executable.clone())
            .unwrap_or_else(|| loaded.config.app.platform.clone())
    };
    let mut capsule = crate::domain::capsule::Capsule::new(app, finding);
    capsule.capabilities.insert(
        "ui_actions".into(),
        crate::domain::capsule::Capability {
            status: crate::domain::capsule::CaptureStatus::Captured,
            detail: Some("ordered conductor schedule".into()),
        },
    );
    capsule.environment.insert("seed".into(), seed.to_string());
    let mut actor_indices = BTreeMap::<String, u32>::new();
    let actor_labels: BTreeMap<&str, String> = actors
        .iter()
        .enumerate()
        .map(|(index, actor)| (actor.as_str(), ((b'a' + index as u8) as char).to_string()))
        .collect();
    capsule.actions = checkpoint
        .iter()
        .chain(suffix.iter())
        .map(|(actor, action)| {
            let label = actor_labels
                .get(actor.as_str())
                .cloned()
                .unwrap_or_else(|| actor.clone());
            let index = actor_indices.entry(label.clone()).or_insert(0);
            *index += 1;
            crate::domain::capsule::Action {
                index: *index,
                actor: label,
                action: action.clone(),
                from_sig: None,
                to_sig: None,
            }
        })
        .collect();
    capsule.ingest_network_files(run_dir)?;
    crate::domain::capsule::redact_capsule(
        &mut capsule,
        &crate::domain::capsule::RedactionPolicy::default(),
    );
    capsule.finalize_id()?;
    if !capsule.confirmable() {
        bail!(
            "multi-actor finding cannot become a causal capsule; missing: {}",
            capsule.missing_required_capabilities().join(", ")
        );
    }
    let missing_replay = capsule.missing_required_replay_capabilities();
    if !missing_replay.is_empty() {
        bail!(
            "multi-actor finding cannot be replayed hermetically; missing: {}",
            missing_replay.join(", ")
        );
    }
    capsule.persist(&loaded.root)?;
    Ok(capsule)
}

pub(super) async fn multi_capsule_reproduces(
    loaded: &config::Loaded,
    actors: &[String],
    checkpoint: &[(String, String)],
    suffix: &[(String, String)],
    signature: &str,
    candidate: &mut crate::domain::capsule::Capsule,
) -> Result<bool> {
    candidate.finalize_id()?;
    candidate.persist(&loaded.root)?;
    let id = candidate.id.clone();
    let guard = crate::domain::capsule::Capsule::materialize_plaintext(&loaded.root, &id)?;
    let result =
        run_tagged_scenario_once(loaded, actors, checkpoint, suffix, Some(guard.path())).await;
    let _ = std::fs::remove_dir_all(crate::runtime::project_layout::capsule_dir(
        &loaded.root,
        &id,
    ));
    let (log, done, _) = result?;
    Ok(done
        && crate::workflows::fuzz::finding_signatures_for_log(&loaded.config, &log)
            .contains(signature))
}

pub(super) async fn shrink_multi_capsule(
    loaded: &config::Loaded,
    actors: &[String],
    checkpoint: &[(String, String)],
    suffix: &[(String, String)],
    signature: &str,
    mut best: crate::domain::capsule::Capsule,
) -> Result<crate::domain::capsule::Capsule> {
    let original_id = best.id.clone();
    let mut index = 0;
    while index < best.exchanges.len() {
        let mut candidate = best.clone();
        candidate.exchanges.remove(index);
        if multi_capsule_reproduces(
            loaded,
            actors,
            checkpoint,
            suffix,
            signature,
            &mut candidate,
        )
        .await?
        {
            best = candidate;
        } else {
            index += 1;
        }
    }
    for exchange_index in 0..best.exchanges.len() {
        for response in [false, true] {
            let current = if response {
                best.exchanges[exchange_index].response_body.clone()
            } else {
                best.exchanges[exchange_index].request_body.clone()
            };
            let Some(current) = current else { continue };
            for reduced in crate::domain::capsule::json_reductions(&current) {
                let mut candidate = best.clone();
                if response {
                    candidate.exchanges[exchange_index].response_body = Some(reduced);
                } else {
                    candidate.exchanges[exchange_index].request_body = Some(reduced);
                }
                if multi_capsule_reproduces(
                    loaded,
                    actors,
                    checkpoint,
                    suffix,
                    signature,
                    &mut candidate,
                )
                .await?
                {
                    best = candidate;
                    break;
                }
            }
        }
    }
    if !multi_capsule_reproduces(loaded, actors, checkpoint, suffix, signature, &mut best).await? {
        bail!("jointly minimized multi-actor causal capsule failed final confirmation");
    }
    best.persist(&loaded.root)?;
    if original_id != best.id {
        let _ = std::fs::remove_dir_all(crate::runtime::project_layout::capsule_dir(
            &loaded.root,
            &original_id,
        ));
    }
    Ok(best)
}

#[allow(clippy::too_many_arguments)] // Persists the complete multi-actor finding contract.
pub(super) fn persist_multi_finding(
    loaded: &config::Loaded,
    checkpoint_name: &str,
    seed: u64,
    actors: &[String],
    checkpoint: &[(String, String)],
    suffix: &[(String, String)],
    signature: &str,
    capsule_id: &str,
    independence: &[IndependentActionPair],
) -> Result<String> {
    let mut hash = 0x811c9dc5u32;
    for b in signature.bytes() {
        hash ^= b as u32;
        hash = hash.wrapping_mul(0x01000193);
    }
    let id = format!("multi-{hash:08x}");
    let steps: Vec<serde_yaml::Value> = checkpoint
        .iter()
        .chain(suffix.iter())
        .map(|(actor, action)| {
            serde_yaml::to_value(BTreeMap::from([
                ("actor".to_string(), actor.clone()),
                ("do".to_string(), action.clone()),
            ]))
            .unwrap()
        })
        .collect();
    let doc = serde_yaml::to_string(&serde_json::json!({
        "journey": id,
        "actors": actors,
        "steps": steps,
    }))?;
    std::fs::create_dir_all(journeys_dir(&loaded.root))?;
    std::fs::write(journey_path(&loaded.root, &id), doc)?;
    let dir = layout::finding_dir(&loaded.root, &id);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(
        dir.join("finding.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "id": id,
            "oracleSignature": signature,
            "checkpoint": checkpoint_name,
            "seed": seed,
            "generatedSteps": suffix.len(),
            "capsuleId": capsule_id,
            "independentActions": independence,
            "run": format!("reproit @{id}"),
        }))?,
    )?;
    std::fs::write(dir.join("capsule-id"), capsule_id)?;
    Ok(id)
}
