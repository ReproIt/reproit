//! Authentication-oriented CLI input and path resolution helpers.

use super::*;
use crate::auth;
use crate::model::{map, repro};

pub(super) fn auth_prompt(label: &str, _secret: bool) -> Result<String> {
    use std::io::Write;
    print!("  {label}: ");
    std::io::stdout().flush()?;
    #[cfg(unix)]
    if _secret {
        let _ = std::process::Command::new("stty").arg("-echo").status();
    }
    let mut value = String::new();
    std::io::stdin().read_line(&mut value)?;
    #[cfg(unix)]
    if _secret {
        let _ = std::process::Command::new("stty").arg("echo").status();
        println!();
    }
    let value = value.trim().to_string();
    if value.is_empty() {
        anyhow::bail!("{label} cannot be empty");
    }
    Ok(value)
}

/// Resolve the vault path from config (or cwd default when no config is found).
fn resolve_vault_path(config_path: Option<&std::path::Path>) -> Result<PathBuf> {
    if let Ok(l) = config::load(config_path) {
        Ok(l.config
            .auth
            .vault
            .as_ref()
            .map(|path| l.root.join(path))
            .unwrap_or_else(|| layout::secrets_vault_path(&l.root)))
    } else {
        Ok(layout::secrets_vault_path(&std::env::current_dir()?))
    }
}

fn resolve_config_path(config_path: Option<&std::path::Path>) -> Result<PathBuf> {
    if let Some(p) = config_path {
        return Ok(p.to_path_buf());
    }
    let mut dir = std::env::current_dir()?;
    loop {
        let p = dir.join("reproit.yaml");
        if p.exists() {
            return Ok(p);
        }
        if !dir.pop() {
            anyhow::bail!("no reproit.yaml found; pass --config or run `reproit init` first");
        }
    }
}

fn yaml_str(s: impl Into<String>) -> serde_yaml::Value {
    serde_yaml::Value::String(s.into())
}

fn yaml_mapping_mut(v: &mut serde_yaml::Value) -> Result<&mut serde_yaml::Mapping> {
    match v {
        serde_yaml::Value::Mapping(m) => Ok(m),
        _ => anyhow::bail!("reproit.yaml must be a YAML mapping"),
    }
}

fn yaml_child_mapping<'a>(
    parent: &'a mut serde_yaml::Mapping,
    key: &str,
) -> Result<&'a mut serde_yaml::Mapping> {
    let k = yaml_str(key);
    if !parent.contains_key(&k) {
        parent.insert(k.clone(), serde_yaml::Value::Mapping(Default::default()));
    }
    match parent.get_mut(&k) {
        Some(serde_yaml::Value::Mapping(m)) => Ok(m),
        _ => anyhow::bail!("`{key}` in reproit.yaml must be a mapping"),
    }
}

fn yaml_child_sequence<'a>(
    parent: &'a mut serde_yaml::Mapping,
    key: &str,
) -> Result<&'a mut Vec<serde_yaml::Value>> {
    let k = yaml_str(key);
    if !parent.contains_key(&k) {
        parent.insert(k.clone(), serde_yaml::Value::Sequence(Vec::new()));
    }
    match parent.get_mut(&k) {
        Some(serde_yaml::Value::Sequence(s)) => Ok(s),
        _ => anyhow::bail!("`{key}` in reproit.yaml must be a list"),
    }
}

fn account_ref(account: &str, field: &str) -> String {
    format!("{account}.{field}")
}

fn insert_yaml_opt(map: &mut serde_yaml::Mapping, key: &str, value: Option<String>) {
    if let Some(value) = value.filter(|s| !s.trim().is_empty()) {
        map.insert(yaml_str(key), yaml_str(value));
    }
}

fn store_secret_opt(
    vault: &mut auth::Vault,
    account: &str,
    field: &str,
    value: Option<String>,
) -> Option<String> {
    let key = account_ref(account, field);
    if let Some(value) = value.filter(|s| !s.is_empty()) {
        vault.set(&key, &value);
    }
    Some(key)
}

fn update_account_config(
    config_path: &Path,
    account: &str,
    strategy: config::AuthStrategy,
    refs: &AuthRefs,
    user_id: Option<String>,
    validate_text: Option<String>,
) -> Result<()> {
    let raw = std::fs::read_to_string(config_path)
        .with_context(|| format!("reading {}", config_path.display()))?;
    let mut doc: serde_yaml::Value =
        serde_yaml::from_str(&raw).with_context(|| format!("parsing {}", config_path.display()))?;
    let root = yaml_mapping_mut(&mut doc)?;
    let auth = yaml_child_mapping(root, "auth")?;
    let accounts = yaml_child_sequence(auth, "accounts")?;
    accounts.retain(|v| {
        !matches!(
            v,
            serde_yaml::Value::Mapping(m)
                if m.get(yaml_str("name")).and_then(serde_yaml::Value::as_str) == Some(account)
        )
    });

    let mut acct = serde_yaml::Mapping::new();
    acct.insert(yaml_str("name"), yaml_str(account));
    acct.insert(yaml_str("strategy"), yaml_str(strategy.as_str()));
    insert_yaml_opt(&mut acct, "userId", user_id);
    insert_yaml_opt(&mut acct, "usernameRef", refs.username_ref.clone());
    insert_yaml_opt(&mut acct, "emailRef", refs.email_ref.clone());
    insert_yaml_opt(&mut acct, "phoneRef", refs.phone_ref.clone());
    insert_yaml_opt(&mut acct, "passwordRef", refs.password_ref.clone());
    insert_yaml_opt(&mut acct, "totpRef", refs.totp_ref.clone());
    insert_yaml_opt(&mut acct, "otpRef", refs.otp_ref.clone());
    insert_yaml_opt(&mut acct, "storageRef", refs.storage_ref.clone());
    if let Some(text) = validate_text.filter(|s| !s.trim().is_empty()) {
        let mut validate = serde_yaml::Mapping::new();
        validate.insert(yaml_str("text"), yaml_str(text));
        acct.insert(yaml_str("validate"), serde_yaml::Value::Mapping(validate));
    }
    accounts.push(serde_yaml::Value::Mapping(acct));

    std::fs::write(config_path, serde_yaml::to_string(&doc)?)
        .with_context(|| format!("writing {}", config_path.display()))?;
    Ok(())
}

#[derive(Default)]
struct AuthRefs {
    username_ref: Option<String>,
    email_ref: Option<String>,
    phone_ref: Option<String>,
    password_ref: Option<String>,
    totp_ref: Option<String>,
    otp_ref: Option<String>,
    storage_ref: Option<String>,
}

fn default_auth_refs(account: &str, strategy: config::AuthStrategy) -> AuthRefs {
    let mut refs = AuthRefs::default();
    match strategy {
        config::AuthStrategy::Password => {
            refs.username_ref = Some(account_ref(account, "username"));
            refs.password_ref = Some(account_ref(account, "password"));
        }
        config::AuthStrategy::PasswordOtp => {
            refs.username_ref = Some(account_ref(account, "username"));
            refs.password_ref = Some(account_ref(account, "password"));
            refs.totp_ref = Some(account_ref(account, "totp"));
        }
        config::AuthStrategy::PhoneOtp => {
            refs.phone_ref = Some(account_ref(account, "phone"));
            refs.otp_ref = Some(account_ref(account, "otp"));
        }
        config::AuthStrategy::EmailLink => {
            refs.email_ref = Some(account_ref(account, "email"));
        }
        config::AuthStrategy::OauthTest
        | config::AuthStrategy::Session
        | config::AuthStrategy::Api => {
            refs.storage_ref = Some(account_ref(account, "session"));
        }
    }
    refs
}

pub(super) async fn auth_cmd(
    config_path: Option<&std::path::Path>,
    action: AuthAction,
) -> Result<()> {
    let vpath = resolve_vault_path(config_path)?;
    match action {
        AuthAction::Add {
            account,
            strategy,
            email,
            phone,
            username,
            password,
            otp,
            totp_secret,
            session,
            user_id,
            validate_text,
            no_discover,
        } => {
            if account.trim().is_empty() {
                anyhow::bail!("account name cannot be empty");
            }
            let strategy = strategy.config();
            let config_file = resolve_config_path(config_path)?;
            let mut refs = default_auth_refs(&account, strategy);
            let mut vault = auth::Vault::open(&vpath)?;

            if email.is_some() {
                refs.email_ref = store_secret_opt(&mut vault, &account, "email", email);
                if matches!(
                    strategy,
                    config::AuthStrategy::Password | config::AuthStrategy::PasswordOtp
                ) && refs.username_ref == Some(account_ref(&account, "username"))
                {
                    refs.username_ref = Some(account_ref(&account, "email"));
                }
            }
            if phone.is_some() {
                refs.phone_ref = store_secret_opt(&mut vault, &account, "phone", phone);
            }
            if username.is_some() {
                refs.username_ref = store_secret_opt(&mut vault, &account, "username", username);
            }
            if password.is_some() {
                refs.password_ref = store_secret_opt(&mut vault, &account, "password", password);
            }
            if otp.is_some() {
                refs.otp_ref = store_secret_opt(&mut vault, &account, "otp", otp);
            }
            if let Some(secret) = totp_secret {
                let Some(code) = auth::totp_now(&secret) else {
                    anyhow::bail!("not a valid base32 TOTP secret");
                };
                let key = account_ref(&account, "totp");
                vault.set(&key, &secret);
                refs.totp_ref = Some(key);
                println!("  TOTP ok (current code {code})");
            }
            if session.is_some() {
                refs.storage_ref = store_secret_opt(&mut vault, &account, "session", session);
            }

            vault.save()?;
            update_account_config(
                &config_file,
                &account,
                strategy,
                &refs,
                user_id,
                validate_text,
            )?;
            println!(
                "  account {account} ({}) written to {}",
                strategy.as_str(),
                config_file.display()
            );
            println!("  vault: {}", vpath.display());
            println!("  use it in journeys with: setup: login({account})");
            if matches!(
                strategy,
                config::AuthStrategy::Session
                    | config::AuthStrategy::Api
                    | config::AuthStrategy::OauthTest
            ) {
                println!("  session-style setup can use: setup: auth({account})");
            } else if !no_discover {
                discover_and_verify_login(config_path, &account).await?;
            }
        }
        AuthAction::Discover { account } => {
            discover_and_verify_login(config_path, &account).await?;
        }
        AuthAction::Doctor { account } => {
            auth_account_doctor(config_path, &account)?;
        }
        AuthAction::Set { key, value } => {
            let val = match value {
                Some(v) => v,
                None => {
                    use std::io::Read;
                    let mut s = String::new();
                    std::io::stdin().read_to_string(&mut s)?;
                    s.trim_end_matches(['\n', '\r']).to_string()
                }
            };
            if val.is_empty() {
                anyhow::bail!("empty value; pass --value or pipe the secret on stdin");
            }
            let mut v = auth::Vault::open(&vpath)?;
            v.set(&key, &val);
            v.save()?;
            println!("  stored {key} in {}", vpath.display());
        }
        AuthAction::SetTotp { key, secret } => {
            let Some(code) = auth::totp_now(&secret) else {
                anyhow::bail!("not a valid base32 TOTP secret");
            };
            let mut v = auth::Vault::open(&vpath)?;
            v.set(&key, &secret);
            v.save()?;
            println!("  stored TOTP {key} (current code {code})");
        }
        AuthAction::List => {
            let v = auth::Vault::open(&vpath)?;
            let keys: Vec<&String> = v.keys().collect();
            if keys.is_empty() {
                println!("  vault is empty ({})", vpath.display());
            } else {
                for k in keys {
                    println!("  {k}");
                }
            }
        }
        AuthAction::Remove { key } => {
            let mut v = auth::Vault::open(&vpath)?;
            if v.remove(&key) {
                v.save()?;
                println!("  removed {key}");
            } else {
                println!("  no such key: {key}");
            }
        }
        AuthAction::Test { account } => {
            let loaded = config::load(config_path)?;
            let acct = loaded
                .config
                .auth
                .accounts
                .iter()
                .find(|a| a.name == account)
                .ok_or_else(|| anyhow::anyhow!("no account named {account} in reproit.yaml"))?;
            let env = auth::secret_env(&loaded.config.auth, &loaded.root)?;
            let ns = format!(
                "REPROIT_SECRET_{}",
                account
                    .chars()
                    .map(|c| if c.is_ascii_alphanumeric() {
                        c.to_ascii_uppercase()
                    } else {
                        '_'
                    })
                    .collect::<String>()
            );
            println!("account {account}:");
            for (k, val) in &env {
                if !k.starts_with(&ns) {
                    continue;
                }
                if k.ends_with("_PASSWORD") {
                    println!("  {k} = (set, {} chars, hidden)", val.len());
                } else {
                    println!("  {k} = {val}");
                }
            }
            if acct.password_ref.is_some() && !env.iter().any(|(k, _)| k.ends_with("_PASSWORD")) {
                println!("  warn: passwordRef set but key not found in vault");
            }
        }
    }
    Ok(())
}

/// Map the unauthenticated UI, infer an account-specific login journey from
/// semantic fields/transitions, then prove it in a clean run before presenting
/// it as usable. The generated YAML remains reviewable project state; secrets
/// stay as vault placeholders and never enter the file.
pub(super) async fn discover_and_verify_login(
    config_path: Option<&std::path::Path>,
    account: &str,
) -> Result<()> {
    let loaded = config::load(config_path)?;
    let freshness = map::map_freshness(&loaded.root)?;
    if !matches!(&freshness, map::MapFreshness::Current) {
        println!("  updating login structure from the current app...");
        rebuild_app_map(
            &loaded,
            "explore",
            Some(30),
            false,
            None,
            matches!(&freshness, map::MapFreshness::Stale(_)),
        )
        .await?;
    }
    let account_cfg = loaded
        .config
        .auth
        .accounts
        .iter()
        .find(|a| a.name == account)
        .ok_or_else(|| anyhow::anyhow!("unknown auth account `{account}`"))?;
    let strategy = account_cfg
        .strategy
        .ok_or_else(|| anyhow::anyhow!("account `{account}` has no auth strategy"))?;
    let validate_text = account_cfg
        .validate
        .as_ref()
        .and_then(|v| v.text.as_deref());
    let appmap = map::load_map(&loaded.root, &loaded.config)?;
    let spec = journey::discover_login_spec(&appmap, account, strategy, validate_text)?;
    let name = format!("login-{account}");
    let path = journey::save(&loaded.root, &name, &spec)?;
    println!("  generated {}", path.display());
    println!("  verifying login from a clean state...");
    let result = journey::run(&loaded, &name, 1, false).await?;
    if result.outcome != repro::Outcome::Pass {
        anyhow::bail!(
            "discovered login did not verify ({}); generated journey kept for review at {}",
            result.outcome.as_str(),
            path.display()
        );
    }
    println!("  login verified: setup: login({account})");
    Ok(())
}

fn vault_has(vault: &auth::Vault, key: &Option<String>) -> bool {
    key.as_ref().is_some_and(|k| vault.get(k).is_some())
}

fn auth_account_doctor(config_path: Option<&std::path::Path>, account: &str) -> Result<()> {
    let loaded = config::load(config_path)?;
    let vpath = resolve_vault_path(config_path)?;
    let acct = loaded
        .config
        .auth
        .accounts
        .iter()
        .find(|a| a.name == account)
        .ok_or_else(|| anyhow::anyhow!("no account named {account} in reproit.yaml"))?;
    let strategy = acct.strategy.unwrap_or(config::AuthStrategy::Password);
    let vault = auth::Vault::open(&vpath)?;
    let login_journey = loaded.root.join("journeys/login.yaml");

    let mut ok = true;
    let mut check = |name: &str, passed: bool, detail: String| {
        ok &= passed;
        println!(
            "  {:7} {name}: {detail}",
            if passed { "ok" } else { "MISSING" }
        );
    };
    println!("account {account} ({})", strategy.as_str());
    check(
        "vault",
        vpath.exists(),
        if vpath.exists() {
            vpath.display().to_string()
        } else {
            format!("{} does not exist yet", vpath.display())
        },
    );
    match strategy {
        config::AuthStrategy::Password => {
            check(
                "identifier",
                acct.username.is_some()
                    || vault_has(&vault, &acct.username_ref)
                    || vault_has(&vault, &acct.email_ref),
                "usernameRef/emailRef or username".into(),
            );
            check(
                "password",
                vault_has(&vault, &acct.password_ref),
                "passwordRef".into(),
            );
            check(
                "login",
                login_journey.exists(),
                login_journey.display().to_string(),
            );
        }
        config::AuthStrategy::PasswordOtp => {
            check(
                "identifier",
                acct.username.is_some()
                    || vault_has(&vault, &acct.username_ref)
                    || vault_has(&vault, &acct.email_ref),
                "usernameRef/emailRef or username".into(),
            );
            check(
                "password",
                vault_has(&vault, &acct.password_ref),
                "passwordRef".into(),
            );
            let totp_ok = acct
                .totp_ref
                .as_ref()
                .and_then(|k| vault.get(k))
                .and_then(auth::totp_now)
                .is_some();
            check(
                "totp",
                totp_ok || vault_has(&vault, &acct.otp_ref),
                "totpRef or otpRef".into(),
            );
            check(
                "login",
                login_journey.exists(),
                login_journey.display().to_string(),
            );
        }
        config::AuthStrategy::PhoneOtp => {
            check(
                "phone",
                vault_has(&vault, &acct.phone_ref),
                "phoneRef".into(),
            );
            check(
                "otp",
                vault_has(&vault, &acct.otp_ref) || vault_has(&vault, &acct.totp_ref),
                "otpRef, totpRef, or provider adapter".into(),
            );
            check(
                "login",
                login_journey.exists(),
                login_journey.display().to_string(),
            );
        }
        config::AuthStrategy::EmailLink => {
            check(
                "email",
                vault_has(&vault, &acct.email_ref),
                "emailRef".into(),
            );
            check(
                "login",
                login_journey.exists(),
                login_journey.display().to_string(),
            );
        }
        config::AuthStrategy::OauthTest
        | config::AuthStrategy::Session
        | config::AuthStrategy::Api => {
            check(
                "session",
                vault_has(&vault, &acct.storage_ref),
                "storageRef".into(),
            );
        }
    }
    if let Some(validate) = &acct.validate {
        if let Some(text) = &validate.text {
            println!("  ok      validate: text `{text}`");
        } else if let Some(state) = &validate.state {
            println!("  ok      validate: state `{state}`");
        }
    } else {
        println!(
            "  warn    validate: add validate.text or validate.state for clearer auth failures"
        );
    }
    if !ok {
        anyhow::bail!("auth account {account} is not ready");
    }
    Ok(())
}
