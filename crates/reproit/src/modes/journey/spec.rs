use super::*;

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
    /// Multi-actor: the participating sessions. Either a bare list (no
    /// per-actor auth):
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
    /// Journey-local temporal properties. These use the same structural
    /// contract language as top-level fuzz and scan contracts.
    #[serde(default)]
    pub contracts: Vec<crate::model::contracts::ContractSpec>,
    /// Execution tier override. Scripted journeys default to the SIM tier (real
    /// simulator + real backend + determinism/permission pinning), because they
    /// are E2E by nature: login needs the network, multi-actor needs N sims,
    /// and the in-process headless tier has none of that (and dies on a
    /// multi-sim machine with no `-d`). Set `tier: headless` only for a
    /// pure-widget journey that needs no backend. Multi-actor scenarios are
    /// always sim.
    #[serde(default)]
    pub tier: Option<String>,
}

/// A per-actor auth prelude, parsed from the actor's `login`/`auth` config.
#[derive(Debug, Clone)]
pub(super) struct ActorAuth {
    pub(super) kind: SetupKind,
    pub(super) account: String,
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
    pub(super) fn entries(&self) -> &[(String, Option<ActorAuth>)] {
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
pub(super) enum SetupKind {
    /// Drive the `login` journey for the account, then run our steps.
    Login,
    /// Bypass the login UI: the runner restores a saved session for the
    /// account.
    Auth,
}

/// Parse `login(guest)` / `auth(admin)` into its kind and account handle.
pub(super) fn parse_setup(s: &str) -> Result<(SetupKind, String)> {
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
/// naming so `secret:` placeholders line up with the injected
/// `REPROIT_SECRET_*`.
pub(super) fn env_ident(name: &str) -> String {
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
pub(super) fn resolve_fill_value(value: &str, account: Option<&str>) -> Result<String> {
    let Some(spec) = value.strip_prefix("secret:") else {
        return Ok(value.to_string());
    };
    let (acct, field) = match spec.split_once('.') {
        Some((a, f)) => (a.to_string(), f),
        None => {
            let a = account.ok_or_else(|| {
                anyhow::anyhow!(
                    "`secret:{spec}` needs an account: name it (`secret:<acct>.{spec}`) or add \
                     `setup: login(<acct>)`"
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
    /// value is injected from the auth vault at run time; anything else is
    /// typed literally. See `resolve_fill_value`.
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
