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
    let command = if first.starts_with("bkt_") {
        Some(("__replay-bucket", None))
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
            expand(&["reproit", "fnd_deadbeef0001"]),
            ["reproit", "check", "--repro-id", "fnd_deadbeef0001"]
        );
        assert_eq!(
            expand(&["reproit", "rep_deadbeef0001"]),
            ["reproit", "check", "--repro-id", "rep_deadbeef0001"]
        );
        assert_eq!(
            expand(&["reproit", "@checkout-crash"]),
            ["reproit", "check", "--repro-id", "checkout-crash"]
        );
        assert_eq!(
            expand(&["reproit", "--json", "bkt_deadbeef0001"]),
            ["reproit", "--json", "__replay-bucket", "bkt_deadbeef0001"]
        );
        assert_eq!(expand(&["reproit", "scan"]), ["reproit", "scan"]);
        assert_eq!(expand(&["reproit", "@"]), ["reproit", "@"]);
    }
}
