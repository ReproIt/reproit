//! Framework-agnostic scripted journeys.
//!
//! A journey is a YAML path through the app: a list of steps that reproit
//! replays deterministically and classifies on the same pass/fail/flaky/stale
//! contract as a fuzz repro. A journey is, in effect, a hand-authored repro with
//! (later) assertions. It is framework-agnostic because the steps are the same
//! finder-based actions every backend already executes (`tap:key:testid:add`,
//! `back`, ...); the per-framework runner is the only framework-specific part,
//! and it is reproit's, not the user's.
//!
//! Steps: `do` (explicit action), `goto: <state>` (pathfind the state graph),
//! `expect` (assert `state`/`text`/`count` against the live screen), and `fill`
//! (type into fields,
//! with `secret:` values injected from the auth vault at run time). A journey may
//! also declare `setup: login(<acct>)` / `auth(<acct>)` to establish auth first.
//! Runs are classified pass / fail (a crash on the way) / stale (a step could not
//! be performed, so the app diverged from the map). `map --verify` reuses the
//! same replay machinery to re-walk the map and report drift.
//!
//! Addressing contract (every runner must uphold): a selector resolves against
//! VISIBLE / on-screen elements only. A `key:` is exact; `label:` matches the
//! visible/semantic label of an on-screen element; `role:<role>#<idx>` and a
//! positional `#<n>` index ONLY the visible elements of that kind, never one
//! built-but-offstage (another PageView/IndexedStack/Tab page, a `display:none`
//! node, a collapsed panel). Visibility can't be resolved host-side, only the
//! runner sees the live UI, so each runner enforces it with its native check
//! (Flutter `hitTestable`, web `getBoundingClientRect`+style, Appium
//! `displayed`). Tapping already does this; filling must match. Today only the
//! web and Flutter runners implement `type`/`fill`; any runner that adds it must
//! resolve visible-only from the start.

use crate::appmap::AppMap;
use crate::config;
use crate::map::{action_str, entry_state};
use crate::orchestrator;
use crate::repro;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

/// A journey file (`journeys/<name>.yaml`).
#[derive(Debug, Deserialize)]
pub struct Journey {
    /// Optional display name; defaults to the file stem.
    #[serde(default)]
    #[allow(dead_code)] // surfaced in output once the executor reports per-journey
    pub journey: Option<String>,
    /// Optional auth prelude run before `steps`. Either `login(<account>)`
    /// (drive the `login` journey for that account first) or `auth(<account>)`
    /// (skip the login UI; the runner restores a pre-authenticated session).
    /// The account also binds `secret:` fill values in this journey's steps.
    #[serde(default)]
    pub setup: Option<String>,
    /// Multi-actor: the participating sessions. Either a bare list (no per-actor
    /// auth):
    ///   `actors: [alice, bob]`
    /// or a map binding each actor to a login/auth prelude:
    ///   `actors: {alice: {login: alice}, bob: {auth: bob}}`
    /// When set, every step must name an `actor`, and reproit drives one runner
    /// per actor against the SAME backend, in the listed step order, so one
    /// actor's effect is observable to another (the point of multi-actor).
    #[serde(default)]
    pub actors: ActorList,
    #[serde(default)]
    pub steps: Vec<Step>,
    /// Execution tier override. Scripted journeys default to the SIM tier (real
    /// simulator + real backend + determinism/permission pinning), because they
    /// are E2E by nature: login needs the network, multi-actor needs N sims, and
    /// the in-process headless tier has none of that (and dies on a multi-sim
    /// machine with no `-d`). Set `tier: headless` only for a pure-widget journey
    /// that needs no backend. Multi-actor scenarios are always sim.
    #[serde(default)]
    pub tier: Option<String>,
}

/// A per-actor auth prelude, parsed from the actor's `login`/`auth` config.
#[derive(Debug, Clone)]
struct ActorAuth {
    kind: SetupKind,
    account: String,
}

/// The journey's actors. Deserializes from either a bare list (`[alice, bob]`,
/// no per-actor auth) or a map binding each actor to a login/auth prelude
/// (`{alice: {login: alice}, bob: {auth: bob}}`). Map keys are taken in sorted
/// order so the actor->device-letter assignment is deterministic.
#[derive(Debug, Default)]
pub struct ActorList(Vec<(String, Option<ActorAuth>)>);

impl ActorList {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    fn entries(&self) -> &[(String, Option<ActorAuth>)] {
        &self.0
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ActorsRaw {
    List(Vec<String>),
    Map(BTreeMap<String, ActorCfgRaw>),
}

#[derive(Deserialize)]
struct ActorCfgRaw {
    #[serde(default)]
    login: Option<String>,
    #[serde(default)]
    auth: Option<String>,
    /// Alternate to login/auth: the same string form as top-level setup,
    /// e.g. `setup: "login(alice)"`.
    #[serde(default)]
    setup: Option<String>,
}

impl<'de> Deserialize<'de> for ActorList {
    fn deserialize<D>(d: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = ActorsRaw::deserialize(d)?;
        let entries = match raw {
            ActorsRaw::List(v) => v.into_iter().map(|n| (n, None)).collect(),
            ActorsRaw::Map(m) => {
                let mut out = Vec::new();
                for (name, cfg) in m {
                    let auth = if let Some(acct) = cfg.login {
                        Some(ActorAuth {
                            kind: SetupKind::Login,
                            account: acct,
                        })
                    } else if let Some(acct) = cfg.auth {
                        Some(ActorAuth {
                            kind: SetupKind::Auth,
                            account: acct,
                        })
                    } else if let Some(s) = cfg.setup {
                        let (kind, account) = parse_setup(&s).map_err(serde::de::Error::custom)?;
                        Some(ActorAuth { kind, account })
                    } else {
                        None
                    };
                    out.push((name, auth));
                }
                out
            }
        };
        Ok(ActorList(entries))
    }
}

/// How a journey establishes auth before its own steps.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum SetupKind {
    /// Drive the `login` journey for the account, then run our steps.
    Login,
    /// Bypass the login UI: the runner restores a saved session for the account.
    Auth,
}

/// Parse `login(guest)` / `auth(admin)` into its kind and account handle.
fn parse_setup(s: &str) -> Result<(SetupKind, String)> {
    let s = s.trim();
    let (kind, rest) = if let Some(r) = s.strip_prefix("login(") {
        (SetupKind::Login, r)
    } else if let Some(r) = s.strip_prefix("auth(") {
        (SetupKind::Auth, r)
    } else {
        bail!("setup must be `login(<account>)` or `auth(<account>)`, got `{s}`");
    };
    let acct = rest
        .strip_suffix(')')
        .ok_or_else(|| anyhow::anyhow!("setup `{s}` is missing its closing `)`"))?
        .trim();
    if acct.is_empty() {
        bail!("setup `{s}` names no account");
    }
    Ok((kind, acct.to_string()))
}

/// Uppercase, non-alphanumeric -> underscore, matching `auth::secret_env`'s env
/// naming so `secret:` placeholders line up with the injected `REPROIT_SECRET_*`.
fn env_ident(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

/// Expand a `fill` value. A `secret:` value becomes a `${REPROIT_SECRET_..}`
/// placeholder the runner substitutes from env (so plaintext never hits disk):
///   `secret:password`        -> the setup account's password
///   `secret:admin.password`  -> account `admin`'s password (explicit override)
/// Any other value is typed literally.
fn resolve_fill_value(value: &str, account: Option<&str>) -> Result<String> {
    let Some(spec) = value.strip_prefix("secret:") else {
        return Ok(value.to_string());
    };
    let (acct, field) = match spec.split_once('.') {
        Some((a, f)) => (a.to_string(), f),
        None => {
            let a = account.ok_or_else(|| {
                anyhow::anyhow!(
                    "`secret:{spec}` needs an account: name it (`secret:<acct>.{spec}`) or add `setup: login(<acct>)`"
                )
            })?;
            (a.to_string(), spec)
        }
    };
    Ok(format!(
        "${{REPROIT_SECRET_{}_{}}}",
        env_ident(&acct),
        env_ident(field)
    ))
}

/// One step. Exactly one of `do`/`goto`/`expect`/`fill` is set.
#[derive(Debug, Deserialize)]
pub struct Step {
    /// Multi-actor only: which actor performs this step. Required (and must be
    /// one of the journey's `actors`) when the journey declares `actors`.
    #[serde(default)]
    pub actor: Option<String>,
    /// An explicit finder-action, e.g. `tap:key:testid:add` or `back`.
    #[serde(default, rename = "do")]
    pub do_action: Option<String>,
    /// Navigate to a named/keyed state: reproit pathfinds the graph.
    #[serde(default)]
    pub goto: Option<String>,
    /// Assert something holds.
    #[serde(default)]
    pub expect: Option<Expect>,
    /// Type values into fields: a map of finder -> value. A `secret:<field>`
    /// value is injected from the auth vault at run time; anything else is typed
    /// literally. See `resolve_fill_value`.
    #[serde(default)]
    pub fill: Option<std::collections::BTreeMap<String, String>>,
}

#[derive(Debug, Deserialize)]
pub struct Expect {
    /// Expected current state (by name/label). Needs a map.
    #[serde(default)]
    pub state: Option<String>,
    /// Visible text that must be present on the screen (substring match).
    #[serde(default)]
    pub text: Option<String>,
    /// Expected element counts: a map of finder -> exact count (e.g. how many
    /// list items are showing). 0 asserts absence.
    #[serde(default)]
    pub count: Option<std::collections::BTreeMap<String, u32>>,
}

/// Where journeys live, relative to the project root.
pub fn journeys_dir(root: &Path) -> PathBuf {
    root.join("journeys")
}

/// The file backing a journey name.
pub fn journey_path(root: &Path, name: &str) -> PathBuf {
    journeys_dir(root).join(format!("{name}.yaml"))
}

/// Whether a journey by this name exists on disk.
pub fn exists(root: &Path, name: &str) -> bool {
    journey_path(root, name).exists()
}

fn load(root: &Path, name: &str) -> Result<Journey> {
    let p = journey_path(root, name);
    let raw = std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
    serde_yaml::from_str(&raw).with_context(|| format!("parsing {}", p.display()))
}

/// Load the committed app map, if one has been built.
fn load_map(root: &Path) -> Option<AppMap> {
    let p = root.join(".reproit/appmap.json");
    serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()
}

/// Does `name` (a state key) or its description match the journey's `target`?
fn state_matches(map: &AppMap, name: &str, needle: &str) -> bool {
    name.to_lowercase().contains(needle)
        || map
            .states
            .get(name)
            .map(|s| s.description.to_lowercase().contains(needle))
            .unwrap_or(false)
}

/// The target state of the edge from `from` whose replay action equals `action`.
fn edge_target(map: &AppMap, from: &str, action: &str) -> Option<String> {
    map.transitions
        .iter()
        .find(|t| t.from == from && action_str(&t.action) == action)
        .map(|t| t.to.clone())
}

/// Shortest action path from `from` to a state matching `target`. Returns the
/// reached state key and the replay actions to get there (empty if `from`
/// already matches). None when no path exists in the map.
fn path_from(map: &AppMap, from: &str, target: &str) -> Option<(String, Vec<String>)> {
    let needle = target.to_lowercase();
    if state_matches(map, from, &needle) {
        return Some((from.to_string(), Vec::new()));
    }
    let mut adj: BTreeMap<&str, Vec<(String, &str)>> = BTreeMap::new();
    for t in &map.transitions {
        adj.entry(t.from.as_str())
            .or_default()
            .push((action_str(&t.action), t.to.as_str()));
    }
    let mut q = VecDeque::new();
    let mut prev: BTreeMap<&str, (&str, String)> = BTreeMap::new();
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    q.push_back(from);
    seen.insert(from);
    let mut goal: Option<&str> = None;
    'bfs: while let Some(cur) = q.pop_front() {
        for (act, to) in adj.get(cur).into_iter().flatten() {
            if seen.insert(to) {
                prev.insert(to, (cur, act.clone()));
                if state_matches(map, to, &needle) {
                    goal = Some(to);
                    break 'bfs;
                }
                q.push_back(to);
            }
        }
    }
    let goal = goal?;
    let mut path = Vec::new();
    let mut node = goal;
    while let Some((parent, act)) = prev.get(node) {
        path.push(act.clone());
        node = parent;
    }
    path.reverse();
    Some((goal.to_string(), path))
}

/// A resolved journey: the replay action sequence. `expect` steps compile to
/// inline `assert:` actions the runner evaluates against the live screen and
/// reports via `FUZZ:ASSERT`, so there is nothing positional to track here.
#[derive(Default)]
struct Plan {
    actions: Vec<String>,
}

/// Resolve a state reference (key or description substring) to its signature.
fn resolve_state_sig(map: &AppMap, target: &str) -> Option<String> {
    let needle = target.to_lowercase();
    map.states
        .keys()
        .find(|k| state_matches(map, k, &needle))
        .map(|k| k.strip_prefix("s_").unwrap_or(k).to_string())
}

/// Resolve a journey into its replay actions and `expect`-state signatures.
/// Tracks the current state so each `goto` pathfinds from where the previous
/// step left off.
fn resolve(map: Option<&AppMap>, j: &Journey, account: Option<&str>) -> Result<Plan> {
    let mut actions = Vec::new();
    let mut current: Option<String> = map.and_then(entry_state);
    for (i, step) in j.steps.iter().enumerate() {
        let n = i + 1;
        match (&step.do_action, &step.goto, &step.expect, &step.fill) {
            (Some(a), None, None, None) => {
                actions.push(a.clone());
                // Advance the known state iff the graph has this exact edge;
                // an unknown edge leaves the state unknown (a later goto errors).
                current = match (map, current.as_deref()) {
                    (Some(m), Some(c)) => edge_target(m, c, a),
                    _ => None,
                };
            }
            (None, Some(target), None, None) => {
                let m = map.ok_or_else(|| {
                    anyhow::anyhow!(
                        "step {n}: `goto: {target}` needs a map; run `reproit map` first"
                    )
                })?;
                let from = current.clone().ok_or_else(|| {
                    anyhow::anyhow!(
                        "step {n}: `goto: {target}` from an unknown state (a prior `do` left a state not in the map)"
                    )
                })?;
                let (reached, path) = path_from(m, &from, target).ok_or_else(|| {
                    anyhow::anyhow!(
                        "step {n}: no path to `{target}` from the current state in the map"
                    )
                })?;
                actions.extend(path);
                current = Some(reached);
            }
            (None, None, Some(e), None) => {
                // Each assertion compiles to an `assert:` action evaluated against
                // the live screen at this point in the replay. They don't move the
                // known state.
                let mut any = false;
                if let Some(state) = &e.state {
                    let m = map.ok_or_else(|| {
                        anyhow::anyhow!(
                            "step {n}: `expect: state` needs a map; run `reproit map` first"
                        )
                    })?;
                    let sig = resolve_state_sig(m, state).ok_or_else(|| {
                        anyhow::anyhow!("step {n}: no state matching `{state}` in the map")
                    })?;
                    actions.push(format!("assert:state={sig}"));
                    any = true;
                }
                if let Some(text) = &e.text {
                    actions.push(format!("assert:text={text}"));
                    any = true;
                }
                if let Some(counts) = &e.count {
                    for (finder, want) in counts {
                        actions.push(format!("assert:count:{finder}={want}"));
                    }
                    any = true;
                }
                if !any {
                    bail!("step {n}: `expect` needs one of `state`, `text`, or `count`");
                }
            }
            (None, None, None, Some(fields)) => {
                // Fill is sugar for explicit type actions, one per field. A
                // `secret:` value becomes a `${REPROIT_SECRET_..}` placeholder the
                // runner resolves from env; everything else is typed literally.
                for (finder, value) in fields {
                    let v = resolve_fill_value(value, account)
                        .with_context(|| format!("step {n}: fill `{finder}`"))?;
                    actions.push(format!("type:{finder}={v}"));
                }
                current = None; // typing may move off the known graph
            }
            (None, None, None, None) => {
                bail!("step {n}: empty step (needs `do`/`goto`/`expect`/`fill`)")
            }
            _ => bail!("step {n}: a step takes exactly one of `do`/`goto`/`expect`/`fill`"),
        }
    }
    Ok(Plan { actions })
}

/// Resolve a journey into a runnable plan, including its `setup` auth prelude.
/// `login(acct)` prepends the `login` journey's actions; `auth(acct)` prepends a
/// single `auth:<acct>` bypass action the runner restores a session from. The
/// account binds `secret:` fills across both the prelude and the journey itself.
fn build_plan(root: &Path, map: Option<&AppMap>, j: &Journey) -> Result<Plan> {
    let setup = match &j.setup {
        Some(s) => Some(parse_setup(s)?),
        None => None,
    };
    let account = setup.as_ref().map(|(_, a)| a.as_str());

    let mut prelude = Plan::default();
    if let Some((kind, acct)) = &setup {
        match kind {
            SetupKind::Login => {
                if !exists(root, "login") {
                    bail!(
                        "`setup: login({acct})` needs a `login` journey; create journeys/login.yaml"
                    );
                }
                let login = load(root, "login")
                    .with_context(|| format!("loading the `login` journey for setup({acct})"))?;
                if login.setup.is_some() {
                    bail!("the `login` journey must not itself declare `setup` (would recurse)");
                }
                prelude = resolve(map, &login, Some(acct))
                    .with_context(|| format!("resolving the `login` journey for {acct}"))?;
            }
            SetupKind::Auth => {
                prelude.actions.push(format!("auth:{acct}"));
            }
        }
    }

    let mut main = resolve(map, j, account)?;
    // The prelude runs first, then the journey's own actions.
    let mut actions = prelude.actions;
    actions.append(&mut main.actions);
    Ok(Plan { actions })
}

/// Load a journey by NAME (`journeys/<name>.yaml`, like any journey target) or
/// by a direct PATH (`./flows/login.yaml`). A value with a slash or a
/// `.yaml`/`.yml` extension that exists on disk is read directly; otherwise it
/// resolves as a journey name. This lets `fuzz --from` point at a freshly
/// `import`ed flow wherever it was written.
fn load_target(root: &Path, name_or_path: &str) -> Result<Journey> {
    let p = Path::new(name_or_path);
    let looks_like_path = name_or_path.contains('/')
        || matches!(p.extension().and_then(|e| e.to_str()), Some("yaml" | "yml"));
    if looks_like_path && p.is_file() {
        let raw = std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?;
        return serde_yaml::from_str(&raw).with_context(|| format!("parsing {}", p.display()));
    }
    load(root, name_or_path)
}

/// Resolve a single-actor journey into its replay action sequence, with secrets
/// bound, for use as a `fuzz --from` prefix. The fuzzer replays these actions to
/// land the app in the journey's end state, then branches the seeded walk
/// outward from there: an imported/recorded flow becomes the launchpad for the
/// bugs it never covered. Multi-actor journeys are rejected (no single linear
/// path to branch a walk from).
pub fn prefix_actions(loaded: &config::Loaded, name_or_path: &str) -> Result<Vec<String>> {
    let j = load_target(&loaded.root, name_or_path)?;
    if !j.actors.is_empty() || j.steps.iter().any(|s| s.actor.is_some()) {
        bail!(
            "`fuzz --from` needs a single-actor journey; `{name_or_path}` is multi-actor \
             (there is no single path to branch a walk from)"
        );
    }
    let map = load_map(&loaded.root);
    let plan = build_plan(&loaded.root, map.as_ref(), &j)?;
    if plan.actions.is_empty() {
        bail!("journey `{name_or_path}` has no actions to replay");
    }
    let secrets = crate::auth::secret_env(&loaded.config.auth, &loaded.root).unwrap_or_default();
    Ok(plan
        .actions
        .iter()
        .map(|a| crate::auth::resolve_placeholders(a, &secrets))
        .collect())
}

/// Classify one journey replay. A crash on the way is a real failure (Broke); a
/// step that could not be performed (`FUZZ:MISS`) or an assertion that no longer
/// holds (`FUZZ:ASSERT fail`) means the app diverged from what the journey
/// assumed, which is stale, not a bug (CouldNotReplay); a clean run is Green.
fn classify_run(log: &str, drive_passed: bool) -> repro::RunVerdict {
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

/// Run a journey `times` times and aggregate to pass/fail/flaky/stale. Replays
/// the journey's actions through the same execution tier `check` uses for repros.
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
    let map = load_map(&loaded.root);
    let plan = build_plan(&loaded.root, map.as_ref(), &j)?;
    if plan.actions.is_empty() {
        bail!("journey `{name}` has no actions to run");
    }

    // Resolve ${REPROIT_SECRET_*} placeholders host-side, so the runner types the
    // real value with no vault/secret code of its own (framework-agnostic).
    let secrets = crate::auth::secret_env(&loaded.config.auth, &loaded.root).unwrap_or_default();
    let actions: Vec<String> = plan
        .actions
        .iter()
        .map(|a| crate::auth::resolve_placeholders(a, &secrets))
        .collect();

    // Scripted journeys are E2E: run them on the sim tier unless explicitly
    // opted into headless.
    let sim = sim_tier(&j);
    let total = times.max(1);
    let mut verdicts = Vec::new();
    for i in 1..=total {
        let (log, passed) = run_replay(loaded, &actions, i > 1, sim).await?;
        let verdict = classify_run(&log, passed);
        if !quiet {
            println!("  run {i}/{total}: {}", verdict.as_str());
        }
        verdicts.push(verdict);
    }
    Ok(repro::CheckResult::from_verdicts(&verdicts))
}

/// Scripted journeys are E2E by default: real sim + backend, not the in-process
/// headless tier (no backend, no sim, can't satisfy login or multi-actor).
/// `tier: headless` opts a pure-widget journey out.
fn sim_tier(j: &Journey) -> bool {
    j.tier.as_deref() != Some("headless")
}

// ---- multi-actor ---------------------------------------------------------

/// Resolve one step to its replay action(s), for multi-actor where there is no
/// per-actor map: only `do`, `fill`, and `expect: text`/`count` are supported
/// (`goto` and `expect: state` need a single-actor map). `secret:` fills resolve
/// against the step's actor account (bound via the actor's `login`/`auth`).
fn resolve_scenario_step(step: &Step, n: usize, account: Option<&str>) -> Result<Vec<String>> {
    match (&step.do_action, &step.goto, &step.expect, &step.fill) {
        (Some(a), None, None, None) => Ok(vec![a.clone()]),
        (None, Some(_), None, None) => {
            bail!("step {n}: `goto` is single-actor only (it needs the map); use explicit `do` actions")
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
fn actor_prelude(root: &Path, map: Option<&AppMap>, a: &ActorAuth) -> Result<Vec<String>> {
    match a.kind {
        SetupKind::Auth => Ok(vec![format!("auth:{}", a.account)]),
        SetupKind::Login => {
            if !exists(root, "login") {
                bail!(
                    "actor login({}) needs a `login` journey; create journeys/login.yaml",
                    a.account
                );
            }
            let login = load(root, "login")
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
type Scenario = (Vec<String>, Vec<(String, String)>);

/// Compile a multi-actor journey to (actor list, tagged actions). Each tagged
/// action is `(actor, action)`; the runner drives them in this exact order:
/// every actor's auth prelude first (in actor order), then the interleaved
/// steps. The conductor enforces the global ordering at run time.
fn build_scenario(root: &Path, map: Option<&AppMap>, j: &Journey) -> Result<Scenario> {
    if j.setup.is_some() {
        bail!("multi-actor journeys bind auth per actor (e.g. `actors: {{alice: {{login: alice}}}}`), not via top-level `setup`");
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
fn warn_uncovered_reset_accounts(cfg: &config::Config, j: &Journey) {
    use crate::config::ResetStep;
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
                "  warn: scenario uses account `{name}` (userId {uid}) but no reset step clears it; stale data may leak between runs"
            );
        }
    }
}

/// Run a multi-actor journey universally: launch the host conductor (the strict
/// step-order barrier) and N device runners (one per actor, any backend), each
/// pulling its own actions from the conductor. Classification is the same
/// log-based contract as single-actor, aggregated across every device log.
async fn run_scenario(
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
    if !crate::platform::speaks_barrier(platform) {
        bail!(
            "platform `{platform}` has no multi-actor runner yet; authored multi-user scenarios \
             currently run on: web-playwright, flutter-ios-sim"
        );
    }
    let map = load_map(&loaded.root);
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
    let secrets = crate::auth::secret_env(&loaded.config.auth, &loaded.root).unwrap_or_default();
    let script: Vec<(usize, String)> = tagged
        .iter()
        .map(|(actor, action)| {
            (
                idx[actor.as_str()],
                crate::auth::resolve_placeholders(action, &secrets),
            )
        })
        .collect();

    let total = times.max(1);
    let mut verdicts = Vec::new();
    for i in 1..=total {
        // A fresh conductor per run: it owns ordering for this run only.
        let conductor = crate::barrier::Conductor::start(script.clone(), n).await?;
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
                crate::barrier::Stage::AwaitingJoin(missing) => {
                    let names: Vec<String> = missing
                        .iter()
                        .map(|&i| format!("{} ({})", crate::barrier::letter(i), who(i)))
                        .collect();
                    println!(
                        "  scenario stalled: actor(s) never joined: {}",
                        names.join(", ")
                    );
                }
                crate::barrier::Stage::Stalled { dev, action, secs } => {
                    println!(
                        "  scenario stalled: actor {} ({}) never completed `{action}` after {secs}s",
                        crate::barrier::letter(dev),
                        who(dev),
                    );
                }
                _ => {}
            }
        }
        if !quiet {
            println!("  run {i}/{total}: {}", verdict.as_str());
        }
        verdicts.push(verdict);
    }
    Ok(repro::CheckResult::from_verdicts(&verdicts))
}

/// Concatenate every device's drive log (`drive-a.log`, `drive-b.log`, ...) so
/// classification sees a crash or missed step on ANY actor.
fn read_device_logs(run_dir: &Path, n: usize) -> String {
    (0..n)
        .map(|i| (b'a' + i as u8) as char)
        .filter_map(|label| {
            std::fs::read_to_string(run_dir.join(format!("drive-{label}.log"))).ok()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Replay an action sequence once through the same tier `check` uses, returning
/// the drive log and whether the harness reported a clean run. `warm` reuses the
/// previous build.
async fn run_replay(
    loaded: &config::Loaded,
    actions: &[String],
    warm: bool,
    sim: bool,
) -> Result<(String, bool)> {
    run_replay_cfg(
        loaded,
        serde_json::json!({ "seed": 0, "replay": actions }),
        warm,
        sim,
    )
    .await
}

/// Replay an arbitrary fuzz-config value (single-actor or multi-actor) once.
/// `sim` picks the tier: true = real simulator + backend (what E2E journeys
/// need), false = the in-process headless tier (pure-widget fuzzing only).
async fn run_replay_cfg(
    loaded: &config::Loaded,
    cfg: serde_json::Value,
    warm: bool,
    sim: bool,
) -> Result<(String, bool)> {
    let cfg_path = loaded.root.join(".reproit/fuzz_config.json");
    std::fs::create_dir_all(cfg_path.parent().unwrap())?;
    std::fs::write(&cfg_path, cfg.to_string())?;
    let defines = vec![(
        "REPROIT_FUZZ_CONFIG".to_string(),
        cfg_path.to_string_lossy().into_owned(),
    )];
    let outcome = orchestrator::run_journey_tier(
        &loaded.config,
        &loaded.root,
        "explore",
        &orchestrator::RunOpts {
            devices: 1,
            warm,
            extra_defines: &defines,
            ..Default::default()
        },
        sim,
    )
    .await?;
    let log = std::fs::read_to_string(outcome.run_dir.join("drive-a.log")).unwrap_or_default();
    Ok((log, outcome.passed))
}

// ---- map --verify --------------------------------------------------------

/// One replayed action's outcome, reconstructed from the drive log so positional
/// alignment survives misses: a missed action emits no `FUZZ:STATE`, so we track
/// the per-action state by walking `FUZZ:ACT` / `FUZZ:MISS` / `FUZZ:STATE` in
/// order rather than by counting `FUZZ:STATE` lines.
struct ReplayStep {
    missed: bool,
    state_after: Option<String>,
}

/// Parse a replay drive log into the initial state and a per-action outcome.
fn replay_trace(log: &str) -> (Option<String>, Vec<ReplayStep>) {
    let mut initial = None;
    let mut steps: Vec<ReplayStep> = Vec::new();
    for line in log.lines() {
        if line.contains("FUZZ:ACT ") {
            steps.push(ReplayStep {
                missed: false,
                state_after: None,
            });
        } else if line.contains("FUZZ:MISS ") {
            if let Some(s) = steps.last_mut() {
                s.missed = true;
            }
        } else if let Some(i) = line.find("FUZZ:STATE ") {
            let sig = line[i + "FUZZ:STATE ".len()..]
                .split_whitespace()
                .next()
                .map(str::to_string);
            match steps.last_mut() {
                Some(s) => s.state_after = sig,
                None => initial = sig, // emitted before the first action
            }
        }
    }
    (initial, steps)
}

/// BFS shortest path (as transition indices) from state key `from` to key `to`.
fn edge_path_to(map: &AppMap, from: &str, to: &str) -> Option<Vec<usize>> {
    if from == to {
        return Some(Vec::new());
    }
    let mut q = VecDeque::new();
    let mut prev: BTreeMap<&str, (usize, &str)> = BTreeMap::new();
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    q.push_back(from);
    seen.insert(from);
    while let Some(cur) = q.pop_front() {
        for (ti, t) in map.transitions.iter().enumerate() {
            if t.from == cur && seen.insert(t.to.as_str()) {
                prev.insert(t.to.as_str(), (ti, cur));
                if t.to == to {
                    let mut path = vec![ti];
                    let mut node = cur;
                    while node != from {
                        let (pti, pfrom) = prev[node];
                        path.push(pti);
                        node = pfrom;
                    }
                    path.reverse();
                    return Some(path);
                }
                q.push_back(t.to.as_str());
            }
        }
    }
    None
}

/// A single walk that traverses every reachable edge once, with the transition
/// each action exercises, plus edges with no path from the entry state.
struct VerifyPlan {
    actions: Vec<String>,
    edge_at: Vec<usize>,
    unreachable: Vec<usize>,
}

/// Greedily build an edge-covering walk from the entry state: repeatedly take
/// the untaken edge reachable by the shortest path, pathfinding to its `from`
/// and then crossing it. Navigation also covers any edges it traverses, so the
/// whole reachable graph is checked in one device run.
fn cover_walk(map: &AppMap) -> Result<VerifyPlan> {
    let entry = entry_state(map).ok_or_else(|| anyhow::anyhow!("map has no entry state"))?;
    let mut untaken: BTreeSet<usize> = (0..map.transitions.len()).collect();
    let mut actions = Vec::new();
    let mut edge_at = Vec::new();
    let mut current = entry;
    while !untaken.is_empty() {
        let mut best: Option<(usize, Vec<usize>)> = None;
        for &ti in &untaken {
            if let Some(path) = edge_path_to(map, &current, &map.transitions[ti].from) {
                if best.as_ref().is_none_or(|(_, p)| path.len() < p.len()) {
                    best = Some((ti, path));
                }
            }
        }
        let Some((ti, mut path)) = best else { break };
        path.push(ti); // navigation edges, then the target edge
        for pti in path {
            let t = &map.transitions[pti];
            actions.push(action_str(&t.action));
            edge_at.push(pti);
            current = t.to.clone();
            untaken.remove(&pti);
        }
    }
    Ok(VerifyPlan {
        actions,
        edge_at,
        unreachable: untaken.into_iter().collect(),
    })
}

/// A drifted edge: the app no longer lands where the map says it should.
pub struct Drift {
    pub from: String,
    pub action: String,
    pub expected: String,
    pub observed: String,
}

/// The result of `map --verify`.
pub struct VerifyReport {
    pub edges: usize,
    pub ok: usize,
    pub entry_drift: Option<(String, String)>, // (expected, observed)
    pub drift: Vec<Drift>,
    pub missed: Vec<(String, String)>, // (from, action) the app could not perform
    pub unreachable: Vec<(String, String)>, // (from, action) no path from entry
    pub crashed: bool,
}

impl VerifyReport {
    /// Clean iff nothing drifted, missed, was unreachable, and no crash.
    pub fn is_clean(&self) -> bool {
        self.entry_drift.is_none()
            && self.drift.is_empty()
            && self.missed.is_empty()
            && self.unreachable.is_empty()
            && !self.crashed
    }

    pub fn print(&self) {
        if let Some((exp, obs)) = &self.entry_drift {
            println!("  DRIFT entry: map says {exp}, app boots {obs}");
        }
        for d in &self.drift {
            println!(
                "  DRIFT {} --{}--> map says {}, app reached {}",
                short(&d.from),
                d.action,
                d.expected,
                d.observed
            );
        }
        for (from, action) in &self.missed {
            println!(
                "  MISS  {} --{}--> action no longer available",
                short(from),
                action
            );
        }
        for (from, action) in &self.unreachable {
            println!(
                "  UNREACHABLE {} --{}--> no path from entry",
                short(from),
                action
            );
        }
        if self.crashed {
            println!("  CRASH the app threw while walking the map");
        }
        if self.is_clean() {
            println!("map verified: {}/{} edges still hold", self.ok, self.edges);
        } else {
            println!(
                "map drifted: {}/{} edges hold ({} drift, {} miss, {} unreachable)",
                self.ok,
                self.edges,
                self.drift.len() + self.entry_drift.iter().count(),
                self.missed.len(),
                self.unreachable.len(),
            );
        }
    }
}

/// Strip the `s_` state-key prefix for display.
fn short(key: &str) -> &str {
    key.strip_prefix("s_").unwrap_or(key)
}

/// Re-walk the committed map and report where the app has drifted from it. One
/// device run covers every reachable edge; each edge's landing state is compared
/// to what the map recorded. This is the "is the map still valid?" check.
pub async fn verify_map(loaded: &config::Loaded, quiet: bool) -> Result<VerifyReport> {
    let map = load_map(&loaded.root).ok_or_else(|| {
        anyhow::anyhow!("no map at .reproit/appmap.json; run `reproit map` first")
    })?;
    let edges = map.transitions.len();
    if edges == 0 {
        return Ok(VerifyReport {
            edges: 0,
            ok: 0,
            entry_drift: None,
            drift: Vec::new(),
            missed: Vec::new(),
            unreachable: Vec::new(),
            crashed: false,
        });
    }
    let plan = cover_walk(&map)?;
    // map --verify re-walks the real app the same way build_map did (sim tier);
    // the headless tier has no backend and dies on a multi-sim host.
    let (log, passed) = run_replay(loaded, &plan.actions, false, true).await?;
    let (initial, steps) = replay_trace(&log);

    let entry = entry_state(&map).unwrap();
    let entry_sig = short(&entry).to_string();
    let entry_drift = match &initial {
        Some(obs) if *obs != entry_sig => Some((entry_sig.clone(), obs.clone())),
        _ => None,
    };

    let mut drift = Vec::new();
    let mut missed = Vec::new();
    let mut ok = 0usize;
    for (k, &ti) in plan.edge_at.iter().enumerate() {
        let t = &map.transitions[ti];
        let action = action_str(&t.action);
        let expected = short(&t.to).to_string();
        match steps.get(k) {
            Some(s) if s.missed || s.state_after.is_none() => {
                missed.push((t.from.clone(), action));
            }
            Some(s) => {
                let obs = s.state_after.as_ref().unwrap();
                if *obs == expected {
                    ok += 1;
                } else {
                    drift.push(Drift {
                        from: t.from.clone(),
                        action,
                        expected,
                        observed: obs.clone(),
                    });
                }
            }
            None => missed.push((t.from.clone(), action)), // log truncated
        }
    }
    let unreachable = plan
        .unreachable
        .iter()
        .map(|&ti| {
            let t = &map.transitions[ti];
            (t.from.clone(), action_str(&t.action))
        })
        .collect();

    let report = VerifyReport {
        edges,
        ok,
        entry_drift,
        drift,
        missed,
        unreachable,
        crashed: !passed,
    };
    if !quiet {
        report.print();
    }
    Ok(report)
}

// ---- authoring (MCP / agent) --------------------------------------------

/// A one-line summary of a saved journey, for `journey list` / the MCP bridge.
#[derive(Serialize)]
pub struct JourneySummary {
    pub name: String,
    pub steps: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub setup: Option<String>,
    /// Set when the file is present but does not parse, so a listing still
    /// surfaces a broken journey rather than dropping it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// List saved journeys (alphabetical), each with a short summary.
pub fn list(root: &Path) -> Result<Vec<JourneySummary>> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(journeys_dir(root)) else {
        return Ok(out); // no journeys/ dir yet
    };
    let mut names: Vec<String> = rd
        .flatten()
        .filter(|e| {
            e.path()
                .extension()
                .is_some_and(|x| x == "yaml" || x == "yml")
        })
        .filter_map(|e| {
            e.path()
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
        })
        .collect();
    names.sort();
    for name in names {
        match load(root, &name) {
            Ok(j) => out.push(JourneySummary {
                name,
                steps: j.steps.len(),
                setup: j.setup,
                error: None,
            }),
            Err(err) => out.push(JourneySummary {
                name,
                steps: 0,
                setup: None,
                error: Some(err.to_string()),
            }),
        }
    }
    Ok(out)
}

/// Structural validation independent of any map: every step takes exactly one of
/// `do`/`goto`/`expect`/`fill`, `expect` carries an assertion, and `setup` (if
/// present) is well-formed. Path/finder validity is checked later by `check`
/// against the live app, the stronger signal.
fn validate_structure(j: &Journey) -> Result<()> {
    if let Some(s) = &j.setup {
        parse_setup(s)?;
    }
    if j.steps.is_empty() {
        bail!("journey has no steps");
    }
    for (i, step) in j.steps.iter().enumerate() {
        let n = i + 1;
        let set = [
            step.do_action.is_some(),
            step.goto.is_some(),
            step.expect.is_some(),
            step.fill.is_some(),
        ];
        match set.iter().filter(|x| **x).count() {
            1 => {}
            0 => bail!("step {n}: empty (needs `do`/`goto`/`expect`/`fill`)"),
            _ => bail!("step {n}: takes exactly one of `do`/`goto`/`expect`/`fill`"),
        }
        if let Some(e) = &step.expect {
            if e.state.is_none() && e.text.is_none() && e.count.is_none() {
                bail!("step {n}: `expect` needs one of `state`/`text`/`count`");
            }
        }
    }
    Ok(())
}

/// Create or overwrite `journeys/<name>.yaml` from a JSON spec
/// (`{"setup"?, "steps":[...]}`). Validates the structure before writing, then
/// emits clean YAML. Returns the written path.
pub fn save(root: &Path, name: &str, spec_json: &str) -> Result<PathBuf> {
    if name.is_empty() || name.contains(['/', '\\', '.']) {
        bail!("invalid journey name `{name}` (no path separators or dots)");
    }
    let value: serde_json::Value =
        serde_json::from_str(spec_json).context("spec is not valid JSON")?;
    // Round-trip through the typed Journey to validate fields and shape.
    let journey: Journey = serde_json::from_value(value.clone())
        .context("spec is not a valid journey (check step keys: do/goto/expect/fill)")?;
    validate_structure(&journey)?;
    // Serialize the original JSON value (preserves the author's key order) as YAML.
    let yaml = serde_yaml::to_string(&value).context("serializing the journey to YAML")?;
    let dir = journeys_dir(root);
    std::fs::create_dir_all(&dir)?;
    let path = journey_path(root, name);
    std::fs::write(&path, yaml).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_scenario_tags_actions_in_order() {
        let j = serde_yaml::from_str::<Journey>(
            "actors: [alice, bob]\nsteps:\n  - actor: alice\n    fill:\n      key:testid:msg: hi\n  - actor: alice\n    do: tap:key:testid:send\n  - actor: bob\n    expect:\n      text: hi\n",
        )
        .unwrap();
        let (actors, tagged) = build_scenario(Path::new("/nonexistent"), None, &j).unwrap();
        assert_eq!(actors, vec!["alice", "bob"]);
        assert_eq!(
            tagged,
            vec![
                ("alice".into(), "type:key:testid:msg=hi".into()),
                ("alice".into(), "tap:key:testid:send".into()),
                ("bob".into(), "assert:text=hi".into()),
            ]
        );
    }

    #[test]
    fn scenario_rejects_single_actor_only_steps() {
        let goto =
            serde_yaml::from_str::<Journey>("actors: [a]\nsteps:\n  - actor: a\n    goto: home\n")
                .unwrap();
        assert!(build_scenario(Path::new("/nonexistent"), None, &goto).is_err());
        let state = serde_yaml::from_str::<Journey>(
            "actors: [a]\nsteps:\n  - actor: a\n    expect: { state: home }\n",
        )
        .unwrap();
        assert!(build_scenario(Path::new("/nonexistent"), None, &state).is_err());
        let no_actor =
            serde_yaml::from_str::<Journey>("actors: [a]\nsteps:\n  - do: back\n").unwrap();
        assert!(build_scenario(Path::new("/nonexistent"), None, &no_actor).is_err());
    }

    #[test]
    fn scripted_journeys_default_to_sim_tier() {
        let dflt = serde_yaml::from_str::<Journey>("steps:\n  - do: back\n").unwrap();
        assert!(sim_tier(&dflt), "no tier -> sim (E2E by default)");
        let head =
            serde_yaml::from_str::<Journey>("tier: headless\nsteps:\n  - do: back\n").unwrap();
        assert!(!sim_tier(&head), "tier: headless opts out");
        let sim = serde_yaml::from_str::<Journey>("tier: sim\nsteps:\n  - do: back\n").unwrap();
        assert!(sim_tier(&sim));
    }

    #[test]
    fn per_actor_auth_prelude_and_secret_fills() {
        // Map-form actors bind each actor to a session-restore account; a
        // `secret:` fill in a step resolves against that actor's account.
        let j = serde_yaml::from_str::<Journey>(
            "actors:\n  alice: { auth: alice }\n  bob: { auth: bob }\nsteps:\n  - actor: alice\n    fill:\n      key:testid:msg: secret:password\n  - actor: bob\n    expect:\n      text: hi\n",
        )
        .unwrap();
        let (actors, tagged) = build_scenario(Path::new("/nonexistent"), None, &j).unwrap();
        assert_eq!(actors, vec!["alice", "bob"]);
        assert_eq!(
            tagged,
            vec![
                // preludes first, in actor order
                ("alice".into(), "auth:alice".into()),
                ("bob".into(), "auth:bob".into()),
                // then the steps; alice's secret fill binds to her account
                (
                    "alice".into(),
                    "type:key:testid:msg=${REPROIT_SECRET_ALICE_PASSWORD}".into()
                ),
                ("bob".into(), "assert:text=hi".into()),
            ]
        );
    }

    fn parse(yaml: &str) -> Journey {
        serde_yaml::from_str(yaml).unwrap()
    }

    /// A tiny map: entry `s_a` --tap add--> `s_b` --tap go--> `s_c`.
    fn chain_map() -> AppMap {
        let sig = serde_json::json!({
            "screenshot_phash": null, "semantics_hash": null, "route": null
        });
        serde_json::from_value(serde_json::json!({
            "app": "t", "version": 1,
            "states": {
                "s_a": {"description": "start", "signature": sig},
                "s_b": {"description": "mid",   "signature": sig},
                "s_c": {"description": "end",   "signature": sig},
            },
            "transitions": [
                {"from": "s_a", "to": "s_b", "action": {"kind": "tap", "finder": "key:testid:add"}, "reversibility": "proposed_reversible"},
                {"from": "s_b", "to": "s_c", "action": {"kind": "tap", "finder": "key:testid:go"}, "reversibility": "proposed_reversible"},
            ],
            "invariants": []
        }))
        .unwrap()
    }

    #[test]
    fn replay_trace_aligns_states_and_misses() {
        let log =
            "FUZZ:STATE a\nFUZZ:ACT tap:add\nFUZZ:STATE b\nFUZZ:ACT tap:go\nFUZZ:MISS tap:go\n";
        let (initial, steps) = replay_trace(log);
        assert_eq!(initial.as_deref(), Some("a"));
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].state_after.as_deref(), Some("b"));
        assert!(!steps[0].missed);
        assert!(steps[1].missed);
        assert_eq!(steps[1].state_after, None);
    }

    #[test]
    fn edge_path_to_finds_chain() {
        let m = chain_map();
        assert_eq!(edge_path_to(&m, "s_a", "s_a"), Some(vec![]));
        assert_eq!(edge_path_to(&m, "s_a", "s_b"), Some(vec![0]));
        assert_eq!(edge_path_to(&m, "s_a", "s_c"), Some(vec![0, 1]));
        assert_eq!(edge_path_to(&m, "s_c", "s_a"), None);
    }

    #[test]
    fn cover_walk_visits_every_edge_once() {
        let plan = cover_walk(&chain_map()).unwrap();
        assert_eq!(
            plan.actions,
            vec!["tap:key:testid:add", "tap:key:testid:go"]
        );
        assert_eq!(plan.edge_at, vec![0, 1]);
        assert!(plan.unreachable.is_empty());
    }

    #[test]
    fn parses_do_steps_into_actions() {
        let j = parse("journey: smoke\nsteps:\n  - do: tap:key:testid:add\n  - do: back\n");
        let plan = resolve(None, &j, None).unwrap();
        assert_eq!(plan.actions, vec!["tap:key:testid:add", "back"]);
    }

    #[test]
    fn goto_and_expect_state_need_a_map() {
        assert!(resolve(None, &parse("steps:\n  - goto: home\n"), None).is_err());
        // `expect: state` needs a map to resolve the signature...
        assert!(resolve(None, &parse("steps:\n  - expect: { state: home }\n"), None).is_err());
    }

    #[test]
    fn expect_text_and_count_compile_to_asserts() {
        // ...but `text`/`count` assertions are evaluated live by the runner, so
        // they need no map.
        let j = parse("steps:\n  - expect:\n      text: Welcome\n  - expect:\n      count:\n        key:testid:item: 3\n");
        let plan = resolve(None, &j, None).unwrap();
        assert_eq!(
            plan.actions,
            vec!["assert:text=Welcome", "assert:count:key:testid:item=3"]
        );
    }

    #[test]
    fn empty_expect_is_rejected() {
        assert!(resolve(None, &parse("steps:\n  - expect: {}\n"), None).is_err());
    }

    #[test]
    fn parse_setup_reads_kind_and_account() {
        assert_eq!(
            parse_setup("login(guest)").unwrap(),
            (SetupKind::Login, "guest".to_string())
        );
        assert_eq!(
            parse_setup(" auth(admin) ").unwrap(),
            (SetupKind::Auth, "admin".to_string())
        );
        assert!(parse_setup("login()").is_err());
        assert!(parse_setup("nope(x)").is_err());
        assert!(parse_setup("login(guest").is_err());
    }

    #[test]
    fn secret_fill_becomes_env_placeholder() {
        let j = parse("steps:\n  - fill:\n      key:testid:pass: secret:password\n");
        let plan = resolve(None, &j, Some("guest")).unwrap();
        assert_eq!(
            plan.actions,
            vec!["type:key:testid:pass=${REPROIT_SECRET_GUEST_PASSWORD}"]
        );
    }

    #[test]
    fn explicit_account_overrides_setup_for_secret() {
        let v = resolve_fill_value("secret:admin.password", Some("guest")).unwrap();
        assert_eq!(v, "${REPROIT_SECRET_ADMIN_PASSWORD}");
    }

    #[test]
    fn bare_secret_without_account_errors() {
        let j = parse("steps:\n  - fill:\n      f: secret:password\n");
        assert!(resolve(None, &j, None).is_err());
    }

    #[test]
    fn fill_expands_to_type_actions() {
        let j = parse("steps:\n  - fill:\n      key:testid:email: guest@example.com\n      key:testid:pass: \"123456\"\n");
        let plan = resolve(None, &j, None).unwrap();
        // BTreeMap orders fields by finder, so the order is deterministic.
        assert_eq!(
            plan.actions,
            vec![
                "type:key:testid:email=guest@example.com",
                "type:key:testid:pass=123456",
            ]
        );
    }

    #[test]
    fn step_with_two_keys_is_rejected() {
        let j = parse("steps:\n  - do: back\n    goto: home\n");
        assert!(resolve(None, &j, None).is_err());
    }

    #[test]
    fn save_then_list_roundtrips() {
        let dir = std::env::temp_dir().join(format!("reproit-jsave-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let spec = r#"{"setup":"login(guest)","steps":[{"do":"tap:key:testid:add"},{"expect":{"text":"Done"}}]}"#;
        let path = save(&dir, "smoke", spec).unwrap();
        assert!(path.exists());
        let listed = list(&dir).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "smoke");
        assert_eq!(listed[0].steps, 2);
        assert_eq!(listed[0].setup.as_deref(), Some("login(guest)"));
        // The written YAML parses back into a runnable journey.
        let j = load(&dir, "smoke").unwrap();
        assert_eq!(j.steps.len(), 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn save_rejects_bad_specs() {
        let dir = std::env::temp_dir().join(format!("reproit-jbad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(save(&dir, "x", r#"{"steps":[{"do":"back","goto":"home"}]}"#).is_err());
        assert!(save(&dir, "x", r#"{"steps":[{"expect":{}}]}"#).is_err());
        assert!(save(&dir, "x", r#"{"steps":[]}"#).is_err());
        assert!(save(&dir, "../evil", r#"{"steps":[{"do":"back"}]}"#).is_err());
        assert!(save(&dir, "x", "not json").is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn failed_assertion_is_stale() {
        let log = "FUZZ:ACT assert:text=Welcome\nFUZZ:ASSERT fail text=\"Welcome\"\n";
        assert_eq!(classify_run(log, true), repro::RunVerdict::CouldNotReplay);
        let ok = "FUZZ:ACT assert:text=Welcome\nFUZZ:ASSERT pass text=\"Welcome\"\n";
        assert_eq!(classify_run(ok, true), repro::RunVerdict::Green);
    }

    #[test]
    fn classify_clean_is_green() {
        let log = "FUZZ:ACT tap:add\nFUZZ:ACT back\nJOURNEY DONE\n";
        assert_eq!(classify_run(log, true), repro::RunVerdict::Green);
    }

    #[test]
    fn classify_miss_is_stale() {
        let log = "FUZZ:ACT tap:add\nFUZZ:MISS tap:gone\nJOURNEY DONE\n";
        assert_eq!(classify_run(log, true), repro::RunVerdict::CouldNotReplay);
    }

    #[test]
    fn classify_crash_is_broke() {
        let log = "FUZZ:ACT tap:x\nflutter: ══╡ EXCEPTION CAUGHT BY WIDGETS LIBRARY ╞══\nboom\n";
        assert_eq!(classify_run(log, true), repro::RunVerdict::Broke);
    }

    /// A `config::Loaded` rooted at `dir`, with a minimal valid config (no
    /// secrets), for exercising `prefix_actions` without a real project.
    fn loaded_at(dir: &Path) -> config::Loaded {
        let cfg: config::Config = serde_yaml::from_str(
            "app:\n  platform: web-playwright\ndevices:\n  namePrefix: t\njourneys:\n  driver: x\n  doneMarkers: [DONE]\n",
        )
        .unwrap();
        config::Loaded {
            config: cfg,
            root: dir.to_path_buf(),
        }
    }

    #[test]
    fn from_journey_resolves_to_a_replay_prefix() {
        let dir = std::env::temp_dir().join(format!("reproit-jfrom-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("journeys")).unwrap();
        // do + fill + expect:text need no map; they become the prefix the fuzzer
        // replays before branching outward.
        std::fs::write(
            dir.join("journeys").join("checkout.yaml"),
            "steps:\n  - do: tap:label:Buy\n  - fill:\n      key:qty: \"2\"\n  - expect:\n      text: Thanks\n",
        )
        .unwrap();
        // Resolves by NAME, like any journey target.
        let by_name = prefix_actions(&loaded_at(&dir), "checkout").unwrap();
        assert_eq!(
            by_name,
            vec!["tap:label:Buy", "type:key:qty=2", "assert:text=Thanks"]
        );
        // And by direct PATH (e.g. wherever `reproit import` wrote it).
        let by_path = prefix_actions(
            &loaded_at(&dir),
            dir.join("journeys").join("checkout.yaml").to_str().unwrap(),
        )
        .unwrap();
        assert_eq!(by_path, by_name);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn from_journey_rejects_multi_actor() {
        let dir = std::env::temp_dir().join(format!("reproit-jfrom-ma-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("journeys")).unwrap();
        std::fs::write(
            dir.join("journeys").join("chat.yaml"),
            "actors: [alice, bob]\nsteps:\n  - actor: alice\n    do: tap:label:Send\n",
        )
        .unwrap();
        let err = prefix_actions(&loaded_at(&dir), "chat")
            .unwrap_err()
            .to_string();
        assert!(err.contains("multi-actor"), "got: {err}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
