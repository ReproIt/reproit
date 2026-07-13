//! Stamp the version from the last RELEASE tag at compile time.
//!
//! `CARGO_PKG_VERSION` alone is stuck at 0.1.0 and cannot tell builds apart, so
//! `reproit --version` could not distinguish an old installed binary from a
//! fresh build. We derive the version from `git describe`, which is the exact
//! release tag on a release build (e.g. "0.1.1") and "<tag>-<n>-g<hash>" for a
//! dev build n commits past the last release, plus "-dirty" for uncommitted
//! changes. So the version bumps only when a new vX.Y.Z tag is cut (one per
//! release), NOT on every commit, and an install/release shows a clean "0.1.1".
//! Falls back to Cargo.toml's version with no git or no tag (a crates.io tarball).

use std::process::Command;

fn main() {
    let described = run(&["git", "describe", "--tags", "--dirty", "--always"])
        .map(|d| d.trim_start_matches('v').to_string());
    let version = match described {
        // A real version contains dots ("0.1.1" / "0.1.1-3-gabc"); a bare hash
        // (no tag reachable) does not, so fall back to the Cargo.toml version.
        Some(v) if v.contains('.') => v,
        _ => std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.1.0".into()),
    };

    println!("cargo:rustc-env=REPROIT_VERSION={version}");
    // Re-stamp when HEAD moves, the index changes, or a tag is cut.
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/index");
    println!("cargo:rerun-if-changed=../../.git/refs/tags");
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
