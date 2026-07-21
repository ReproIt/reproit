//! State reset: the ordered steps run before every journey/gate run. This is
//! the customer-facing integration contract (see docs/cli.md). Steps are
//! best-effort unless marked required.
//!
//! Steps may reference account fields by name with `${account.<name>.<field>}`
//! (fields: `userId`, `username`, `name`), so reset clears the exact accounts a
//! scenario uses without hardcoding ids in two places. This keeps reset in sync
//! with `auth.accounts` (one source of truth for each test user's id).

use crate::adapters::config::{Account, ResetStep};
use crate::runtime::process::run_configured_shell;
use anyhow::{bail, Result};
use std::path::Path;

/// Expand `${account.<name>.<field>}` references against the configured
/// accounts. Only non-secret fields are templatable (never passwords/TOTP), so
/// reset bodies stay safe to commit.
fn expand(s: &str, accounts: &[Account]) -> String {
    if !s.contains("${account.") {
        return s.to_string();
    }
    let mut out = s.to_string();
    for acct in accounts {
        let fields = [
            ("userId", acct.user_id.as_deref()),
            ("username", acct.username.as_deref()),
            ("name", Some(acct.name.as_str())),
        ];
        for (field, val) in fields {
            if let Some(v) = val {
                out = out.replace(&format!("${{account.{}.{}}}", acct.name, field), v);
            }
        }
    }
    out
}

pub async fn run_reset(steps: &[ResetStep], accounts: &[Account], root: &Path) -> Result<()> {
    if steps.is_empty() {
        return Ok(());
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()?;
    for step in steps {
        match step {
            ResetStep::Command { run, required } => {
                let run = expand(run, accounts);
                let res = run_configured_shell(&run, root).await;
                if res.ok() {
                    println!("  reset ok    {run}");
                } else if *required {
                    bail!("required reset step failed: {run}\n{}", res.stderr);
                } else {
                    println!("  reset skip  {run}");
                }
            }
            ResetStep::Http {
                method,
                url,
                body,
                required,
            } => {
                let url = expand(url, accounts);
                let m =
                    reqwest::Method::from_bytes(method.as_bytes()).unwrap_or(reqwest::Method::POST);
                let mut req = client.request(m, &url);
                if let Some(b) = body {
                    req = req
                        .header("content-type", "application/json")
                        .body(expand(b, accounts));
                }
                let outcome = req.send().await;
                let ok = matches!(&outcome, Ok(r) if r.status().is_success());
                if ok {
                    println!("  reset ok    {method} {url}");
                } else if *required {
                    bail!("required reset step failed: {method} {url} ({outcome:?})");
                } else {
                    println!("  reset skip  {method} {url}");
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn acct(name: &str, user_id: Option<&str>) -> Account {
        Account {
            name: name.to_string(),
            strategy: None,
            user_id: user_id.map(String::from),
            username: None,
            username_ref: None,
            email_ref: None,
            phone_ref: None,
            password_ref: None,
            totp_ref: None,
            otp_ref: None,
            storage_ref: None,
            validate: None,
        }
    }

    #[test]
    fn expands_account_user_id() {
        let accts = vec![
            acct("alice", Some("dev-aaaa")),
            acct("bob", Some("dev-bbbb")),
        ];
        let body = r#"{"user_ids":["${account.alice.userId}","${account.bob.userId}"]}"#;
        assert_eq!(
            expand(body, &accts),
            r#"{"user_ids":["dev-aaaa","dev-bbbb"]}"#
        );
    }

    #[test]
    fn leaves_plain_strings_untouched() {
        let accts = vec![acct("alice", Some("dev-aaaa"))];
        assert_eq!(
            expand("http://api/dev/flush", &accts),
            "http://api/dev/flush"
        );
    }
}
