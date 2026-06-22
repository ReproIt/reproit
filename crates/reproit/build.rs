//! Stamp the git commit + build date into the version at compile time.
//!
//! `CARGO_PKG_VERSION` alone is stuck at 0.1.0 and cannot tell builds apart, so
//! `reproit --version` could not distinguish an old installed binary from a
//! fresh build. We embed the short git rev (with a `-dirty` marker when the
//! working tree has uncommitted changes) and the build date, so every build
//! self-identifies. The semver in Cargo.toml stays the deliberate release knob.

use std::process::Command;

fn main() {
    let count = run(&["git", "rev-list", "--count", "HEAD"]);
    // None on command failure / empty => not dirty (covers a no-git crates.io install).
    let dirty = run(&["git", "status", "--porcelain"]).is_some();

    // Base semver: major.minor from Cargo.toml + commit-count patch when building
    // from a git checkout, so the patch moves on every commit (0.1.x) with zero
    // manual bumping. With no git (a crates.io install), fall back to the full
    // Cargo.toml version, which the release step set to the right 0.1.<count>.
    // major.minor stays the deliberate knob: bump to 0.2 by hand for a release.
    let pkg = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.1.0".into());
    let base = match &count {
        Some(c) => {
            let mm: String = pkg.split('.').take(2).collect::<Vec<_>>().join(".");
            format!("{mm}.{c}")
        }
        None => pkg,
    };

    // A clean install/build shows just `0.1.64`. Only a local working build
    // (uncommitted edits) gets the rev + -dirty + date so it is obviously a dev
    // build and not the published binary.
    let version = if dirty {
        let hash = run(&["git", "rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "nogit".into());
        let date = run(&["date", "+%Y-%m-%d"]).unwrap_or_default();
        format!("{base} ({hash}-dirty {date})")
    } else {
        base
    };

    println!("cargo:rustc-env=REPROIT_VERSION={version}");
    // Re-stamp when a new commit lands or the index changes.
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/index");
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
