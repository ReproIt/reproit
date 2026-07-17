use super::*;

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

pub(super) fn load(root: &Path, name: &str) -> Result<Journey> {
    let p = journey_path(root, name);
    let raw = std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
    serde_yaml::from_str(&raw).with_context(|| format!("parsing {}", p.display()))
}

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

/// Structural validation independent of any map: every step takes exactly one
/// of `do`/`goto`/`expect`/`fill`, `expect` carries an assertion, and `setup`
/// (if present) is well-formed. Path/finder validity is checked later by
/// `check` against the live app, the stronger signal.
pub(super) fn validate_structure(j: &Journey) -> Result<()> {
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

/// Infer a deterministic login journey from the empirically mapped UI. This is
/// deliberately semantics-first: only stable selectors already captured in the
/// app map are used, and the returned journey must still pass a clean run
/// before callers accept it. Multi-screen password/OTP flows are followed
/// through map transitions, with each secret filled when its semantic field
/// appears.
pub fn discover_login_spec(
    map: &AppMap,
    account: &str,
    strategy: crate::config::AuthStrategy,
    validate_text: Option<&str>,
) -> Result<String> {
    use crate::model::appmap::Action;
    let start = map
        .states
        .iter()
        .find(|(_, s)| s.elements.iter().any(|e| e.input_purpose.is_some()))
        .map(|(id, _)| id.clone())
        .ok_or_else(|| anyhow::anyhow!("no login screen found in the app map"))?;

    let required: Vec<&str> = match strategy {
        crate::config::AuthStrategy::Password => vec!["identifier", "password"],
        crate::config::AuthStrategy::PasswordOtp => vec!["identifier", "password", "totp"],
        crate::config::AuthStrategy::PhoneOtp => vec!["phone", "otp"],
        crate::config::AuthStrategy::EmailLink => vec!["email"],
        _ => anyhow::bail!("session/API authentication does not need a UI login journey"),
    };
    let mut remaining: std::collections::BTreeSet<&str> = required.into_iter().collect();
    let mut steps = Vec::<serde_json::Value>::new();
    let mut state_id = start;
    let mut visited = std::collections::BTreeSet::new();

    for _ in 0..10 {
        if !visited.insert(state_id.clone()) {
            break;
        }
        let Some(state) = map.states.get(&state_id) else {
            break;
        };
        for element in &state.elements {
            let purpose = element.input_purpose.as_deref().unwrap_or("");
            let field = if remaining.contains("phone") && purpose == "phone" {
                Some("phone")
            } else if remaining.contains("password") && purpose == "password" {
                Some("password")
            } else if remaining.contains("totp") && purpose == "otp" {
                Some("totp")
            } else if remaining.contains("otp") && purpose == "otp" {
                Some("otp")
            } else if remaining.contains("email") && purpose == "email" {
                Some("email")
            } else if remaining.contains("identifier") && matches!(purpose, "email" | "username") {
                Some(if purpose == "email" {
                    "email"
                } else {
                    "username"
                })
            } else {
                None
            };
            if let Some(field) = field {
                let key = if field == "email" || field == "username" {
                    "identifier"
                } else {
                    field
                };
                if remaining.remove(key) {
                    steps.push(serde_json::json!({
                        "fill": { element.sel.clone(): format!("secret:{account}.{field}") }
                    }));
                }
            }
        }
        let next = map
            .transitions
            .iter()
            .filter(|t| {
                t.from == state_id && t.to != state_id && matches!(t.action, Action::Tap { .. })
            })
            .find(|t| {
                map.states.get(&t.to).is_some_and(|s| {
                    s.elements.iter().any(|e| {
                        e.input_purpose.as_deref().is_some_and(|p| {
                            remaining.contains(p)
                                || (remaining.contains("identifier")
                                    && matches!(p, "email" | "username"))
                                || (remaining.contains("totp") && p == "otp")
                        })
                    })
                })
            })
            .or_else(|| {
                map.transitions.iter().find(|t| {
                    t.from == state_id && t.to != state_id && matches!(t.action, Action::Tap { .. })
                })
            });
        let Some(next) = next else { break };
        if let Action::Tap { finder } = &next.action {
            steps.push(serde_json::json!({ "do": format!("tap:{finder}") }));
        }
        state_id = next.to.clone();
        if remaining.is_empty() {
            break;
        }
    }
    if !remaining.is_empty() {
        anyhow::bail!(
            "login discovery could not locate required field(s): {}",
            remaining.into_iter().collect::<Vec<_>>().join(", ")
        );
    }
    if let Some(text) = validate_text {
        steps.push(serde_json::json!({ "expect": { "text": text } }));
    }
    Ok(serde_json::json!({ "steps": steps }).to_string())
}
