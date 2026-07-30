//! Route-path joining shared by every family's source reader.
//!
//! A joined path is the reader's output identity: six private copies produced six
//! edge-case behaviours (one emitted "" for a root route), so the two real semantics
//! live here and nowhere else. A reader that needs a third behaviour adds it here with
//! its edge cases pinned below, never as a private copy.

/// Joins two normalized path segments. Both sides are stripped of surrounding slashes
/// and the result is re-rooted, so it is always absolute: `("api/", "/users")` is
/// `"/api/users"`, and both-empty is the root `"/"`, never `""`.
pub(super) fn join_segments(prefix: &str, suffix: &str) -> String {
    let prefix = prefix.trim_matches('/');
    let suffix = suffix.trim_matches('/');
    match (prefix.is_empty(), suffix.is_empty()) {
        (true, true) => "/".to_string(),
        (true, false) => format!("/{suffix}"),
        (false, true) => format!("/{prefix}"),
        (false, false) => format!("/{prefix}/{suffix}"),
    }
}

/// Joins a mount prefix with a suffix that carries its own leading slash (nest/mount
/// syntax, where the suffix's shape is significant). `""` or `"/"` means "the mount
/// itself": the trimmed prefix, or the root `"/"` when there is no prefix.
pub(super) fn join_mount(prefix: &str, suffix: &str) -> String {
    let base = prefix.trim_end_matches('/');
    if suffix.is_empty() || suffix == "/" {
        if base.is_empty() {
            return "/".to_string();
        }
        return base.to_string();
    }
    format!("{base}{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_join_is_always_absolute_and_never_empty() {
        assert_eq!(join_segments("", ""), "/");
        assert_eq!(join_segments("", "users"), "/users");
        assert_eq!(join_segments("/api/", ""), "/api");
        assert_eq!(join_segments("api", "users"), "/api/users");
        assert_eq!(join_segments("/api/", "/users/"), "/api/users");
    }

    #[test]
    fn mount_join_collapses_the_root_suffix_onto_the_mount() {
        assert_eq!(join_mount("", "/"), "/");
        assert_eq!(join_mount("", ""), "/");
        assert_eq!(join_mount("/admin", "/"), "/admin");
        assert_eq!(join_mount("/admin/", ""), "/admin");
        assert_eq!(join_mount("/admin", "/users"), "/admin/users");
        assert_eq!(join_mount("/admin/", "/users/"), "/admin/users/");
    }
}
