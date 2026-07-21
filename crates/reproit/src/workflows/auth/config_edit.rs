use super::*;
use std::io::Write;

const MAX_CONFIG_BYTES: usize = 4 * 1024 * 1024;

fn yaml_str(value: impl Into<String>) -> serde_yaml::Value {
    serde_yaml::Value::String(value.into())
}

fn insert_opt(map: &mut serde_yaml::Mapping, key: &str, value: Option<String>) {
    if let Some(value) = value.filter(|value| !value.trim().is_empty()) {
        map.insert(yaml_str(key), yaml_str(value));
    }
}

fn account_mapping(
    account: &str,
    strategy: config::AuthStrategy,
    refs: &AuthRefs,
    user_id: Option<String>,
    validate_text: Option<String>,
) -> serde_yaml::Mapping {
    let mut mapping = serde_yaml::Mapping::new();
    mapping.insert(yaml_str("name"), yaml_str(account));
    mapping.insert(yaml_str("strategy"), yaml_str(strategy.as_str()));
    insert_opt(&mut mapping, "userId", user_id);
    insert_opt(&mut mapping, "usernameRef", refs.username_ref.clone());
    insert_opt(&mut mapping, "emailRef", refs.email_ref.clone());
    insert_opt(&mut mapping, "phoneRef", refs.phone_ref.clone());
    insert_opt(&mut mapping, "passwordRef", refs.password_ref.clone());
    insert_opt(&mut mapping, "totpRef", refs.totp_ref.clone());
    insert_opt(&mut mapping, "otpRef", refs.otp_ref.clone());
    insert_opt(&mut mapping, "storageRef", refs.storage_ref.clone());
    if let Some(text) = validate_text.filter(|value| !value.trim().is_empty()) {
        let mut validate = serde_yaml::Mapping::new();
        validate.insert(yaml_str("text"), yaml_str(text));
        mapping.insert(yaml_str("validate"), serde_yaml::Value::Mapping(validate));
    }
    mapping
}

fn indentation(line: &str) -> Result<usize> {
    let spaces = line.bytes().take_while(|byte| *byte == b' ').count();
    if line.as_bytes().get(spaces) == Some(&b'\t') {
        anyhow::bail!("tab-indented YAML cannot be edited safely");
    }
    Ok(spaces)
}

fn uncommented(line: &str) -> &str {
    line.split('#').next().unwrap_or("").trim_end()
}

fn is_key(line: &str, key: &str) -> bool {
    uncommented(line).trim() == format!("{key}:")
}

fn block_end(lines: &[String], start: usize, parent_indent: usize) -> Result<usize> {
    for (index, line) in lines.iter().enumerate().skip(start + 1) {
        if line.trim().is_empty() {
            continue;
        }
        if indentation(line)? <= parent_indent {
            return Ok(index);
        }
    }
    Ok(lines.len())
}

fn render_account(mapping: &serde_yaml::Mapping, item_indent: usize) -> Result<Vec<String>> {
    let yaml = serde_yaml::to_string(mapping)?;
    let mut rendered = Vec::new();
    for (index, line) in yaml.lines().enumerate() {
        let prefix = if index == 0 { "- " } else { "  " };
        rendered.push(format!("{}{prefix}{line}", " ".repeat(item_indent)));
    }
    Ok(rendered)
}

fn item_name(lines: &[String], item_indent: usize) -> Option<String> {
    let yaml = lines
        .iter()
        .map(|line| line.get(item_indent..).unwrap_or(line))
        .collect::<Vec<_>>()
        .join("\n");
    let values: Vec<serde_yaml::Value> = serde_yaml::from_str(&yaml).ok()?;
    values
        .first()?
        .as_mapping()?
        .get(yaml_str("name"))?
        .as_str()
        .map(str::to_string)
}

fn preserve_comment_lines(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .filter(|line| line.trim_start().starts_with('#'))
        .cloned()
        .collect()
}

fn config_shape(document: &serde_yaml::Value) -> Result<(bool, bool)> {
    let root = document
        .as_mapping()
        .ok_or_else(|| anyhow::anyhow!("reproit.yaml must be a YAML mapping"))?;
    let Some(auth) = root.get(yaml_str("auth")) else {
        return Ok((false, false));
    };
    let auth = auth
        .as_mapping()
        .ok_or_else(|| anyhow::anyhow!("`auth` in reproit.yaml must be a mapping"))?;
    let Some(accounts) = auth.get(yaml_str("accounts")) else {
        return Ok((true, false));
    };
    if !accounts.is_sequence() {
        anyhow::bail!("`auth.accounts` in reproit.yaml must be a list");
    }
    Ok((true, true))
}

fn first_content_indent(lines: &[String]) -> Result<Option<usize>> {
    lines
        .iter()
        .find(|line| {
            let trimmed = line.trim_start();
            !trimmed.is_empty() && !trimmed.starts_with('#')
        })
        .map(|line| indentation(line))
        .transpose()
}

fn update_document(raw: &str, account: &str, mapping: &serde_yaml::Mapping) -> Result<String> {
    if raw.len() > MAX_CONFIG_BYTES {
        anyhow::bail!("reproit.yaml exceeds the 4 MiB safe editing limit");
    }
    let document: serde_yaml::Value = serde_yaml::from_str(raw).context("parsing reproit.yaml")?;
    let (has_auth, has_accounts) = config_shape(&document)?;
    let line_ending = if raw.contains("\r\n") { "\r\n" } else { "\n" };
    let trailing_newline = raw.ends_with('\n');
    let mut lines = raw.lines().map(str::to_string).collect::<Vec<_>>();
    let root_indent = first_content_indent(&lines)?.unwrap_or(0);

    let auth_start = lines
        .iter()
        .position(|line| is_key(line, "auth") && indentation(line).ok() == Some(root_indent));
    let Some(auth_start) = auth_start else {
        if has_auth {
            anyhow::bail!("inline `auth` YAML cannot be edited safely");
        }
        if !lines.last().is_none_or(|line| line.trim().is_empty()) {
            lines.push(String::new());
        }
        lines.push("auth:".into());
        lines.push("  accounts:".into());
        lines.extend(render_account(mapping, 4)?);
        return Ok(join_lines(lines, line_ending, true));
    };
    let auth_indent = indentation(&lines[auth_start])?;
    let auth_end = block_end(&lines, auth_start, auth_indent)?;
    let auth_child_indent = lines[auth_start + 1..auth_end]
        .iter()
        .filter(|line| {
            let trimmed = line.trim_start();
            !trimmed.is_empty() && !trimmed.starts_with('#')
        })
        .map(|line| indentation(line))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .filter(|indent| *indent > auth_indent)
        .min();
    let accounts_start = (auth_start + 1..auth_end).find(|index| {
        is_key(&lines[*index], "accounts")
            && auth_child_indent
                .is_some_and(|indent| indentation(&lines[*index]).ok() == Some(indent))
    });
    let Some(accounts_start) = accounts_start else {
        if has_accounts {
            anyhow::bail!("inline `auth.accounts` YAML cannot be edited safely");
        }
        let child_indent = auth_child_indent.unwrap_or(auth_indent + 2);
        let mut addition = vec![format!("{}accounts:", " ".repeat(child_indent))];
        addition.extend(render_account(mapping, child_indent + 2)?);
        lines.splice(auth_end..auth_end, addition);
        return Ok(join_lines(lines, line_ending, trailing_newline));
    };
    let accounts_indent = indentation(&lines[accounts_start])?;
    let accounts_end = block_end(&lines, accounts_start, accounts_indent)?;
    let item_indent = accounts_indent + 2;
    let starts = (accounts_start + 1..accounts_end)
        .filter(|index| {
            indentation(&lines[*index]).ok() == Some(item_indent)
                && lines[*index].trim_start().starts_with('-')
        })
        .collect::<Vec<_>>();

    for (position, start) in starts.iter().copied().enumerate() {
        let end = starts.get(position + 1).copied().unwrap_or(accounts_end);
        if item_name(&lines[start..end], item_indent).as_deref() != Some(account) {
            continue;
        }
        let mut replacement = preserve_comment_lines(&lines[start..end]);
        replacement.extend(render_account(mapping, item_indent)?);
        lines.splice(start..end, replacement);
        return Ok(join_lines(lines, line_ending, trailing_newline));
    }

    lines.splice(
        accounts_end..accounts_end,
        render_account(mapping, item_indent)?,
    );
    Ok(join_lines(lines, line_ending, trailing_newline))
}

fn join_lines(lines: Vec<String>, line_ending: &str, trailing_newline: bool) -> String {
    let mut output = lines.join(line_ending);
    if trailing_newline && !output.ends_with(line_ending) {
        output.push_str(line_ending);
    }
    output
}

fn atomic_write(path: &Path, contents: &str) -> Result<()> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow::anyhow!("configuration path has no file name"))?;
    let temporary = path.with_file_name(format!(".{file_name}.reproit-{}.tmp", std::process::id()));
    let result = (|| -> Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
        std::fs::set_permissions(&temporary, std::fs::metadata(path)?.permissions())?;
        std::fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

pub(super) fn update_account_config(
    config_path: &Path,
    account: &str,
    strategy: config::AuthStrategy,
    refs: &AuthRefs,
    user_id: Option<String>,
    validate_text: Option<String>,
) -> Result<()> {
    let raw = std::fs::read_to_string(config_path)
        .with_context(|| format!("reading {}", config_path.display()))?;
    let mapping = account_mapping(account, strategy, refs, user_id, validate_text);
    let updated = update_document(&raw, account, &mapping)?;
    let _: serde_yaml::Value = serde_yaml::from_str(&updated)
        .context("account edit would produce invalid reproit.yaml")?;
    atomic_write(config_path, &updated)
        .with_context(|| format!("writing {}", config_path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mapping(name: &str) -> serde_yaml::Mapping {
        account_mapping(
            name,
            config::AuthStrategy::Password,
            &AuthRefs {
                username_ref: Some(format!("{name}.email")),
                password_ref: Some(format!("{name}.password")),
                ..Default::default()
            },
            None,
            Some("Welcome".into()),
        )
    }

    #[test]
    fn adding_an_account_preserves_comments_order_and_quoting() {
        let raw = "# project comment\napp:\n  platform: flutter # keep inline\n  bundleId: 'com.example.app'\n\nauth:\n  # account notes\n  accounts:\n    - name: existing\n      strategy: session\n      storageRef: existing.session\n\nevidence:\n  outDir: evidence\n";
        let updated = update_document(raw, "alice", &mapping("alice")).unwrap();

        assert!(updated.starts_with("# project comment\napp:\n"));
        assert!(updated.contains("platform: flutter # keep inline"));
        assert!(updated.contains("bundleId: 'com.example.app'"));
        assert!(updated.contains("# account notes"));
        assert!(updated.contains("- name: existing"));
        assert!(updated.contains("- name: alice"));
        assert!(updated.ends_with("evidence:\n  outDir: evidence\n"));
    }

    #[test]
    fn updating_one_account_leaves_unrelated_sections_byte_for_byte() {
        let raw = "app:\n  platform: flutter\nauth:\n  accounts:\n    - name: alice\n      strategy: password\n      # keep this account note\n      passwordRef: old\n    - name: bob\n      strategy: session\n      storageRef: bob.session\n# trailing comment\n";
        let updated = update_document(raw, "alice", &mapping("alice")).unwrap();

        assert!(updated.contains("# keep this account note"));
        assert!(updated
            .contains("    - name: bob\n      strategy: session\n      storageRef: bob.session"));
        assert!(updated.ends_with("# trailing comment\n"));
        assert_eq!(updated.matches("- name: alice").count(), 1);
    }

    #[test]
    fn nested_or_inline_auth_keys_are_never_mistaken_for_editable_blocks() {
        let nested = "app:\n  options:\n    auth:\n      accounts: []\n";
        let updated = update_document(nested, "alice", &mapping("alice")).unwrap();
        assert!(updated.contains("    auth:\n      accounts: []"));
        assert!(updated.contains("\nauth:\n  accounts:\n    - name: alice"));

        for inline in ["app: {}\nauth: {}\n", "app: {}\nauth:\n  accounts: []\n"] {
            assert!(update_document(inline, "alice", &mapping("alice")).is_err());
        }
    }
}
