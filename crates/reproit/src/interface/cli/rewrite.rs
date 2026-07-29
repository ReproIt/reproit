//! Direct bug-ID command rewriting.

/// Turn a bug id into the command that already owns its execution semantics.
///
/// `reproit` is itself the verb ("reproduce it"), so the public fast path is
/// deliberately `reproit <id>`. Saved aliases and journeys use `reproit @name`,
/// which is unambiguous with command names. Production buckets pull and replay;
/// local findings and named local artifacts use the deterministic check path.
pub(crate) fn expand_direct_reference_arg(
    mut args: Vec<std::ffi::OsString>,
) -> Vec<std::ffi::OsString> {
    let mut index = 1;
    while let Some(arg) = args.get(index).and_then(|arg| arg.to_str()) {
        match arg {
            "--json" | "--quiet" | "--yes" => index += 1,
            "--config" => index += 2,
            _ if arg.starts_with("--config=") => index += 1,
            _ => break,
        }
    }
    let Some(first) = args.get(index).and_then(|arg| arg.to_str()) else {
        return args;
    };
    let direct_alias = first
        .strip_prefix('@')
        .filter(|alias| !alias.is_empty())
        .map(str::to_owned);
    let normalized_reference = direct_alias.clone().unwrap_or_else(|| first.to_string());
    let local_repro =
        first.starts_with("fnd_") || first.starts_with("rep_") || direct_alias.is_some();
    let inspectable = local_repro || first.starts_with("bkt_") || first.starts_with("occ_");
    let direct_modes = [
        ("--proof", "proof"),
        ("--inspect", "inspect"),
        ("--watch", "watch"),
        ("--simplify", "simplify"),
    ]
    .into_iter()
    .filter_map(|(flag, mode)| {
        args.iter()
            .position(|arg| arg == flag)
            .map(|position| (position, mode))
    })
    .collect::<Vec<_>>();
    if direct_modes.len() == 1 {
        let (position, mode) = direct_modes[0];
        let supported = match mode {
            "proof" | "inspect" => inspectable,
            "watch" | "simplify" => local_repro,
            _ => false,
        };
        if supported {
            args.remove(position);
            if mode == "simplify" {
                args[index] = "repro".into();
                args.insert(index + 1, "simplify".into());
                args.insert(index + 2, normalized_reference.into());
            } else {
                args[index] = mode.into();
                args.insert(index + 1, normalized_reference.into());
            }
            return args;
        }
    }
    let command = if first.starts_with("bkt_") {
        Some(("__replay-bucket", None))
    } else if first.starts_with("occ_") {
        Some(("__occurrence", None))
    } else if first.starts_with("cap_") {
        Some(("__capture", None))
    } else if first.starts_with("fnd_") || first.starts_with("rep_") || direct_alias.is_some() {
        Some(("check", Some("--repro-id")))
    } else {
        None
    };
    if let Some((command, internal_arg)) = command {
        if let Some(alias) = direct_alias {
            args[index] = alias.into();
        }
        args.insert(index, command.into());
        if let Some(internal_arg) = internal_arg {
            args.insert(index + 1, internal_arg.into());
        }
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_bug_ids_expand_to_their_existing_execution_paths() {
        let expand = |args: &[&str]| {
            expand_direct_reference_arg(args.iter().map(std::ffi::OsString::from).collect())
                .into_iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            expand(&["reproit", "cap_deadbeef00000000", "--watch"]),
            ["reproit", "__capture", "cap_deadbeef00000000", "--watch"]
        );
        assert_eq!(
            expand(&["reproit", "bkt_deadbeef0001"]),
            ["reproit", "__replay-bucket", "bkt_deadbeef0001"]
        );
        assert_eq!(
            expand(&["reproit", "occ_deadbeef0001"]),
            ["reproit", "__occurrence", "occ_deadbeef0001"]
        );
        assert_eq!(
            expand(&["reproit", "fnd_deadbeef0001"]),
            ["reproit", "check", "--repro-id", "fnd_deadbeef0001"]
        );
        assert_eq!(
            expand(&["reproit", "rep_deadbeef0001"]),
            ["reproit", "check", "--repro-id", "rep_deadbeef0001"]
        );
        assert_eq!(
            expand(&["reproit", "@checkout-crash", "--record-video"]),
            [
                "reproit",
                "check",
                "--repro-id",
                "checkout-crash",
                "--record-video"
            ]
        );
        assert_eq!(
            expand(&["reproit", "--json", "bkt_deadbeef0001"]),
            ["reproit", "--json", "__replay-bucket", "bkt_deadbeef0001"]
        );
        assert_eq!(expand(&["reproit", "scan"]), ["reproit", "scan"]);
        assert_eq!(expand(&["reproit", "@"]), ["reproit", "@"]);
    }

    #[test]
    fn direct_repro_operations_need_no_secondary_command_vocabulary() {
        let expand = |args: &[&str]| {
            expand_direct_reference_arg(args.iter().map(std::ffi::OsString::from).collect())
                .into_iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            expand(&["reproit", "fnd_deadbeef0001", "--proof"]),
            ["reproit", "proof", "fnd_deadbeef0001"]
        );
        assert_eq!(
            expand(&["reproit", "@checkout-crash", "--inspect"]),
            ["reproit", "inspect", "checkout-crash"]
        );
        assert_eq!(
            expand(&["reproit", "rep_deadbeef0001", "--watch"]),
            ["reproit", "watch", "rep_deadbeef0001"]
        );
        assert_eq!(
            expand(&[
                "reproit",
                "rep_deadbeef0001",
                "--simplify",
                "--to",
                "[\"tap:key:add\"]",
            ]),
            [
                "reproit",
                "repro",
                "simplify",
                "rep_deadbeef0001",
                "--to",
                "[\"tap:key:add\"]",
            ]
        );
        assert_eq!(
            expand(&["reproit", "bkt_deadbeef0001", "--inspect"]),
            ["reproit", "inspect", "bkt_deadbeef0001"]
        );
    }

    #[test]
    fn auth_has_one_canonical_account_form() {
        let expand = |args: &[&str]| {
            expand_direct_reference_arg(args.iter().map(std::ffi::OsString::from).collect())
                .into_iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            expand(&["reproit", "auth", "alice"]),
            ["reproit", "auth", "alice"]
        );
        assert_eq!(
            expand(&["reproit", "--json", "auth", "alice", "--discover"]),
            ["reproit", "--json", "auth", "alice", "--discover"]
        );
    }
}
