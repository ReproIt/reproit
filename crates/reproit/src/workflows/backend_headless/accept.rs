use super::*;

/// Per-finding acceptance, with a reason and an optional expiry.
///
/// `check --update-baseline` accepts EVERYTHING present in the run. A team
/// living with one known issue had no way to say so without also silently
/// accepting anything else that happened to be reproducing at that moment,
/// which is the opposite of what a gate is for. This is the lint-suppression
/// shape instead: name the finding, say why, optionally say until when.
///
/// Three properties make it an exception rather than a mute button:
/// an accept names ONE fingerprint, so it can never cover a finding nobody
/// looked at; it always carries a reason; and an expired accept blocks again
/// rather than quietly lapsing into permanent silence.
#[derive(Default, Serialize, Deserialize)]
pub(super) struct Accepted {
    #[serde(default)]
    findings: BTreeMap<String, AcceptEntry>,
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct AcceptEntry {
    pub(super) operation: String,
    pub(super) reason: String,
    /// `YYYY-MM-DD`. After this date the finding blocks again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) expires: Option<String>,
}

fn accepted_path(root: &Path) -> PathBuf {
    layout::reproit_dir(root).join("backend-accepted.json")
}

pub(super) fn load(root: &Path) -> Accepted {
    std::fs::read(accepted_path(root))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save(root: &Path, accepted: &Accepted) -> Result<()> {
    let path = accepted_path(root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_json::to_vec_pretty(accepted)?)?;
    Ok(())
}

/// How an accept applies to a finding seen in this run.
#[derive(Debug, PartialEq)]
pub(super) enum Verdict {
    /// No accept recorded: the finding blocks as normal.
    None,
    /// Accepted and still in date.
    Accepted(String),
    /// Accepted, but the expiry has passed: it blocks again.
    Expired(String),
}

impl Accepted {
    pub(super) fn verdict(&self, fingerprint: &str, today: &str) -> Verdict {
        let Some(entry) = self.findings.get(fingerprint) else {
            return Verdict::None;
        };
        match &entry.expires {
            Some(expires) if expires.as_str() < today => Verdict::Expired(expires.clone()),
            _ => Verdict::Accepted(entry.reason.clone()),
        }
    }

    /// Accepts whose finding did not appear in this run.
    ///
    /// Reported, but NOT a failure, which is the opposite of the dependency
    /// audit allowlist. There, an entry is keyed by advisory and a stale one
    /// could mask a different advisory. Here an accept names one fingerprint and
    /// can only ever silence that finding, so a stale entry is cruft to clean up
    /// rather than a hole to fail on.
    /// Every acceptance, so the store can be inspected rather than guessed at.
    pub(super) fn entries(&self) -> Vec<(&String, &AcceptEntry)> {
        self.findings.iter().collect()
    }

    pub(super) fn stale(&self, seen: &BTreeSet<String>) -> Vec<(&String, &AcceptEntry)> {
        self.findings
            .iter()
            .filter(|(fingerprint, _)| !seen.contains(*fingerprint))
            .collect()
    }
}

pub(super) fn today() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

/// `reproit accept <id...> --reason "..." [--until YYYY-MM-DD]`
pub async fn run(
    ctx: &Ctx,
    ids: &[String],
    reason: &str,
    until: Option<&str>,
    remove: bool,
    list: bool,
) -> Result<ExitCode> {
    if reason.trim().is_empty() && !remove && !list {
        bail!("--reason is required: an accepted finding without a stated reason is a mute button");
    }
    if let Some(until) = until {
        validate_date(until)?;
    }
    let root = std::env::current_dir()?;
    let mut accepted = load(&root);
    if list {
        let entries = accepted.entries();
        if entries.is_empty() {
            ctx.say("no accepted findings".to_string());
        }
        for (fingerprint, entry) in entries {
            ctx.say(format!(
                "{} on {}{}: {}",
                &fingerprint[..fingerprint.len().min(12)],
                entry.operation,
                entry
                    .expires
                    .as_ref()
                    .map(|until| format!(" until {until}"))
                    .unwrap_or_default(),
                entry.reason
            ));
        }
        return Ok(ExitCode::SUCCESS);
    }
    let mut changed = 0usize;
    for id in ids {
        let Some(raw) = repro::raw_finding_id(id) else {
            bail!("{id} is not a finding id");
        };
        let Some(path) = find_artifact(raw)? else {
            bail!("no persisted finding artifact for {id}");
        };
        let document: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
        let fingerprint = document
            .pointer("/finding/fingerprint")
            .and_then(Value::as_str)
            .context("finding artifact has no fingerprint")?
            .to_string();
        let operation = document
            .pointer("/finding/operation")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if remove {
            if accepted.findings.remove(&fingerprint).is_some() {
                ctx.say(format!("{id}: acceptance removed; it blocks again"));
                changed += 1;
            } else {
                ctx.say(format!("{id}: was not accepted"));
            }
            continue;
        }
        accepted.findings.insert(
            fingerprint,
            AcceptEntry {
                operation: operation.clone(),
                reason: reason.to_string(),
                expires: until.map(str::to_string),
            },
        );
        changed += 1;
        match until {
            Some(until) => ctx.say(format!(
                "{id} accepted on {operation} until {until}: {reason}"
            )),
            None => ctx.say(format!("{id} accepted on {operation}: {reason}")),
        }
    }
    if changed > 0 {
        save(&root, &accepted)?;
    }
    // Acceptance is deliberately NOT a baseline update: it records exactly the
    // findings named and leaves every other finding blocking.
    ctx.say("the gate now passes these findings only; nothing else was accepted".to_string());
    Ok(ExitCode::SUCCESS)
}

fn validate_date(value: &str) -> Result<()> {
    let valid = value.len() == 10
        && value.as_bytes()[4] == b'-'
        && value.as_bytes()[7] == b'-'
        && value
            .bytes()
            .enumerate()
            .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit());
    if !valid {
        bail!("--until must be a YYYY-MM-DD date, got {value:?}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(entries: &[(&str, &str, Option<&str>)]) -> Accepted {
        Accepted {
            findings: entries
                .iter()
                .map(|(fingerprint, reason, expires)| {
                    (
                        fingerprint.to_string(),
                        AcceptEntry {
                            operation: "op".into(),
                            reason: reason.to_string(),
                            expires: expires.map(str::to_string),
                        },
                    )
                })
                .collect(),
        }
    }

    #[test]
    fn an_unaccepted_finding_still_blocks() {
        assert_eq!(
            store(&[("fp1", "known", None)]).verdict("fp2", "2026-07-25"),
            Verdict::None,
            "accepting one finding must never cover another"
        );
    }

    #[test]
    fn an_accept_without_an_expiry_holds() {
        assert_eq!(
            store(&[("fp1", "tracked in JIRA-1", None)]).verdict("fp1", "2030-01-01"),
            Verdict::Accepted("tracked in JIRA-1".into())
        );
    }

    #[test]
    fn an_expired_accept_blocks_again() {
        // The point of the expiry: silence lapses loudly, not quietly.
        assert_eq!(
            store(&[("fp1", "until the rewrite", Some("2026-07-24"))]).verdict("fp1", "2026-07-25"),
            Verdict::Expired("2026-07-24".into())
        );
    }

    #[test]
    fn an_accept_is_live_on_its_expiry_date() {
        assert!(matches!(
            store(&[("fp1", "r", Some("2026-07-25"))]).verdict("fp1", "2026-07-25"),
            Verdict::Accepted(_)
        ));
    }

    #[test]
    fn an_accept_whose_finding_stopped_appearing_is_reported_as_stale() {
        let accepted = store(&[("fp1", "r", None), ("fp2", "r", None)]);
        let seen: BTreeSet<String> = ["fp1".to_string()].into_iter().collect();
        let stale = accepted.stale(&seen);
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].0, "fp2");
    }

    #[test]
    fn a_malformed_expiry_is_rejected() {
        assert!(validate_date("2026-07-25").is_ok());
        for bad in ["25-07-2026", "2026/07/25", "soon", "2026-7-5"] {
            assert!(validate_date(bad).is_err(), "{bad} should be rejected");
        }
    }
}
