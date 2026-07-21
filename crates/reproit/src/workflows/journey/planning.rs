use super::*;

pub(super) fn load_map(root: &Path) -> Result<Option<AppMap>> {
    crate::domain::map::load_existing_map(root)
}

/// Does `name` (a state key) or its description match the journey's `target`?
pub(super) fn state_matches(map: &AppMap, name: &str, needle: &str) -> bool {
    name.to_lowercase().contains(needle)
        || map
            .states
            .get(name)
            .map(|state| {
                state
                    .name
                    .as_deref()
                    .is_some_and(|label| label.to_lowercase().contains(needle))
                    || state.description.to_lowercase().contains(needle)
            })
            .unwrap_or(false)
}

/// The target state of the edge from `from` whose replay action equals
/// `action`.
pub(super) fn edge_target(map: &AppMap, from: &str, action: &str) -> Option<String> {
    map.transitions
        .iter()
        .find(|t| t.from == from && action_str(&t.action) == action)
        .map(|t| t.to.clone())
}

/// Shortest action path from `from` to a state matching `target`. Returns the
/// reached state key and the replay actions to get there (empty if `from`
/// already matches). None when no path exists in the map.
pub(super) fn path_from(map: &AppMap, from: &str, target: &str) -> Option<(String, Vec<String>)> {
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
pub(super) struct Plan {
    pub(super) actions: Vec<String>,
}

/// Resolve a state reference (key or description substring) to its signature.
pub(super) fn resolve_state_sig(map: &AppMap, target: &str) -> Option<String> {
    let needle = target.to_lowercase();
    map.states
        .keys()
        .find(|k| state_matches(map, k, &needle))
        .map(|k| k.strip_prefix("s_").unwrap_or(k).to_string())
}

/// Resolve a journey into its replay actions and `expect`-state signatures.
/// Tracks the current state so each `goto` pathfinds from where the previous
/// step left off.
pub(super) fn resolve(map: Option<&AppMap>, j: &Journey, account: Option<&str>) -> Result<Plan> {
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
                        "step {n}: `goto: {target}` needs an app model; run `reproit scan` once \
                         to learn the app"
                    )
                })?;
                let from = current.clone().ok_or_else(|| {
                    anyhow::anyhow!(
                        "step {n}: `goto: {target}` from an unknown state (a prior `do` left a \
                         state not in the map)"
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
                            "step {n}: `expect: state` needs an app model; run `reproit scan` \
                             once to learn the app"
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
/// `login(acct)` prepends the `login` journey's actions; `auth(acct)` prepends
/// a single `auth:<acct>` bypass action the runner restores a session from. The
/// account binds `secret:` fills across both the prelude and the journey
/// itself.
pub(super) fn build_plan(root: &Path, map: Option<&AppMap>, j: &Journey) -> Result<Plan> {
    let setup = match &j.setup {
        Some(s) => Some(parse_setup(s)?),
        None => None,
    };
    let account = setup.as_ref().map(|(_, a)| a.as_str());

    let prelude = match &setup {
        Some((kind, account)) => build_auth_prelude(root, map, *kind, account)?,
        None => Plan::default(),
    };

    let mut main = resolve(map, j, account)?;
    // The prelude runs first, then the journey's own actions.
    let mut actions = prelude.actions;
    actions.append(&mut main.actions);
    Ok(Plan { actions })
}

fn build_auth_prelude(
    root: &Path,
    map: Option<&AppMap>,
    kind: SetupKind,
    account: &str,
) -> Result<Plan> {
    if kind == SetupKind::Auth {
        return Ok(Plan {
            actions: vec![format!("auth:{account}")],
        });
    }

    let account_login = format!("login-{account}");
    let login_name = if exists(root, &account_login) {
        account_login.as_str()
    } else {
        "login"
    };
    if !exists(root, login_name) {
        bail!(
            "`setup: login({account})` needs `{account_login}` or `login`; run `reproit auth \
             discover {account}`"
        );
    }
    let login = load(root, login_name)
        .with_context(|| format!("loading the `login` journey for setup({account})"))?;
    if login.setup.is_some() {
        bail!("the `login` journey must not itself declare `setup` (would recurse)");
    }
    resolve(map, &login, Some(account))
        .with_context(|| format!("resolving the `login` journey for {account}"))
}

/// Build and verify one configured principal before a route-access probe.
pub fn account_setup_actions(loaded: &config::Loaded, account: &str) -> Result<Vec<String>> {
    let configured = loaded
        .config
        .auth
        .accounts
        .iter()
        .find(|candidate| candidate.name == account)
        .ok_or_else(|| anyhow::anyhow!("unknown auth account {account:?}"))?;
    let strategy = configured
        .strategy
        .unwrap_or(config::AuthStrategy::Password);
    let kind = match strategy {
        config::AuthStrategy::OauthTest
        | config::AuthStrategy::Session
        | config::AuthStrategy::Api => SetupKind::Auth,
        _ => SetupKind::Login,
    };
    let map = load_map(&loaded.root)?;
    let mut plan = build_auth_prelude(&loaded.root, map.as_ref(), kind, account)?;
    let validate = configured.validate.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "routeAccess principal {account:?} needs auth.accounts[].validate authority"
        )
    })?;
    let mut assertions = 0usize;
    if let Some(text) = &validate.text {
        plan.actions.push(format!("assert:text={text}"));
        assertions += 1;
    }
    if let Some(state) = &validate.state {
        let map = map.as_ref().ok_or_else(|| {
            anyhow::anyhow!("auth validation by state needs an app map; run `reproit scan` first")
        })?;
        let signature = resolve_state_sig(map, state).ok_or_else(|| {
            anyhow::anyhow!("auth validation state {state:?} is absent from the app map")
        })?;
        plan.actions.push(format!("assert:state={signature}"));
        assertions += 1;
    }
    if let Some(route) = &validate.route {
        plan.actions.push(format!("assert:route={route}"));
        assertions += 1;
    }
    if assertions == 0 {
        bail!(
            "routeAccess principal {account:?} needs validate.text, validate.state, or \
             validate.route"
        );
    }

    let secrets = crate::adapters::credentials::secret_env(&loaded.config.auth, &loaded.root)
        .with_context(|| format!("loading credentials for routeAccess principal {account:?}"))?;
    Ok(plan
        .actions
        .iter()
        .map(|action| crate::adapters::credentials::resolve_placeholders(action, &secrets))
        .collect())
}

/// Load a journey by NAME (`journeys/<name>.yaml`, like any journey target) or
/// by a direct PATH (`./flows/login.yaml`). A value with a slash or a
/// `.yaml`/`.yml` extension that exists on disk is read directly; otherwise it
/// resolves as a journey name. This lets `fuzz --from` point at a freshly
/// `import`ed flow wherever it was written.
pub(super) fn load_target(root: &Path, name_or_path: &str) -> Result<Journey> {
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
/// bound, for use as a `fuzz --from` prefix. The fuzzer replays these actions
/// to land the app in the journey's end state, then branches the seeded walk
/// outward from there: an imported/recorded flow becomes the launchpad for the
/// bugs it never covered. Multi-actor journeys are rejected (no single linear
/// path to branch a walk from).
pub fn prefix_actions(loaded: &config::Loaded, name_or_path: &str) -> Result<Vec<String>> {
    let j = load_target(&loaded.root, name_or_path)?;
    if !j.actors.is_empty() || j.steps.iter().any(|s| s.actor.is_some()) {
        bail!(
            "`fuzz --from` needs a single-actor journey; `{name_or_path}` is multi-actor (there \
             is no single path to branch a walk from)"
        );
    }
    let map = load_map(&loaded.root)?;
    let plan = build_plan(&loaded.root, map.as_ref(), &j)?;
    if plan.actions.is_empty() {
        bail!("journey `{name_or_path}` has no actions to replay");
    }
    let secrets = crate::adapters::credentials::secret_env(&loaded.config.auth, &loaded.root)
        .unwrap_or_default();
    Ok(plan
        .actions
        .iter()
        .map(|a| crate::adapters::credentials::resolve_placeholders(a, &secrets))
        .collect())
}

/// Whether a `fuzz --from` target is a multi-actor checkpoint rather than a
/// single-session replay prefix.
pub fn is_multi_actor_target(loaded: &config::Loaded, name_or_path: &str) -> Result<bool> {
    let j = load_target(&loaded.root, name_or_path)?;
    Ok(!j.actors.is_empty() || j.steps.iter().any(|s| s.actor.is_some()))
}
