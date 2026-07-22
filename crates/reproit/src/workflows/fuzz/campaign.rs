use super::*;

pub(super) async fn fuzz_one_locale(
    cfg: &Config,
    root: &Path,
    args: &FuzzArgs,
    locale: Option<&str>,
) -> Result<FuzzSummary> {
    let mut found_sigs: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    // State-present issues (overflow/content/choice/broken-route) seen on the
    // way, deduped by signature -> oracle, for the footer that points at `scan`.
    let mut state_present: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    // --all: crash-signature -> (human label, [(repro id, action count, seed)]).
    // Same signature = same bug; the buckets become the unique-bugs summary.
    let mut buckets: BugBuckets = BugBuckets::new();
    // Equivalent findings reached by another seed reuse the representative's
    // minimized actions. This avoids paying ddmin's replay cost once per seed.
    let mut shrink_cache: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    let mut shrink_representatives = std::collections::BTreeSet::new();
    let cfg_path = crate::runtime::project_layout::fuzz_config_path(root);
    std::fs::create_dir_all(cfg_path.parent().unwrap())?;
    let mut defines = vec![(
        "REPROIT_FUZZ_CONFIG".to_string(),
        cfg_path.to_string_lossy().into_owned(),
    )];
    // LOCALE contract: REPROIT_LOCALE travels as a dart-define (Flutter) / env
    // var (other backends) via the orchestrator's define list. The explorers
    // honor it (owned by a separate agent); here we only emit + tag.
    if let Some(loc) = locale {
        defines.push((
            crate::domain::locale::LOCALE_ENV.to_string(),
            loc.to_string(),
        ));
    }

    // Batch size: 0 means "all runs in one drive session" (the default, the
    // big win). 1 means one drive per seed. Clamp to runs.
    let batch_size = if args.batch == 0 {
        args.runs.max(1)
    } else {
        args.batch.clamp(1, args.runs.max(1))
    };
    let json = args.json;
    if batch_size > 1 {
        say(
            json,
            format!(
                "fuzz: {} seed(s) in batches of {} (startup amortized per batch)",
                args.runs, batch_size
            ),
        );
    }

    // PURE: fuzz reads the committed map/visits ONCE, then accrues coverage
    // guidance IN MEMORY across batches/seeds (it never writes the committed
    // graph, so a fixed seed replays identically across invocations; `map` is
    // what folds discoveries in). Each seed in a batch shares the snapshot as it
    // stands at the START of that batch; the in-memory snapshot updates BETWEEN
    // batches (via absorb_run_inmem below), not within. Smaller batches tighten
    // the guidance loop at the cost of more startups.
    let (mut map, mut visits) = crate::domain::map::load_snapshot(root, cfg)?;
    // Routes the aggregate map can leave: folded into each per-seed permission
    // trap check so a sparse seed does not false-flag an escapable page.
    // Grows as seeds reveal exits the shallow map-build never reached.
    let mut escapable = map_escapable_routes(&map);
    let mut warm = false;
    let mut done = 0u32;
    let static_guidance = static_guidance(cfg, args);
    // Seeds that ACTUALLY executed (one log segment each), vs `done` which counts
    // seeds DISPATCHED into a batch. A wall-clock timeout can kill a multi-seed
    // batch after only the first seed, so the summary must report seeds_run, not
    // the configured count, or it overstates how much was explored.
    let mut seeds_run = 0u32;
    let mut complete = true;
    let mut evidence = crate::domain::evidence::EvidenceCounts::default();
    let mut confirmed_findings = Vec::new();
    while done < args.runs {
        let this_batch = batch_size.min(args.runs - done);
        let guidance = batch_guidance(args, &map, &visits, &static_guidance);
        let plans: Vec<SeedPlan> = (0..this_batch)
            .map(|j| {
                let seed = args.seed + (done + j) as u64;
                plan_seed(args, &guidance, seed, done + j)
            })
            .collect();

        // Write the config the explorer reads. A single-seed batch uses the
        // compact {"seed":..} shape; multi-seed batches use {"batch":[...]} and
        // the explorer resets the widget tree between seeds.
        let config = if plans.len() == 1 {
            plans[0].config.clone()
        } else {
            json!({ "batch": plans.iter().map(|p| p.config.clone()).collect::<Vec<_>>() })
        };
        std::fs::write(&cfg_path, config.to_string())?;

        let outcome = run_explorer(
            cfg,
            root,
            &args.journey,
            warm,
            &defines,
            args.profile_timing,
            args.sim,
            false,
        )
        .await?;
        warm = true;
        done += this_batch;

        // Split the single drive log per seed by the SEED:BEGIN/END markers,
        // so coverage, trace, and findings are attributed to the right seed.
        let full_log =
            std::fs::read_to_string(outcome.run_dir.join("drive-a.log")).unwrap_or_default();
        evidence.merge(&crate::domain::evidence::EvidenceCounts::from_log(
            &full_log,
        ));
        let segments = split_seed_segments(&full_log, &plans);
        seeds_run += segments.len() as u32;
        complete &= batch_completed(&full_log, &plans);
        let parsed_segments: Vec<_> = segments
            .into_iter()
            .map(|(seed, log)| {
                let parsed = crate::domain::runner::ParsedRun::new(
                    log,
                    &[],
                    !cfg.contracts.is_empty(),
                    cfg.backend.enabled,
                );
                (seed, parsed)
            })
            .collect();

        // Pool escapable routes across ALL seeds in this batch BEFORE judging any
        // of them. A permission trap is a graph property, so one seed's sparse view is
        // too partial: an early seed that only reached a page as its budget
        // terminus would false-flag it even though a sibling seed left it cleanly.
        // Pooling (and accumulating into `escapable` across batches) means a page
        // any seed could leave via a forward action is never a trap.
        for (_, parsed) in &parsed_segments {
            let o = &parsed.map;
            for (from, action, to) in &o.edges {
                if action != "back" && to != from {
                    if let Some(r) = o.routes.get(from) {
                        let labels: std::collections::BTreeSet<String> = o
                            .states
                            .get(from)
                            .map(|labels| labels.iter().cloned().collect())
                            .unwrap_or_default();
                        std::sync::Arc::make_mut(&mut escapable)
                            .entry(r.clone())
                            .or_default()
                            .insert(labels);
                    }
                }
            }
        }

        for (idx, (seed, parsed)) in parsed_segments.into_iter().enumerate() {
            let trace = parsed.trace.clone();
            // Accrue this walk's coverage into the IN-MEMORY snapshot only, so
            // later batches in THIS run get the guidance, but the committed
            // map/visits stay untouched (fuzz is pure; re-run `map` to fold in).
            crate::domain::map::absorb_obs_inmem(&mut map, &mut visits, &parsed.map);
            // Findings attributed to THIS seed: exceptions parsed from the
            // seed's log slice, plus the per-device perf oracle (whole-session;
            // attributed to whichever seed it lands in only when we can't split
            // perf per seed, frame timing is session-wide, so it is attributed
            // to the run as a whole on the first seed that has the manifest).
            // The INVARIANTS oracle: evaluate the built-in + custom invariant
            // set over THIS seed's parsed state graph + exceptions (shared with
            // findings_for_tier/scan via findings_from_log). no-exception
            // subsumes the old raw-exception oracle, so the exceptions are fed in
            // and folded back when that invariant is disabled. The pooled
            // `escapable` routes keep a permission trap only when no batch's
            // evidence escapes it. Jank/leak stay handled by perf_findings below
            // for the sim tier (session-wide frame stream).
            let normalized_evidence = NormalizedEvidence {
                observations: &parsed.observations,
                backend_events: &parsed.backend,
                stream_defects: &parsed.defects,
            };
            let mut findings = findings_from_parsed(
                cfg,
                parsed.map,
                parsed.exceptions,
                args.sim,
                escapable.clone(),
                normalized_evidence,
            );
            let contract_evaluations = crate::domain::contracts::evaluate_stream(
                &cfg.contracts,
                &parsed.observations,
                &parsed.defects,
            );
            let _ = crate::domain::contracts::write_evidence(
                &outcome
                    .run_dir
                    .join(format!("contract-evidence-{seed}.json")),
                &cfg.contracts,
                &parsed.observations,
                &contract_evaluations,
                &parsed.defects,
            );
            // Perf is session-wide (one frame stream); attribute it once. The
            // sim manifest's per-device jank is the authoritative no-jank signal;
            // headless has a fake clock so this is empty there (sim-only).
            if idx == 0 {
                findings.extend(perf_findings(&outcome.run_dir));
            }
            // ORACLE filter: tag every kept finding with its `oracle` category
            // and drop the categories `--only`/`--no` excluded. Done before the
            // empty check so an all-filtered seed is correctly reported clean.
            let dropped;
            (findings, dropped) = args.oracle_filter.apply(findings);
            if !dropped.is_empty() {
                say(
                    json,
                    format!(
                        "  seed {seed}: {} finding(s) filtered out by --only/--no",
                        dropped.len()
                    ),
                );
            }
            // ADVISORY split: non-deterministic pixel/timing signals (e.g.
            // paint-flicker) are reported for information but NEVER counted as a
            // verdict-bearing repro, per reproit's "reproduces on any machine"
            // promise. Pull them out before the verdict is formed so they never
            // create a FINDING or a saved repro.
            let (verdict_findings, advisory): (Vec<_>, Vec<_>) =
                findings.into_iter().partition(|finding| {
                    !finding
                        .get("advisory")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                });
            findings = verdict_findings;
            let unreported_violations = findings
                .iter()
                .filter(|finding| {
                    let oracle = crate::domain::oracle::classify(finding).as_str();
                    !crate::domain::evidence::has_explicit_status_marker(oracle)
                })
                .count();
            evidence.observe_unreported_violations(unreported_violations);
            for f in &advisory {
                say(
                    json,
                    format!(
                        "  seed {seed}: advisory (not a repro): {}",
                        f.get("message").and_then(Value::as_str).unwrap_or("")
                    ),
                );
            }
            // LOCALE tag: stamp every kept finding with the locale it was found
            // under, and record its signature for the cross-locale i18n diff.
            if let Some(loc) = locale {
                for f in findings.iter_mut() {
                    crate::domain::locale::tag_finding_locale(f, loc);
                }
            }
            for f in &findings {
                let signature = finding_signature(f);
                found_sigs.insert(signature.clone());
                // Tally the STATE-PRESENT issues this walk passed (content /
                // choice / broken-route), deduped by signature,
                // so the report can point them at `scan` instead of burying them
                // under the per-seed crash headline.
                let oracle = crate::domain::oracle::classify(f).as_str();
                if matches!(
                    oracle,
                    "content-bug"
                        | "detached-indicator"
                        | "choice-anomaly"
                        | "broken-route"
                        | "security"
                ) {
                    state_present.insert(signature, oracle.to_string());
                }
            }
            if findings.is_empty() {
                say(json, format!("  seed {seed}: clean"));
                continue;
            }
            // Summarize which named invariants fired (count per invariant id).
            let mut by_inv: std::collections::BTreeMap<&str, usize> =
                std::collections::BTreeMap::new();
            for f in &findings {
                *by_inv
                    .entry(
                        f.get("invariant")
                            .and_then(Value::as_str)
                            .unwrap_or("exception"),
                    )
                    .or_default() += 1;
            }
            let summary = by_inv
                .iter()
                .map(|(k, n)| format!("{k} x{n}"))
                .collect::<Vec<_>>()
                .join(", ");
            say(
                json,
                format!(
                    "  seed {seed}: observation ({} violation(s): {summary})",
                    findings.len()
                ),
            );
            let mut shrunk = trace.clone();
            let want = shrink_target(&findings);
            let mut confirmation = reproit_protocol::ConfirmationStatus::NotAttempted;
            // Confirmation is the product trust gate, not an optional polish
            // pass: replay in a clean session and require the same oracle before
            // a candidate can be promoted. The shrinker starts with a zero-action
            // replay, so load-state failures are confirmed too.
            if args.shrink {
                if !confirm_trace(
                    cfg,
                    root,
                    &args.journey,
                    &cfg_path,
                    &defines,
                    &trace,
                    args.sim,
                    &want,
                )
                .await?
                {
                    confirmation = reproit_protocol::ConfirmationStatus::NotReproduced;
                    say(
                        json,
                        format!(
                            "  seed {seed}: candidate did NOT reproduce in a clean session; \
                             retained with replay blocker"
                        ),
                    );
                } else {
                    confirmation = reproit_protocol::ConfirmationStatus::Reproduced;
                    say(json, format!("  seed {seed}: CONFIRMED in a clean replay"));
                    let equivalent = equivalent_findings_key(&findings);
                    if args.all {
                        if !reserve_shrink_representative(&mut shrink_representatives, &findings) {
                            let representative = shrink_cache
                                .get(&equivalent)
                                .expect("reserved shrink representative must be cached");
                            shrunk = representative.clone();
                            say(
                                json,
                                "  shrink: equivalent finding already minimized; reusing \
                                 representative",
                            );
                        } else {
                            shrunk = shrink(
                                cfg,
                                root,
                                &args.journey,
                                &cfg_path,
                                &defines,
                                trace.clone(),
                                args.sim,
                                &want,
                                json,
                            )
                            .await?;
                            shrink_cache.insert(equivalent, shrunk.clone());
                        }
                    } else {
                        shrunk = shrink(
                            cfg,
                            root,
                            &args.journey,
                            &cfg_path,
                            &defines,
                            trace.clone(),
                            args.sim,
                            &want,
                            json,
                        )
                        .await?;
                    }
                }
            }
            // The finding's content-hash id (over seed + the minimized actions,
            // exactly what `keep` later hashes), plus the two commands it teaches:
            // `reproit <id>` confirms it replays NOW (before you commit it to the
            // suite), `keep <id>` saves it as a guard.
            let primary_sig = primary_finding(&findings)
                .map(finding_signature)
                .unwrap_or_else(|| "unknown".to_string());
            let mut repro_id = crate::domain::repro::finding_id(
                &target_identity(cfg),
                &primary_sig,
                seed,
                &shrunk,
            );
            let mut finding_id = crate::domain::repro::display_finding_id(&repro_id);
            // `--all` batches every seed into ONE drive run_dir, so writing each
            // finding's report to that shared dir would overwrite the previous
            // fuzz.md and only the last finding would be resolvable by
            // check/keep. Give each finding its OWN report dir, keyed by id and an
            // immediate child of the evidence out dir, so find_finding_by_id can
            // resolve EVERY unique bug the run reports, not just the last.
            let mut report_dir = if args.all {
                let d = root
                    .join(&cfg.evidence.out_dir)
                    .join(format!("finding-{repro_id}"));
                std::fs::create_dir_all(&d)?;
                d
            } else {
                outcome.run_dir.clone()
            };
            write_report(
                &report_dir,
                &repro_id,
                seed,
                &findings,
                &trace,
                &shrunk,
                confirmation,
            )?;
            let proof = write_run_evidence_graph(
                &report_dir,
                RunEvidence {
                    capture_dir: &outcome.run_dir,
                    finding_id: &repro_id,
                    trace: &trace,
                    findings: &findings,
                    minimized: &shrunk,
                    confirmation,
                    capsule: None,
                },
            )?;
            let promoted = proof.promotion == reproit_protocol::PromotionStatus::Confirmed;
            persist_finding_report(root, &repro_id, &report_dir)?;
            if let Some(guard) = crate::domain::contracts::FrozenContractGuard::from_findings(
                &cfg.contracts,
                &findings,
            ) {
                guard.save(&layout::finding_dir(root, &repro_id).join("contract.json"))?;
            }
            if let Some(guard) =
                crate::domain::backend::FrozenBackendGuard::from_findings(&cfg.backend, &findings)
            {
                guard.save(&layout::finding_dir(root, &repro_id).join("backend-contract.json"))?;
            }
            if let Some(primary) = primary_finding(&findings).filter(|_| promoted) {
                let Some(capsule_capture) = capture_confirmed_trace(
                    cfg,
                    root,
                    &args.journey,
                    &cfg_path,
                    &defines,
                    &shrunk,
                    args.sim,
                    &want,
                )
                .await?
                else {
                    let _ = std::fs::remove_dir_all(layout::finding_dir(root, &repro_id));
                    say(
                        json,
                        format!(
                            "  seed {seed}: minimized trace lost its exact identity during final \
                             causal capture; quarantined"
                        ),
                    );
                    continue;
                };
                let capsule = persist_causal_capsule(
                    cfg,
                    root,
                    &capsule_capture.run_dir,
                    primary,
                    &shrunk,
                    &defines,
                    seed,
                )?;
                let capsule = shrink_causal_capsule(
                    cfg,
                    root,
                    &args.journey,
                    &cfg_path,
                    &defines,
                    args.sim,
                    &want,
                    capsule,
                    json,
                )
                .await?;
                let mut provisional_id = None;
                let causal_actions = capsule.replay_actions();
                if causal_actions != shrunk {
                    let previous_repro_id = repro_id.clone();
                    provisional_id = Some(previous_repro_id.clone());
                    let previous_report_dir = report_dir.clone();
                    shrunk = causal_actions;
                    repro_id = crate::domain::repro::finding_id(
                        &target_identity(cfg),
                        &primary_sig,
                        seed,
                        &shrunk,
                    );
                    finding_id = crate::domain::repro::display_finding_id(&repro_id);
                    if args.all {
                        report_dir = root
                            .join(&cfg.evidence.out_dir)
                            .join(format!("finding-{repro_id}"));
                        std::fs::create_dir_all(&report_dir)?;
                    }
                    write_report(
                        &report_dir,
                        &repro_id,
                        seed,
                        &findings,
                        &trace,
                        &shrunk,
                        confirmation,
                    )?;
                    if let Some(guard) =
                        crate::domain::contracts::FrozenContractGuard::from_findings(
                            &cfg.contracts,
                            &findings,
                        )
                    {
                        guard.save(&layout::finding_dir(root, &repro_id).join("contract.json"))?;
                    }
                    if let Some(guard) = crate::domain::backend::FrozenBackendGuard::from_findings(
                        &cfg.backend,
                        &findings,
                    ) {
                        guard.save(
                            &layout::finding_dir(root, &repro_id).join("backend-contract.json"),
                        )?;
                    }
                    if previous_repro_id != repro_id
                        && args.all
                        && previous_report_dir != report_dir
                    {
                        let _ = std::fs::remove_dir_all(previous_report_dir);
                    }
                }
                let capsule_id = capsule.id.clone();
                let guard =
                    crate::domain::capsule::Capsule::materialize_plaintext(root, &capsule_id)?;
                let mut capsule_defines = defines.clone();
                capsule_defines.push((
                    "REPROIT_CAPSULE".into(),
                    guard.path().to_string_lossy().into_owned(),
                ));
                if !confirm_trace(
                    cfg,
                    root,
                    &args.journey,
                    &cfg_path,
                    &capsule_defines,
                    &shrunk,
                    args.sim,
                    &want,
                )
                .await?
                {
                    let _ = std::fs::remove_dir_all(crate::runtime::project_layout::capsule_dir(
                        root,
                        &capsule_id,
                    ));
                    let _ = std::fs::remove_dir_all(layout::finding_dir(root, &repro_id));
                    say(
                        json,
                        format!(
                            "  seed {seed}: live failure confirmed, but causal capsule did not \
                             reproduce exactly; quarantined"
                        ),
                    );
                    continue;
                }
                let _ = write_run_evidence_graph(
                    &report_dir,
                    RunEvidence {
                        capture_dir: &capsule_capture.run_dir,
                        finding_id: &repro_id,
                        trace: &trace,
                        findings: &findings,
                        minimized: &shrunk,
                        confirmation,
                        capsule: Some(&capsule),
                    },
                )?;
                persist_finding_report(root, &repro_id, &report_dir)?;
                let finding_dir = layout::finding_dir(root, &repro_id);
                std::fs::create_dir_all(&finding_dir)?;
                std::fs::write(finding_dir.join("capsule-id"), &capsule_id)?;
                let bug_id = capsule.finding.bug_id();
                std::fs::write(
                    finding_dir.join("identity.json"),
                    serde_json::to_vec_pretty(&json!({
                        "bugId": &bug_id,
                        "identity": capsule.finding,
                    }))?,
                )?;
                promote_finding(root, provisional_id.as_deref(), &repro_id, &report_dir)?;
                confirmed_findings.push(super::ConfirmedFinding {
                    id: finding_id.clone(),
                    cause: capsule.cause_category(),
                    action_count: capsule.actions.len(),
                    artifact: layout::finding_dir(root, &repro_id),
                });
                say(json, format!("  capsule: {capsule_id}"));
                say(json, format!("  structural bug: {bug_id}"));
                say(json, "  Finding confirmed: yes");
                say(
                    json,
                    format!("  Cause: {}", capsule.cause_category().as_str()),
                );
                say(
                    json,
                    format!("  Actions required: {}", capsule.actions.len()),
                );
                say(
                    json,
                    format!(
                        "  Causal HTTP request: {}",
                        if matches!(
                            capsule.cause_category(),
                            crate::domain::capsule::CauseCategory::HttpTransaction
                        ) {
                            "captured"
                        } else {
                            "not applicable"
                        }
                    ),
                );
                say(json, "  Finding minimized: yes");
                say(json, "  Finding artifact saved: yes");
                say(json, "  Regression guard kept: no");
                say(
                    json,
                    format!("  Next: reproit keep {finding_id} --as <name>"),
                );
            }
            // In --all the per-seed id is intermediate: the SAME bug reached by
            // different seeds yields different ids, so teaching check/keep here
            // hands the agent several competing ids for one bug. The deduped
            // summary at the end is authoritative and teaches the commands on the
            // one canonical id; here we just note the finding. Without --all this
            // IS the single finding, so teach its commands directly.
            if !promoted {
                say(
                    json,
                    format!(
                        "  candidate {finding_id}   blockers: {}   inspect: reproit proof \
                         {finding_id}",
                        proof
                            .blockers
                            .iter()
                            .map(|blocker| blocker.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                );
            } else if args.all {
                say(
                    json,
                    format!("  found ({} action(s)) -> id {finding_id}", shrunk.len()),
                );
            } else {
                say(
                    json,
                    format!(
                        "  confirmed bug {finding_id}   reproduce: reproit {finding_id}   keep: \
                         reproit keep {finding_id} --as <name>"
                    ),
                );
            }
            say(
                json,
                format!("  report: {}", report_dir.join("fuzz.md").display()),
            );
            // --all: file this finding under its crash signature so the same bug
            // reached by different paths collapses to one bucket.
            if promoted && args.all {
                if let Some(primary) = primary_finding(&findings) {
                    let sig = finding_signature(primary);
                    buckets
                        .entry(sig)
                        .or_insert_with(|| (finding_label(primary), Vec::new()))
                        .1
                        .push((repro_id.clone(), shrunk.len(), seed));
                }
            }

            // Auto-escalate: when a HEADLESS finding lands, optionally replay the
            // MINIMIZED repro ONCE on the simulator to (a) confirm it on the
            // real runtime and (b) be the run where the annotated repro video
            // gets recorded later. Gated behind --confirm-on-sim (default off),
            // so the default fuzz stays pure-headless and fast.
            // The run dir whose video the delivery pipeline records from: the
            // sim-confirm run when we have one, else the discovering run (already
            // a sim run when --sim was used directly).
            let mut deliver_dir = outcome.run_dir.clone();
            let mut confirmed = args.sim && promoted;
            if promoted && args.confirm_on_sim && !args.sim && !shrunk.is_empty() {
                say(
                    json,
                    format!(
                        "  confirm-on-sim: replaying {} minimized action(s) on the simulator",
                        shrunk.len()
                    ),
                );
                std::fs::write(&cfg_path, json!({ "replay": shrunk }).to_string())?;
                match run_explorer(
                    cfg,
                    root,
                    &args.journey,
                    false,
                    &defines,
                    args.profile_timing,
                    true,
                    false,
                )
                .await
                {
                    Ok(o) => {
                        let sim_log = std::fs::read_to_string(o.run_dir.join("drive-a.log"))
                            .unwrap_or_default();
                        confirmed = replay_is_hermetic(&sim_log)
                            && reproduces_original(
                                &findings_for_tier(cfg, &o.run_dir, true),
                                &want,
                            );
                        say(
                            json,
                            format!(
                                "  confirm-on-sim: {} (sim evidence: {})",
                                if confirmed {
                                    "CONFIRMED on real runtime"
                                } else {
                                    "did NOT reproduce on the simulator (headless-only finding)"
                                },
                                o.run_dir.display()
                            ),
                        );
                        // The sim run holds the .mov; copy the finding's report
                        // (with the minimized repro block) into it so the
                        // delivery pipeline reads the repro + summary from there.
                        let sim_confirmation = if confirmed {
                            reproit_protocol::ConfirmationStatus::Reproduced
                        } else {
                            reproit_protocol::ConfirmationStatus::NotReproduced
                        };
                        write_report(
                            &o.run_dir,
                            &repro_id,
                            seed,
                            &findings,
                            &trace,
                            &shrunk,
                            sim_confirmation,
                        )?;
                        let _ = write_run_evidence_graph(
                            &o.run_dir,
                            RunEvidence {
                                capture_dir: &o.run_dir,
                                finding_id: &repro_id,
                                trace: &trace,
                                findings: &findings,
                                minimized: &shrunk,
                                confirmation: sim_confirmation,
                                capsule: None,
                            },
                        )?;
                        deliver_dir = o.run_dir;
                    }
                    Err(e) => say(json, format!("  confirm-on-sim: sim run failed: {e}")),
                }
            }

            // With --cloud set, record and upload the annotated minimized-repro
            // clip, then optionally emit the review comment. Best-effort: a
            // delivery failure never fails fuzz.
            if let (true, Some(cloud), Some(app), Some(bucket)) =
                (promoted, &args.cloud, &args.app, &args.app_bucket)
            {
                if let Err(e) = deliver_finding(
                    cfg,
                    root,
                    &deliver_dir,
                    cloud,
                    app,
                    bucket,
                    args.post_comment,
                    confirmed,
                    json,
                )
                .await
                {
                    say(json, format!("  deliver: {e}"));
                }
            } else if promoted
                && (args.cloud.is_some() || args.app.is_some() || args.app_bucket.is_some())
            {
                say(
                    json,
                    "  deliver: need --cloud, --app, and --bucket to deliver; skipping",
                );
            }
            // Neutralize: a later warm replay must not reuse this fuzz state.
            let _ = std::fs::write(&cfg_path, "{}");
            // Default: one finding per invocation (shrinking is expensive; fix it
            // before hunting more). With --all, keep going to collect every bug.
            if !args.all {
                state_present_footer(json, &state_present);
                return Ok(FuzzSummary {
                    signatures: found_sigs,
                    complete,
                    seeds_run,
                    seeds_requested: args.runs,
                    evidence,
                    confirmed_findings,
                });
            }
        }
    }
    // --all: report the deduped unique bugs (one bucket per crash signature).
    if args.all && !buckets.is_empty() {
        let total: usize = buckets.values().map(|(_, v)| v.len()).sum();
        say(
            json,
            format!(
                "\nunique bugs: {} (from {total} finding(s) over {seeds_run} seed(s))",
                buckets.len(),
            ),
        );
        for (_sig, (label, mut entries)) in buckets {
            // Canonical repro for the bug: the shortest (fewest actions).
            entries.sort_by_key(|(_, n, _)| *n);
            let (id, n, _) = entries[0].clone();
            let finding_id = crate::domain::repro::display_finding_id(&id);
            let dups = entries.len().saturating_sub(1);
            let also = if dups > 0 {
                format!("  (+{dups} more path(s) reach the same bug)")
            } else {
                String::new()
            };
            say(
                json,
                format!("  {finding_id}  {label}  [{n} action(s)]{also}"),
            );
            say(
                json,
                format!(
                    "    reproduce: reproit {finding_id}   keep: reproit keep {finding_id} --as \
                     <name>"
                ),
            );
        }
        state_present_footer(json, &state_present);
        let _ = std::fs::write(&cfg_path, "{}");
        if !complete || seeds_run < args.runs {
            say(
                json,
                format!(
                    "\nincomplete fuzz coverage: ran {seeds_run} of {} requested seed(s)",
                    args.runs
                ),
            );
        }
        return Ok(FuzzSummary {
            signatures: found_sigs,
            complete: complete && seeds_run == args.runs,
            seeds_run,
            seeds_requested: args.runs,
            evidence,
            confirmed_findings,
        });
    }
    say(
        json,
        format!(
            "\nno findings over {seeds_run} seed(s), budget {}",
            args.budget
        ),
    );
    // Neutralize: a later warm replay must not reuse fuzz state.
    let _ = std::fs::write(&cfg_path, "{}");
    if !complete || seeds_run < args.runs {
        say(
            json,
            format!(
                "incomplete fuzz coverage: ran {seeds_run} of {} requested seed(s)",
                args.runs
            ),
        );
    }
    Ok(FuzzSummary {
        signatures: found_sigs,
        complete: complete && seeds_run == args.runs,
        seeds_run,
        seeds_requested: args.runs,
        evidence,
        confirmed_findings,
    })
}
