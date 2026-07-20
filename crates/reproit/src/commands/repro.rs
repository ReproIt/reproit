//! Finding lookup, saved-repro manipulation, and replay verification.

use super::record::web_record_metadata;
use super::*;
use crate::model::repro;
use crate::modes::fuzz;
use std::path::PathBuf;

/// A human label for a repro in CLI output: `<id> (<alias>)` when an alias is
/// set, else just the id.
pub(super) fn repro_label(m: &repro::Meta) -> String {
    let id = repro::display_repro_id(&m.id);
    match &m.alias {
        Some(a) => format!("{id} ({a})"),
        None => id,
    }
}

fn pending_label(id: &str) -> String {
    repro::display_finding_id(id)
}

pub(super) fn check_label(m: &repro::Meta) -> String {
    if m.created.is_empty() {
        pending_label(&m.id)
    } else {
        repro_label(m)
    }
}

pub(super) fn public_json_id(m: &repro::Meta) -> String {
    if m.created.is_empty() {
        repro::display_finding_id(&m.id)
    } else {
        repro::display_repro_id(&m.id)
    }
}

pub(super) fn public_json_kind(m: &repro::Meta) -> &'static str {
    if m.created.is_empty() {
        "finding"
    } else {
        "repro"
    }
}

/// One finding from a fuzz artifact: the seed, the minimized action sequence,
/// and the source `fuzz.md`'s run dir (for evidence/copying).
pub(super) struct Finding {
    pub(super) id: String,
    pub(super) seed: u64,
    pub(super) actions: Vec<String>,
    pub(super) run_dir: PathBuf,
}

impl Finding {
    /// Scoped content id persisted by fuzz (target + bug + replay identity).
    pub(super) fn id(&self) -> String {
        self.id.clone()
    }

    /// An in-memory `Meta` for a finding that has NOT been kept yet, so `check`
    /// can replay it straight from the fuzz artifact (the "confirm before you
    /// keep" path). It is never written to disk: status is quarantined, with no
    /// alias and no creation stamp. The trigger index is the full minimized
    /// length (the finding fired at the end of its own sequence).
    pub(super) fn pending_meta(&self) -> repro::Meta {
        repro::Meta {
            id: self.id(),
            alias: None,
            status: repro::Status::Quarantined,
            seed: self.seed,
            created: String::new(),
            last_checked: None,
            last_result: None,
            trigger_index: Some(repro::normalize_actions(&self.actions).len()),
            trigger_sig: None,
            trigger_selector: None,
            trigger_fingerprint: None,
            oracle: None,
            record_url: None,
            record_action: None,
        }
    }
}

/// Find a fuzz finding by its content-hash id, scanning EVERY run dir under the
/// evidence out dir (not just the latest), so `reproit <id>` can confirm any
/// finding the last `fuzz` reported, before it is `keep`-ed. Returns the first
/// dir whose `fuzz.md` repro block hashes to `id`.
pub(super) fn find_finding_by_id(loaded: &config::Loaded, id: &str) -> Option<Finding> {
    // Direct `reproit fnd_...` syntax is normalized into the internal raw id
    // before command dispatch. The hidden check route still receives the prefix.
    // Accept both forms at this internal lookup boundary.
    let id = repro::raw_finding_id(id)
        .or_else(|| (id.len() == 12 && id.chars().all(|c| c.is_ascii_hexdigit())).then_some(id))?;
    let durable = layout::finding_dir(&loaded.root, id);
    if let Some(finding) = finding_from_report_dir(&durable, id) {
        return Some(finding);
    }
    let base = loaded.root.join(&loaded.config.evidence.out_dir);
    for e in std::fs::read_dir(&base).ok()?.flatten() {
        let p = e.path();
        if let Some(finding) = finding_from_report_dir(&p, id) {
            return Some(finding);
        }
    }
    None
}

fn finding_from_report_dir(run_dir: &Path, id: &str) -> Option<Finding> {
    if !run_dir.is_dir() {
        return None;
    }
    let md = std::fs::read_to_string(run_dir.join("fuzz.md")).ok()?;
    let (seed, actions) = parse_fuzz_report(&md)?;
    // New reports persist their scoped finding id. Legacy reports remain
    // addressable through their historical seed+actions hash.
    let declared = parse_fuzz_finding_id(&md);
    let matches = declared.as_deref() == Some(id)
        || (declared.is_none() && repro::repro_id(seed, &actions) == id);
    matches.then_some(Finding {
        id: id.to_string(),
        seed,
        actions,
        run_dir: run_dir.to_path_buf(),
    })
}

/// Find the latest fuzz finding artifact: the most-recent run dir under the
/// evidence out dir that holds a `fuzz.md` (the discovered repro report). The
/// out dir doubles as the fuzz artifact root (`fuzz` writes findings there).
pub(super) fn latest_finding(loaded: &config::Loaded) -> Option<Finding> {
    let base = loaded.root.join(&loaded.config.evidence.out_dir);
    let mut runs: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for e in std::fs::read_dir(&base).ok()?.flatten() {
        let p = e.path();
        if p.is_dir() && p.join("fuzz.md").exists() {
            let t = e
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            runs.push((t, p));
        }
    }
    runs.sort_by_key(|(t, _)| *t);
    let (_, run_dir) = runs.pop()?;
    let md = std::fs::read_to_string(run_dir.join("fuzz.md")).ok()?;
    let (seed, actions) = parse_fuzz_report(&md)?;
    Some(Finding {
        id: parse_fuzz_finding_id(&md).unwrap_or_else(|| repro::repro_id(seed, &actions)),
        seed,
        actions,
        run_dir,
    })
}

pub(super) fn parse_fuzz_finding_id(md: &str) -> Option<String> {
    md.lines().find_map(|line| {
        line.trim()
            .strip_prefix("<!-- finding-id:")
            .and_then(|value| value.strip_suffix("-->"))
            .map(str::trim)
            .filter(|value| value.len() == 12 && value.chars().all(|c| c.is_ascii_hexdigit()))
            .map(str::to_string)
    })
}

/// Parse a `fuzz.md` report into (seed, repro actions). The report header is
/// `# fuzz finding (seed N)` and the repro block is the fenced code under a
/// `## confirmed repro (...)` heading (one action per line). Pure, so it is
/// unit-tested.
pub(super) fn parse_fuzz_report(md: &str) -> Option<(u64, Vec<String>)> {
    let seed = md.lines().find_map(|l| {
        let i = l.find("(seed ")? + "(seed ".len();
        let rest = &l[i..];
        let end = rest.find(')')?;
        rest[..end].trim().parse::<u64>().ok()
    })?;
    // The repro block: the first fence after the report writer's confirmed
    // repro heading.
    let mut in_repro_section = false;
    let mut in_fence = false;
    let mut actions = Vec::new();
    for line in md.lines() {
        if line.starts_with("## confirmed repro") {
            in_repro_section = true;
            continue;
        }
        if !in_repro_section {
            continue;
        }
        if line.trim_start().starts_with("```") {
            if in_fence {
                break; // closing fence: repro block done
            }
            in_fence = true;
            continue;
        }
        if in_fence {
            let a = line.trim();
            if !a.is_empty() {
                actions.push(a.to_string());
            }
        }
    }
    Some((seed, actions))
}

/// Parse the `## oracle` block fuzz.md emits into (oracle category, sig,
/// selector, fingerprint). The selector and fingerprint identify the exact
/// subject of a relational or semantic-state finding.
/// The sig
/// is empty for non-graph findings. Returns (None, None) when no block is
/// present (an older report), in which case `check` falls back to the crash
/// path. Pure, so it is unit-tested.
pub(super) fn parse_fuzz_oracle(
    md: &str,
) -> (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
) {
    let field = |key: &str| -> Option<String> {
        md.lines().find_map(|l| {
            let l = l.trim();
            let rest = l.strip_prefix(&format!("- {key}:"))?;
            Some(rest.trim().trim_matches('`').trim().to_string())
        })
    };
    let oracle = field("oracle").filter(|s| !s.is_empty());
    let sig = field("sig").filter(|s| !s.is_empty());
    let selector = field("selector").filter(|s| !s.is_empty());
    let fingerprint = field("fingerprint").filter(|s| !s.is_empty());
    (oracle, sig, selector, fingerprint)
}

/// `keep`: take a finding from the latest fuzz artifact, compute its content
/// hash id, and write the committed store dir + meta.json. The store dir name
/// IS the content hash (stable across machines, self-deduping). Default status
/// is quarantined; `--strict` lands it required. `--as` sets the alias.
pub(super) fn keep_repro(
    ctx: &Ctx,
    loaded: &config::Loaded,
    id: Option<&str>,
    as_name: Option<&str>,
    strict: bool,
) -> Result<()> {
    let root = loaded.root.as_path();
    // Resolve the finding to keep: a specific one by id (any finding the last
    // fuzz reported, so `keep <id>` pairs with `reproit <id>`), or the latest when
    // no id is given.
    let finding = match id {
        Some(want) => find_finding_by_id(loaded, want).ok_or_else(|| {
            anyhow::anyhow!(
                "no fuzz finding with id `{want}` under {}. List ids from the last `reproit \
                 fuzz`, or omit the id to keep the latest finding.",
                loaded.config.evidence.out_dir
            )
        })?,
        None => latest_finding(loaded).ok_or_else(|| {
            anyhow::anyhow!(
                "no fuzz finding under {}. Run `reproit fuzz` first.",
                loaded.config.evidence.out_dir
            )
        })?,
    };
    let computed = finding.id();
    let dir = repro::repro_dir(root, &computed);
    // Repros are content-addressed, so the same case keeps to the same id:
    // re-keeping is a no-op-ish "already saved" that must PRESERVE the existing
    // guard's history (status promotion, check results, created stamp, alias)
    // rather than clobber it back to a fresh quarantine.
    let existing = repro::load_meta(root, &computed);
    std::fs::create_dir_all(&dir)?;
    // Store the replay config so `check` can reproduce the case deterministically.
    let replay = serde_json::json!({ "seed": finding.seed, "replay": finding.actions });
    std::fs::write(
        dir.join("replay.json"),
        serde_json::to_string_pretty(&replay)?,
    )?;
    // Carry the discovering report for human reference (best-effort).
    let _ = std::fs::copy(finding.run_dir.join("fuzz.md"), dir.join("fuzz.md"));
    let finding_evidence = layout::finding_dir(root, &computed).join("run-evidence.json");
    if finding_evidence.exists() {
        std::fs::copy(finding_evidence, dir.join("run-evidence.json"))?;
    }
    let finding_capsule = layout::finding_dir(root, &computed).join("capsule-id");
    if let Ok(id) = std::fs::read_to_string(finding_capsule) {
        std::fs::write(dir.join("capsule-id"), id)?;
    }
    let finding_contract = layout::finding_dir(root, &computed).join("contract.json");
    if finding_contract.exists() {
        std::fs::copy(finding_contract, dir.join("contract.json"))?;
    }
    let finding_backend_contract =
        layout::finding_dir(root, &computed).join("backend-contract.json");
    if finding_backend_contract.exists() {
        std::fs::copy(finding_backend_contract, dir.join("backend-contract.json"))?;
    }

    // Status: a fresh keep lands quarantined (or required with --strict); a
    // RE-keep preserves the existing status, so re-running keep never demotes a
    // guard that already went green (--strict can still upgrade it to required).
    let status = if strict {
        repro::Status::Required
    } else {
        existing
            .as_ref()
            .map(|m| m.status)
            .unwrap_or(repro::Status::Quarantined)
    };
    // Alias: an explicit `--as` sets (or renames) the alias; without it, an
    // existing alias is kept rather than wiped.
    let alias = as_name
        .map(String::from)
        .or_else(|| existing.as_ref().and_then(|m| m.alias.clone()));
    // Record the finding's TRIGGER POINT so `check` can tell "the fix changed
    // downstream navigation" (a miss AFTER the trigger -> still PASS) from "the
    // path to the bug is gone" (a miss BEFORE the trigger -> STALE). The saved
    // `actions` are the minimized sequence that LEADS TO the finding, so the
    // finding fired after performing all of them: the trigger index is that
    // count. (The fuzz report does not currently carry the trigger state sig, so
    // `trigger_sig` stays None and the index does the work.)
    let trigger_index = Some(repro::normalize_actions(&finding.actions).len());
    // Record the finding's ORACLE category and violating state sig. `keep` reads
    // these from the `## oracle` block fuzz.md emits.
    let md = std::fs::read_to_string(finding.run_dir.join("fuzz.md")).unwrap_or_default();
    let (oracle, finding_sig, trigger_selector, trigger_fingerprint) = parse_fuzz_oracle(&md);
    // Crash findings use the exception path; state findings retain the signature
    // for direct recording and existing sig-reached logic.
    let trigger_sig = finding_sig.filter(|s| !s.is_empty());
    let log = std::fs::read_to_string(finding.run_dir.join("drive-a.log")).unwrap_or_default();
    let (record_url, record_action) = web_record_metadata(
        loaded.config.app.url.as_deref(),
        oracle.as_deref(),
        trigger_sig.as_deref(),
        &log,
    );
    let meta = repro::Meta {
        id: computed.clone(),
        alias: alias.clone(),
        status,
        seed: finding.seed,
        // Preserve the original creation stamp on a re-keep; stamp now on a fresh
        // save.
        created: existing
            .as_ref()
            .map(|m| m.created.clone())
            .unwrap_or_else(|| chrono::Local::now().to_rfc3339()),
        last_checked: existing.as_ref().and_then(|m| m.last_checked.clone()),
        last_result: existing.as_ref().and_then(|m| m.last_result.clone()),
        trigger_index,
        trigger_sig,
        trigger_selector,
        trigger_fingerprint,
        oracle,
        record_url,
        record_action,
    };
    repro::save_meta(root, &meta)?;

    // Was this already in the suite? If so, report it as "already saved" (and
    // note an alias rename) instead of pretending it's a fresh keep.
    let prior_alias = existing.as_ref().and_then(|m| m.alias.clone());
    let renamed = match (&prior_alias, as_name) {
        (Some(old), Some(new)) if old != new => Some((old.clone(), new.to_string())),
        _ => None,
    };
    let public_id = repro::display_repro_id(&computed);
    let source_id = repro::display_finding_id(&computed);
    if ctx.json {
        ctx.emit(&serde_json::json!({
            "command": "keep",
            "id": public_id,
            "kind": "repro",
            "source_id": source_id,
            "alias": meta.alias,
            "status": status.as_str(),
            "already_saved": existing.is_some(),
            "renamed_from": renamed.as_ref().map(|(old, _)| old.clone()),
            "seed": finding.seed,
            "actions": finding.actions,
            "dir": dir.to_string_lossy(),
        }));
    } else if existing.is_some() {
        match &renamed {
            Some((old, new)) => ctx.say(format!(
                "  already saved ({}); alias {old} -> {new}",
                public_id
            )),
            None => {
                let label = alias.as_deref().unwrap_or(&public_id);
                ctx.say(format!("  already saved as {label} ({})", status.as_str()));
            }
        }
        ctx.say(format!("  reproduce: reproit {public_id}"));
    } else {
        ctx.say(format!("  kept {} ({})", public_id, status.as_str()));
        if let Some(a) = &alias {
            ctx.say(format!("  alias: {a}"));
        }
        ctx.say(format!("  verify: reproit {public_id}"));
    }
    Ok(())
}

pub(super) async fn simplify_repro(
    ctx: &Ctx,
    config_path: Option<&Path>,
    reference: &str,
    raw_candidate: &str,
) -> Result<ExitCode> {
    let loaded = config::load(config_path)?;
    let meta = repro::resolve(&loaded.root, reference)
        .or_else(|| find_finding_by_id(&loaded, reference).map(|finding| finding.pending_meta()))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no repro or finding `{reference}` (by id or alias). List them with `reproit \
                 repros`."
            )
        })?;
    let current = load_repro_actions(&loaded, &meta.id)?;
    let parsed: Vec<String> = serde_json::from_str(raw_candidate)
        .map_err(|error| anyhow::anyhow!("--to must be a JSON array of action strings: {error}"))?;
    let candidate = repro::normalize_actions(&parsed);
    if candidate.is_empty() {
        anyhow::bail!("--to is empty");
    }
    let times = loaded.config.gate.runs.max(1);
    let (result, _) = check_repro(
        &loaded,
        &meta.id,
        times,
        1,
        None,
        None,
        ctx.json || ctx.quiet,
        Some(&candidate),
    )
    .await?;
    let reproduces = result.outcome == repro::Outcome::Fail;
    let new_id = repro::repro_id(meta.seed, &candidate);
    let adopted = reproduces && candidate.len() <= current.len() && new_id != meta.id;
    if adopted {
        adopt_simplified(&loaded, &meta, &candidate, &new_id)?;
    }
    report_simplification(
        ctx,
        &meta,
        result.outcome,
        current.len(),
        candidate.len(),
        &new_id,
        adopted,
    );
    Ok(ExitCode::SUCCESS)
}

fn report_simplification(
    ctx: &Ctx,
    meta: &repro::Meta,
    outcome: repro::Outcome,
    current_actions: usize,
    candidate_actions: usize,
    new_id: &str,
    adopted: bool,
) {
    let reproduces = outcome == repro::Outcome::Fail;
    if ctx.json {
        ctx.emit(&serde_json::json!({
            "command": "simplify",
            "repro": public_json_id(meta),
            "kind": public_json_kind(meta),
            "reproduces": reproduces,
            "verdict": outcome.as_str(),
            "from_actions": current_actions,
            "to_actions": candidate_actions,
            "adopted": adopted,
            "new_id": adopted.then(|| repro::display_repro_id(new_id)),
            "alias": meta.alias,
        }));
    } else if adopted {
        let tag = meta
            .alias
            .as_deref()
            .map(|alias| format!(" [{alias}]"))
            .unwrap_or_default();
        ctx.say(format!(
            "  simplified {} ({current_actions} actions) -> {} ({candidate_actions} \
             actions){tag}",
            public_json_id(meta),
            repro::display_repro_id(new_id),
        ));
    } else if !reproduces {
        ctx.say(format!(
            "  candidate did NOT reproduce (verdict: {}); kept {}",
            outcome.as_str(),
            public_json_id(meta)
        ));
    } else {
        ctx.say(format!(
            "  candidate reproduces but is not shorter ({candidate_actions} vs \
             {current_actions}); kept {}",
            public_json_id(meta)
        ));
    }
}

/// The action sequence a repro currently replays: from its committed
/// `replay.json`, or from a pending fuzz finding when it hasn't been kept yet.
pub(super) fn load_repro_actions(loaded: &config::Loaded, id: &str) -> Result<Vec<String>> {
    let dir = repro::repro_dir(&loaded.root, id);
    if dir.join("replay.json").exists() {
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("replay.json"))?)?;
        Ok(v.get("replay")
            .and_then(serde_json::Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default())
    } else if let Some(f) = find_finding_by_id(loaded, id) {
        Ok(f.actions)
    } else {
        anyhow::bail!("no repro or finding `{id}`")
    }
}

/// Adopt a verified, simpler action sequence AS the repro: write the new
/// content-hash store dir (carrying the alias, status, and oracle), and remove
/// the superseded one. The trigger is the candidate's full length (a clean
/// agent-proposed repro ends at the action that fires the finding).
/// Build the simplified repro's replay.json: the minimized ACTIONS plus the
/// seed, carrying over the property-matched fixture (`inputs`/`locale`) from
/// the source repro so a data-dependent bug still reproduces after
/// simplification (simplify minimizes actions, never the data). A source
/// without a fixture (a path-only repro, or a pending finding with no
/// replay.json) yields the bare `{seed, replay}`. Pure, so it is unit-tested.
pub(super) fn build_simplified_replay(
    seed: u64,
    candidate: &[String],
    src_replay: &serde_json::Value,
) -> serde_json::Value {
    let mut replay = serde_json::json!({ "seed": seed, "replay": candidate });
    for k in ["inputs", "locale"] {
        if let Some(v) = src_replay.get(k) {
            replay[k] = v.clone();
        }
    }
    replay
}

pub(super) fn adopt_simplified(
    loaded: &config::Loaded,
    meta: &repro::Meta,
    candidate: &[String],
    new_id: &str,
) -> Result<()> {
    let root = loaded.root.as_path();
    let new_dir = repro::repro_dir(root, new_id);
    std::fs::create_dir_all(&new_dir)?;
    // Carry the property-matched fixture (inputs/locale) from the source repro so a
    // data-dependent bug still reproduces after simplification: we minimize
    // ACTIONS, never the data. A path-only repro (or a pending finding with no
    // replay.json) carries neither, so this is inert for non-data bugs.
    let src_replay: serde_json::Value =
        std::fs::read_to_string(repro::repro_dir(root, &meta.id).join("replay.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| serde_json::json!({}));
    let replay = build_simplified_replay(meta.seed, candidate, &src_replay);
    std::fs::write(
        new_dir.join("replay.json"),
        serde_json::to_string_pretty(&replay)?,
    )?;
    let new_meta = repro::Meta {
        id: new_id.to_string(),
        alias: meta.alias.clone(),
        status: meta.status,
        seed: meta.seed,
        created: if meta.created.is_empty() {
            chrono::Local::now().to_rfc3339()
        } else {
            meta.created.clone()
        },
        last_checked: None,
        last_result: None,
        trigger_index: Some(repro::normalize_actions(candidate).len()),
        trigger_sig: meta.trigger_sig.clone(),
        trigger_selector: meta.trigger_selector.clone(),
        trigger_fingerprint: meta.trigger_fingerprint.clone(),
        oracle: meta.oracle.clone(),
        record_url: meta.record_url.clone(),
        record_action: meta.record_action.clone(),
    };
    repro::save_meta(root, &new_meta)?;
    // Carry the discovering report and retire the superseded KEPT repro (a
    // pending finding has no committed dir to remove).
    let old_dir = repro::repro_dir(root, &meta.id);
    if old_dir != new_dir && old_dir.join("replay.json").exists() {
        let _ = std::fs::copy(old_dir.join("fuzz.md"), new_dir.join("fuzz.md"));
        let _ = std::fs::remove_dir_all(&old_dir);
    }
    Ok(())
}

/// Resolve the journey a kept repro replays under (for `record`). Repros
/// replay through the explorer journey, fed the stored replay.json; the journey
/// name carried is the default explorer.
pub(super) fn resolve_repro_journey(root: &std::path::Path, name: &str) -> Result<String> {
    repro::resolve(root, name)
        .ok_or_else(|| anyhow::anyhow!("no repro `{name}` (by id or alias)"))?;
    Ok("explore".to_string())
}

/// Detect an exact reproduction of a stored startup-crash identity in one
/// structured exception stream. Web page exceptions are deliberately written
/// to `exceptions.jsonl` rather than mixed into `drive-a.log`; a zero-action
/// repro fires while the initial page is loading, before an action marker can
/// carry it into a replay segment. Matching the complete normalized structural
/// identity keeps this narrow: a different exception during startup is not a
/// reproduction of the saved bug.
fn matching_startup_crash(exceptions_jsonl: &str, want: &capsule::FindingIdentity) -> bool {
    if want.oracle != "crash" {
        return false;
    }
    exceptions_jsonl
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|exception| {
            !exception
                .get("kind")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .contains("TEST FRAMEWORK")
        })
        .any(|exception| {
            let message = exception
                .get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            capsule::FindingIdentity {
                oracle: "crash".into(),
                invariant: "no-exception".into(),
                kind: "exception".into(),
                message: fuzz::normalize_message(message),
                // Crash identity intentionally excludes browser/source-map
                // frames and runner-only triggers (the same contract used when
                // the causal capsule is persisted).
                frame: String::new(),
                trigger: String::new(),
                boundary: None,
            } == *want
        })
}

fn replay_drive_count(zero_action_schedule: bool, times: u32) -> u32 {
    if zero_action_schedule {
        times.max(1)
    } else {
        1
    }
}

/// Run one repro N times and classify the aggregate outcome (pass/fail/flaky/
/// stale). Each replay writes the stored action sequence to the fuzz config the
/// explorer reads, runs the explorer on the platform's execution tier (headless
/// for Flutter, the real tier for web-cdp/appium/desktop, mirroring how `fuzz`
/// selects), and the per-run drive log is classified by
/// `repro::verdict_from_log`. Returns the result + the last run dir.
#[allow(clippy::too_many_arguments)]
pub(super) async fn check_repro(
    loaded: &config::Loaded,
    id: &str,
    times: u32,
    devices: usize,
    kind: Option<&str>,
    locale: Option<&str>,
    quiet: bool,
    // When set, replay these actions INSTEAD of the repro's saved sequence, but
    // classify against the repro's oracle. This is the verify primitive behind
    // `simplify`: "does this alternate (agent-proposed) sequence still reproduce
    // the same finding?" The seed is kept; the trigger's oracle still selects the
    // crash/graph path.
    override_actions: Option<&[String]>,
) -> Result<(repro::CheckResult, PathBuf)> {
    // Crash-reporter suppression for native check replays (which can crash the
    // target app while re-confirming a crash repro). Inert for web/headless.
    // Restored on Drop, including the early `?` returns below.
    let _crash_guard = match platform::resolve(&loaded.config.app.platform) {
        Some(p) => crashreporter::CrashReporterGuard::engage(p.backend),
        None => crashreporter::CrashReporterGuard::engage_inert(),
    };
    let dir = repro::repro_dir(&loaded.root, id);
    let frozen_contract = crate::model::contracts::FrozenContractGuard::load(
        &dir.join("contract.json"),
    )
    .or_else(|| {
        crate::model::contracts::FrozenContractGuard::load(
            &layout::finding_dir(&loaded.root, id).join("contract.json"),
        )
    });
    let frozen_backend =
        crate::model::backend::FrozenBackendGuard::load(&dir.join("backend-contract.json"))
            .or_else(|| {
                crate::model::backend::FrozenBackendGuard::load(
                    &layout::finding_dir(&loaded.root, id).join("backend-contract.json"),
                )
            });
    // Replay source: a KEPT repro's store (replay.json + meta trigger) when it
    // exists, else a PENDING fuzz finding by id read straight from the artifact,
    // so `reproit <id>` can confirm a finding BEFORE it is `keep`-ed. For the
    // pending case the trigger is derived from the finding itself (full minimized
    // length; oracle/sig from its fuzz.md).
    let (replay, trigger): (serde_json::Value, repro::Trigger) = if dir.join("replay.json").exists()
    {
        let replay = serde_json::from_str(&std::fs::read_to_string(dir.join("replay.json"))?)?;
        // The finding's trigger context, recorded at `keep`. A repro kept
        // before this field existed loads with all None, so the classifier
        // falls back to its first-action heuristic.
        let trigger = match repro::load_meta(&loaded.root, id) {
            Some(m) => repro::Trigger {
                index: m.trigger_index,
                sig: m.trigger_sig,
                selector: m.trigger_selector,
                fingerprint: m.trigger_fingerprint,
                oracle: m.oracle,
            },
            None => repro::Trigger::unknown(),
        };
        (replay, trigger)
    } else if let Some(f) = find_finding_by_id(loaded, id) {
        let md = std::fs::read_to_string(f.run_dir.join("fuzz.md")).unwrap_or_default();
        let (oracle, sig, selector, fingerprint) = parse_fuzz_oracle(&md);
        let replay = serde_json::json!({ "seed": f.seed, "replay": f.actions });
        let trigger = repro::Trigger {
            index: Some(repro::normalize_actions(&f.actions).len()),
            sig,
            selector,
            fingerprint,
            oracle,
        };
        (replay, trigger)
    } else {
        anyhow::bail!(
            "no repro or finding `{id}`; keep it from a fuzz finding (`reproit keep`) or run \
             `reproit fuzz` first"
        );
    };

    // Verify an alternate sequence (simplify): replace ONLY the actions, keeping
    // the seed AND the property-matched fixture (inputs/locale) so the verdict
    // still answers "does this reproduce the SAME finding?". Dropping the fixture
    // here would re-run each candidate WITHOUT the data a data-dependent bug needs,
    // so the minimization would shrink against a bug that never fires (garbage
    // result), and the adopted minimal repro would lose its data and stop
    // reproducing. Clone + overwrite the action list to preserve the rest.
    let replay = match override_actions {
        Some(actions) => {
            let mut r = replay.clone();
            r["replay"] = serde_json::json!(actions);
            r
        }
        None => replay,
    };
    let zero_action_schedule = replay
        .get("replay")
        .and_then(serde_json::Value::as_array)
        .is_some_and(Vec::is_empty);

    // The fuzz config the explorer reads on each replay.
    let cfg_path = layout::fuzz_config_path(&loaded.root);
    std::fs::create_dir_all(cfg_path.parent().unwrap())?;
    let mut defines = vec![(
        "REPROIT_FUZZ_CONFIG".to_string(),
        cfg_path.to_string_lossy().into_owned(),
    )];
    // LOCALE contract: the locale travels to the runner as REPROIT_LOCALE (a
    // dart-define for Flutter, an env var for the rest, both via the
    // orchestrator's define list), so a repro can be replayed under each locale.
    // Precedence: an explicit `--locale` (the cross-locale matrix) wins; otherwise
    // fall back to a `locale` pinned in the stored production replay.json /
    // `reproduce` (the property-matched fixture's locale), so a locale-dependent
    // prod bug reproduces under a plain `reproit @name` without the caller
    // having to remember which locale it came from. The runner reads `inputs` off
    // the config directly, but reads locale ONLY from REPROIT_LOCALE, so the
    // fixture locale must be lifted here.
    let fixture_locale = replay.get("locale").and_then(|v| v.as_str());
    if let Some(loc) = locale.or(fixture_locale) {
        defines.push((crosscut::LOCALE_ENV.to_string(), loc.to_string()));
    }
    let finding_capsule_link = layout::finding_dir(&loaded.root, id).join("capsule-id");
    let kept_capsule_link = dir.join("capsule-id");
    let capsule_id = std::fs::read_to_string(&finding_capsule_link)
        .or_else(|_| std::fs::read_to_string(&kept_capsule_link))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let mut capsule_plaintext = None;
    let mut expected_finding_identity = None;
    if let Some(capsule_id) = capsule_id {
        let capsule = capsule::Capsule::load(&loaded.root, &capsule_id)?;
        let missing = capsule.missing_required_replay_capabilities();
        if !missing.is_empty() {
            anyhow::bail!(
                "capsule `{capsule_id}` cannot replay on `{}`; missing capability: {}",
                loaded.config.app.platform,
                missing.join(", ")
            );
        }
        let guard = capsule::Capsule::materialize_plaintext(&loaded.root, &capsule_id)?;
        defines.push((
            "REPROIT_CAPSULE".into(),
            guard.path().to_string_lossy().into_owned(),
        ));
        expected_finding_identity = Some(capsule.finding.clone());
        capsule_plaintext = Some(guard);
    }

    let _ = devices; // a repro replays on one device; kept for parity.

    // Non-empty action replays retain the fast single-launch batch. A
    // zero-action crash, however, is specifically a startup/load failure: one
    // launch may emit duplicate exception records, but it is still only ONE
    // reproduction attempt. Run those checks as independent cold launches so
    // `--runs 3` means three distinct startup verdicts.
    let drive_count = replay_drive_count(zero_action_schedule, times);
    let mut verdicts = Vec::new();
    let mut last_dir = None;
    for _ in 0..drive_count {
        let config = if zero_action_schedule || times <= 1 {
            replay.clone()
        } else {
            serde_json::json!({ "batch": (0..times).map(|_| replay.clone()).collect::<Vec<_>>() })
        };
        std::fs::write(&cfg_path, config.to_string())?;
        // Select the execution tier the same way `fuzz` does: Flutter replays
        // on the headless tier; every other backend routes through the real
        // tier. `warm: false` is essential for independent startup attempts.
        let outcome = orchestrator::run_journey_tier(
            &loaded.config,
            &loaded.root,
            "explore",
            &orchestrator::RunOpts {
                kind,
                devices: 1,
                warm: false,
                extra_defines: &defines,
                ..Default::default()
            },
            false,
        )
        .await?;
        let full_log =
            std::fs::read_to_string(outcome.run_dir.join("drive-a.log")).unwrap_or_default();
        let exact_startup_crash = zero_action_schedule
            && expected_finding_identity.as_ref().is_some_and(|want| {
                let exceptions = std::fs::read_to_string(outcome.run_dir.join("exceptions.jsonl"))
                    .unwrap_or_default();
                matching_startup_crash(&exceptions, want)
            });
        let replay_batch_completed_cleanly = full_log
            .lines()
            .any(|line| line.trim() == "All tests passed");
        // One segment per replay (the whole log for each zero-action cold run).
        let segments = fuzz::split_log_segments(&full_log);
        for seg in segments {
            // Trust a completed replay over a later browser-cleanup failure.
            let segment_passed = outcome.passed || replay_batch_completed_cleanly;
            let mut verdict = repro::verdict_from_log_with_trigger(seg, segment_passed, &trigger);
            // Mere presence of an exception is insufficient: only the complete
            // stored structural crash identity confirms this launch.
            if exact_startup_crash {
                verdict = repro::RunVerdict::Broke;
            }
            if let Some(guard) = &frozen_contract {
                let parsed = crate::model::runner::ParsedRun::new(seg, &[], true, false);
                if guard.reproduces(&parsed.observations, &parsed.defects) {
                    verdict = repro::RunVerdict::Broke;
                }
            }
            if frozen_backend
                .as_ref()
                .is_some_and(|guard| guard.reproduces(seg))
            {
                verdict = repro::RunVerdict::Broke;
            }
            if !quiet {
                println!(
                    "  run {}/{}: {}",
                    verdicts.len() + 1,
                    times.max(1),
                    verdict.as_str()
                );
            }
            verdicts.push(verdict);
        }
        last_dir = Some(outcome.run_dir);
    }
    let last_dir = last_dir.expect("at least one replay drive");
    drop(capsule_plaintext);
    // Neutralize: a later warm run must not replay this case.
    let _ = std::fs::write(&cfg_path, "{}");
    Ok((repro::CheckResult::from_verdicts(&verdicts), last_dir))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crash_identity(message: &str) -> capsule::FindingIdentity {
        capsule::FindingIdentity {
            oracle: "crash".into(),
            invariant: "no-exception".into(),
            kind: "exception".into(),
            message: fuzz::normalize_message(message),
            frame: String::new(),
            trigger: String::new(),
            boundary: None,
        }
    }

    #[test]
    fn zero_action_startup_crash_requires_exact_stored_identity() {
        let wanted = crash_identity(
            "Module \"node:path\" cannot access \"node:path.isAbsolute\" at localhost:4173",
        );
        let jsonl = concat!(
            r#"{"kind":"EXCEPTION","message":"Module \"node:path\" cannot access "#,
            r#"\"node:path.isAbsolute\" at localhost:4173"}"#,
            "\n",
            r#"{"kind":"EXCEPTION","message":"a different startup failure"}"#,
            "\n",
            r#"{"kind":"EXCEPTION","message":"Module \"node:path\" cannot access "#,
            r#"\"node:path.isAbsolute\" at localhost:9000"}"#,
            "\n",
        );
        // Dynamic ports normalize away, while a different exception does not
        // broaden the match.
        assert!(matching_startup_crash(jsonl, &wanted));
        assert!(!matching_startup_crash(jsonl, &crash_identity("unrelated")));
    }

    #[test]
    fn framework_and_non_crash_exceptions_cannot_confirm_startup_crash() {
        let wanted = crash_identity("boom");
        let jsonl = concat!(
            r#"{"kind":"EXCEPTION CAUGHT BY TEST FRAMEWORK","message":"boom"}"#,
            "\n",
        );
        assert!(!matching_startup_crash(jsonl, &wanted));

        let mut non_crash = wanted;
        non_crash.oracle = "content-bug".into();
        assert!(!matching_startup_crash(jsonl, &non_crash));
    }

    #[test]
    fn requested_zero_action_runs_are_distinct_cold_drives() {
        assert_eq!(replay_drive_count(true, 3), 3);
        assert_eq!(replay_drive_count(true, 0), 1);
        // Action-bearing replays retain one batched drive.
        assert_eq!(replay_drive_count(false, 3), 1);
    }
}
