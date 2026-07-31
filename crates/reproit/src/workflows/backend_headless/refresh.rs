//! `reproit keep --refresh <guard>`: re-record a drifted hermetic guard.
//!
//! A guard drifts when the code stops making the calls its capture recorded.
//! The gate quarantines that (it reports, it never blocks), but the only
//! advice it can give is "re-capture", which is a dead end: the production
//! failure that produced the capture cannot be made to happen again on
//! demand.
//!
//! Refresh closes that loop. It boots the guard's OWN stored recipe in RECORD
//! mode against a disposable local environment, fires the guard's recorded
//! inbound trigger, and captures the exchange sequence the CURRENT code
//! makes. The old and new sequences are diffed and printed, and nothing is
//! written until the diff is confirmed. The inbound trigger and the oracle are
//! preserved from the original capture, so a refresh re-records HOW the
//! operation reaches its dependencies, never WHAT was asked of it or what
//! counts as failure. That is the line between re-recording and inventing a
//! new guard.

use super::capture_replay::parse_capture;
use crate::domain::backend::BackendEventKind;
use crate::interface::cli::context::Ctx;
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// One recorded dependency call, reduced to the identity a human reads in a
/// diff: the protocol and the request line. Bodies are deliberately excluded;
/// a body change is not a drift in the call SHAPE, and printing payloads in a
/// terminal diff invites leaking captured data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ExchangeKey {
    protocol: String,
    label: String,
}

impl std::fmt::Display for ExchangeKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{} {}", self.protocol, self.label)
    }
}

/// Read the ordered exchange identities out of a capture payload.
pub(super) fn exchange_keys(events: &[crate::domain::backend::BackendEvent]) -> Vec<ExchangeKey> {
    events
        .iter()
        .filter_map(|event| match &event.event {
            BackendEventKind::Effect {
                exchange: Some(exchange),
                ..
            } => Some(exchange),
            _ => None,
        })
        .map(|exchange| {
            let protocol = exchange
                .get("protocol")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            let request = exchange.get("request");
            // HTTP reads as method + path; a statement reads as its text. Both
            // are the identity the replay matcher keys on, so the diff a human
            // sees is the diff the matcher would have failed on.
            let label = match (
                request
                    .and_then(|value| value.get("method"))
                    .and_then(Value::as_str),
                request
                    .and_then(|value| value.get("url"))
                    .and_then(Value::as_str),
                request
                    .and_then(|value| value.get("text"))
                    .and_then(Value::as_str),
            ) {
                (Some(method), Some(url), _) => format!("{method} {}", path_of(url)),
                (_, _, Some(text)) => text.trim().to_string(),
                _ => "(unlabelled)".to_string(),
            };
            ExchangeKey { protocol, label }
        })
        .collect()
}

/// The path and query of a recorded URL; the origin is a per-run detail (an
/// ephemeral port) and would show as drift on every refresh.
fn path_of(url: &str) -> String {
    match url.parse::<reqwest::Url>() {
        Ok(parsed) => {
            let mut rendered = parsed.path().to_string();
            if let Some(query) = parsed.query() {
                rendered.push('?');
                rendered.push_str(query);
            }
            rendered
        }
        Err(_) => url.to_string(),
    }
}

/// The old-versus-new comparison a human confirms before anything is written.
pub(super) struct ExchangeDiff {
    pub(super) added: Vec<ExchangeKey>,
    pub(super) removed: Vec<ExchangeKey>,
    pub(super) reordered: bool,
}

impl ExchangeDiff {
    pub(super) fn unchanged(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && !self.reordered
    }
}

/// Diff two ordered exchange sequences. Multiset semantics on the content, so
/// a call made twice is not reported as "added" when it was recorded twice;
/// ordering is reported separately, because the replay matcher is ordinal and
/// a reorder alone is enough to diverge.
pub(super) fn diff_exchanges(old: &[ExchangeKey], new: &[ExchangeKey]) -> ExchangeDiff {
    let mut remaining: Vec<&ExchangeKey> = old.iter().collect();
    let mut added = Vec::new();
    for key in new {
        match remaining.iter().position(|candidate| *candidate == key) {
            Some(index) => {
                remaining.remove(index);
            }
            None => added.push(key.clone()),
        }
    }
    let removed: Vec<ExchangeKey> = remaining.into_iter().cloned().collect();
    let reordered = added.is_empty() && removed.is_empty() && old != new;
    ExchangeDiff {
        added,
        removed,
        reordered,
    }
}

/// Locate a guard directory by id or alias, requiring it to be hermetic.
fn locate_guard(reference: &str) -> Result<(PathBuf, PathBuf, PathBuf, String)> {
    let root = std::env::current_dir()?;
    let meta = crate::domain::repro::resolve(&root, reference)
        .with_context(|| format!("no saved repro matches `{reference}`"))?;
    let directory = crate::domain::repro::repro_dir(&root, &meta.id);
    let capture = directory.join("capture.json");
    let recipe = directory.join("hermetic.json");
    if !capture.is_file() || !recipe.is_file() {
        bail!(
            "`{reference}` is not a hermetic capture guard (no capture.json + hermetic.json in \
             {}); only hermetic guards can be refreshed",
            directory.display()
        );
    }
    Ok((directory, capture, recipe, meta.id))
}

/// Read the guard's stored boot recipe. The capture never supplies a command.
fn stored_exec(recipe: &Path) -> Result<String> {
    serde_json::from_slice::<Value>(&std::fs::read(recipe)?)
        .ok()
        .and_then(|value| {
            value
                .get("exec")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .with_context(|| format!("{} has no `exec` command", recipe.display()))
}

/// `reproit keep --refresh <guard>`: re-record, diff, confirm, then rewrite.
pub async fn refresh_capture_guard(ctx: &Ctx, reference: &str) -> Result<ExitCode> {
    let (directory, capture_path, recipe_path, id) = locate_guard(reference)?;
    let exec = stored_exec(&recipe_path)?;
    let old_bytes = std::fs::read(&capture_path)?;
    let old = parse_capture(&old_bytes)?;
    let old_keys = exchange_keys(&old.events);

    ctx.say(format!("Refreshing hermetic guard {id}"));
    ctx.say(format!("  operation: {}", old.operation));
    ctx.say(format!("  oracle:    {}", old.oracle));
    ctx.say(format!("  recording: {exec}"));

    let recorded = super::rerecord::record_current(&old, &exec).await?;
    let new_keys = exchange_keys(&recorded.events);
    let diff = diff_exchanges(&old_keys, &new_keys);

    ctx.emit(&json!({
        "command": "keep --refresh",
        "id": id,
        "operation": old.operation,
        "oracle": old.oracle,
        "exchanges": {
            "before": old_keys.iter().map(ToString::to_string).collect::<Vec<_>>(),
            "after": new_keys.iter().map(ToString::to_string).collect::<Vec<_>>(),
            "added": diff.added.iter().map(ToString::to_string).collect::<Vec<_>>(),
            "removed": diff.removed.iter().map(ToString::to_string).collect::<Vec<_>>(),
            "reordered": diff.reordered,
        },
        "written": false,
    }));

    if diff.unchanged() {
        ctx.say("  no change: the code still makes exactly the recorded calls");
        ctx.say("  nothing rewritten");
        return Ok(ExitCode::SUCCESS);
    }
    ctx.say("\n  exchange diff (old -> new):");
    for key in &diff.removed {
        ctx.say(format!("    - {key}"));
    }
    for key in &diff.added {
        ctx.say(format!("    + {key}"));
    }
    if diff.reordered {
        ctx.say("    ~ same calls, different order (the matcher is ordinal, so this diverges)");
    }

    // Never silently re-baseline. Rewriting a guard's capture changes what
    // future CI runs compare against, so it takes an explicit yes.
    if !ctx.confirmed() {
        ctx.say(
            "\n  NOT rewritten. Review the diff above: if the new calls are the intended \
             behaviour, rerun with --yes to re-record this guard.",
        );
        return Ok(ExitCode::from(3));
    }

    // The inbound trigger and the oracle are the guard's identity and are
    // preserved; only the dependency exchanges are re-recorded.
    let refreshed = super::rerecord::merge_preserving_trigger(&old_bytes, &recorded)?;
    std::fs::write(&capture_path, serde_json::to_vec_pretty(&refreshed)?)?;
    ctx.say(format!(
        "\n  re-recorded {} exchange(s) into {}",
        new_keys.len(),
        capture_path.display()
    ));
    ctx.say("  the inbound trigger and oracle are unchanged; only the calls were refreshed");
    ctx.emit(&json!({
        "command": "keep --refresh",
        "id": id,
        "written": true,
        "directory": directory,
    }));
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(protocol: &str, label: &str) -> ExchangeKey {
        ExchangeKey {
            protocol: protocol.into(),
            label: label.into(),
        }
    }

    #[test]
    fn an_identical_sequence_is_no_drift() {
        let old = vec![key("http", "GET /a"), key("pg", "SELECT 1")];
        let diff = diff_exchanges(&old, &old.clone());
        assert!(
            diff.unchanged(),
            "identical sequences must not report drift"
        );
    }

    #[test]
    fn a_new_call_reports_as_added_and_an_absent_one_as_removed() {
        let old = vec![key("http", "GET /a")];
        let new = vec![key("http", "GET /a"), key("http", "GET /inventory")];
        let diff = diff_exchanges(&old, &new);
        assert_eq!(diff.added, vec![key("http", "GET /inventory")]);
        assert!(diff.removed.is_empty());

        let dropped = diff_exchanges(&new, &old);
        assert_eq!(dropped.removed, vec![key("http", "GET /inventory")]);
        assert!(dropped.added.is_empty());
    }

    /// The matcher is ordinal, so the same calls in a different order still
    /// diverge. A refresh must show that as a change, not as "no drift".
    #[test]
    fn the_same_calls_in_a_different_order_are_reported_as_reordered() {
        let old = vec![key("pg", "SELECT 1"), key("http", "GET /a")];
        let new = vec![key("http", "GET /a"), key("pg", "SELECT 1")];
        let diff = diff_exchanges(&old, &new);
        assert!(diff.added.is_empty() && diff.removed.is_empty());
        assert!(diff.reordered);
        assert!(!diff.unchanged());
    }

    /// A call made twice and recorded twice is not "added" on the second
    /// sighting; multiset semantics keep a repeated call honest.
    #[test]
    fn a_repeated_call_is_matched_by_count_not_by_presence() {
        let old = vec![key("http", "GET /a"), key("http", "GET /a")];
        let new = vec![key("http", "GET /a")];
        let diff = diff_exchanges(&old, &new);
        assert_eq!(diff.removed, vec![key("http", "GET /a")]);
        assert!(diff.added.is_empty());
    }
}
