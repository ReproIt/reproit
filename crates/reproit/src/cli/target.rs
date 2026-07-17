//! Pure classification of positional URL, alias, and executable targets.

/// Classify the positional fuzz target: a URL (point at a deployed app) vs an
/// alias/node to scope the hunt to. Returns the full URL (scheme prepended if
/// missing) when it looks like one, else None (an alias like "login").
///
/// We don't sniff http-vs-https: a bare host defaults to https (http redirects
/// to it anyway), and localhost/loopback defaults to http (dev servers). A
/// token is a URL if it has a scheme, a dotted host, is loopback, or has a
/// host:port.
pub(crate) fn target_as_url(t: &str) -> Option<String> {
    let t = t.trim();
    if t.is_empty() {
        return None;
    }
    // A URL never contains whitespace, so a target with a space is a command line
    // (e.g. `less sample.txt`), not a bare host that happens to end in a TLD-like
    // token -- this is what lets `scan "lazygit --flag"` reach executable detection
    // instead of being misread as `https://lazygit --flag`.
    if t.chars().any(char::is_whitespace) {
        return None;
    }
    if t.starts_with("http://") || t.starts_with("https://") {
        return Some(t.to_string());
    }
    // The authority is everything before the first '/'; the host is before any ':'.
    let authority = t.split('/').next().unwrap_or(t);
    let host = authority.split(':').next().unwrap_or(authority);
    let is_loopback = host == "localhost" || host == "127.0.0.1" || host == "0.0.0.0";
    let has_port = authority
        .rsplit_once(':')
        .map(|(_, p)| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
        .unwrap_or(false);
    // A dotted host is a real host only if its LAST label is TLD-like (alphabetic,
    // 2+ chars), so `google.com` is a URL but an alias like `checkout.2` (numeric
    // last label) is not -- OR every label is numeric (an IPv4 address). (A bare
    // `host:port` is still treated as a URL; `screen:1` vs `myhost:3000` can't be
    // told apart by shape, so that case is left as-is.)
    let labels: Vec<&str> = host.split('.').collect();
    let dotted_host = labels.len() >= 2 && {
        let last = labels.last().copied().unwrap_or("");
        let tld_like = last.len() >= 2 && last.chars().all(|c| c.is_ascii_alphabetic());
        let ipv4 = labels.len() == 4
            && labels
                .iter()
                .all(|l| !l.is_empty() && l.chars().all(|c| c.is_ascii_digit()));
        tld_like || ipv4
    };
    if is_loopback || dotted_host || has_port {
        let scheme = if is_loopback { "http" } else { "https" };
        return Some(format!("{scheme}://{t}"));
    }
    None
}

/// Does `p` name an existing executable file (unix: with an exec bit)?
fn is_executable_file(p: &std::path::Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p)
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

/// Extensions to try when resolving a bare command name on `PATH`: just the
/// name on unix; the Windows `PATHEXT` set (so `lazygit` finds `lazygit.exe`)
/// plus the bare name (in case it already carries an extension) on Windows.
fn path_executable_extensions() -> Vec<String> {
    #[cfg(windows)]
    {
        let raw = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
        let mut exts: Vec<String> = raw
            .split(';')
            .filter(|s| !s.is_empty())
            .map(|s| s.to_ascii_lowercase())
            .collect();
        exts.push(String::new());
        exts
    }
    #[cfg(not(windows))]
    {
        vec![String::new()]
    }
}

/// Is `prog` (a bare command name) resolvable to an executable on `PATH`?
/// Honors Windows `PATHEXT`, so `htop` matches `htop.exe` there.
pub(crate) fn command_on_path(prog: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    let exts = path_executable_extensions();
    std::env::split_paths(&paths).any(|dir| {
        exts.iter()
            .any(|ext| is_executable_file(&dir.join(format!("{prog}{ext}"))))
    })
}

/// A non-URL target that names a runnable TERMINAL executable: an existing
/// executable file path, or a bare command resolvable on `PATH` (e.g.
/// `lazygit`, `htop`). Returns the command line to run in a PTY (`reproit scan
/// <exe>`), args preserved. `None` for anything that isn't clearly an
/// executable, so a saved alias / journey / map node remains a scoped target.
pub(crate) fn target_as_executable(t: &str) -> Option<String> {
    let t = t.trim();
    if t.is_empty() {
        return None;
    }
    // The first whitespace token is the program; the rest are its args.
    let prog = t.split_whitespace().next()?;
    // A path (a separator -- `/` everywhere, also `\` on Windows): must point at an
    // existing executable file. A bare name: resolve it on PATH.
    let is_path = prog.contains('/') || (cfg!(windows) && prog.contains('\\'));
    let ok = if is_path {
        is_executable_file(std::path::Path::new(prog))
    } else {
        command_on_path(prog)
    };
    ok.then(|| t.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_as_executable_detects_path_and_on_path_commands() {
        #[cfg(windows)]
        const SHELL: &str = "cmd";
        #[cfg(not(windows))]
        const SHELL: &str = "sh";

        assert_eq!(target_as_executable(SHELL).as_deref(), Some(SHELL));
        #[cfg(windows)]
        assert_eq!(
            target_as_executable("cmd /C exit 0").as_deref(),
            Some("cmd /C exit 0")
        );
        #[cfg(not(windows))]
        assert_eq!(
            target_as_executable("sh -c true").as_deref(),
            Some("sh -c true")
        );
        #[cfg(not(windows))]
        if std::path::Path::new("/bin/sh").exists() {
            assert_eq!(target_as_executable("/bin/sh").as_deref(), Some("/bin/sh"));
        }
        assert_eq!(target_as_executable("/no/such/binary-xyzzy"), None);
        assert_eq!(target_as_executable("checkout-flow-screen-xyzzy"), None);
        assert_eq!(target_as_executable("my-saved-alias-qqq"), None);
        assert_eq!(target_as_executable(""), None);
    }

    #[test]
    fn target_as_url_classifies_urls_vs_aliases() {
        assert_eq!(
            target_as_url("https://app.com").as_deref(),
            Some("https://app.com")
        );
        assert_eq!(
            target_as_url("http://x.io/a").as_deref(),
            Some("http://x.io/a")
        );
        assert_eq!(
            target_as_url("google.com").as_deref(),
            Some("https://google.com")
        );
        assert_eq!(
            target_as_url("app.vercel.app/dash").as_deref(),
            Some("https://app.vercel.app/dash")
        );
        assert_eq!(
            target_as_url("localhost:3000").as_deref(),
            Some("http://localhost:3000")
        );
        assert_eq!(
            target_as_url("127.0.0.1:8117/").as_deref(),
            Some("http://127.0.0.1:8117/")
        );
        assert_eq!(
            target_as_url("myhost:3000").as_deref(),
            Some("https://myhost:3000")
        );
        assert_eq!(target_as_url("login"), None);
        assert_eq!(target_as_url("checkout"), None);
        assert_eq!(target_as_url(""), None);
        assert_eq!(target_as_url("checkout.2"), None);
        assert_eq!(target_as_url("step.3"), None);
        assert_eq!(
            target_as_url("10.0.0.5").as_deref(),
            Some("https://10.0.0.5")
        );
    }
}
