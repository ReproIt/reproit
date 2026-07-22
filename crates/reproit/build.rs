//! Stamp an exact release version or an unmistakable source-build version.
//!
//! Release builds create the manifest's `vX.Y.Z` tag in their isolated checkout
//! before compiling, so they report the plain manifest version. Untagged source
//! builds include the current commit and dirty state. This prevents an old
//! installed release and a fresh source build from both reporting the same
//! version while a new release tag is still being prepared.

use std::process::Command;

fn main() {
    let manifest = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into());
    let release_tag = format!("v{manifest}");
    let exact_tags = run(&["git", "tag", "--points-at", "HEAD"]);
    let is_release = exact_tags
        .as_deref()
        .is_some_and(|tags| tags.lines().any(|tag| tag == release_tag));
    let version = if is_release {
        manifest
    } else if let Some(commit) = run(&["git", "rev-parse", "--short=12", "HEAD"]) {
        let dirty = run(&["git", "status", "--porcelain"]).is_some_and(|status| !status.is_empty());
        format!(
            "{}-dev+g{}{}",
            manifest,
            commit,
            if dirty { ".dirty" } else { "" }
        )
    } else {
        manifest
    };

    println!("cargo:rustc-env=REPROIT_VERSION={version}");
    // Re-stamp when HEAD moves, the index changes, or a tag is cut.
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/index");
    println!("cargo:rerun-if-changed=../../.git/refs/tags");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=src");
}

/// Run a command, returning trimmed stdout, or None on failure / empty output.
fn run(args: &[&str]) -> Option<String> {
    let out = Command::new(args[0]).args(&args[1..]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}
