//! `reproit screenshots`: drive a journey "tour" across locales and devices,
//! capturing its named SHOOT shots into a fastlane-compatible layout.
//!
//! This is a peer to the visual tour (modes/visual.rs): both reuse the SHOOT
//! capture machinery (drive.rs parses the runner's `SHOOT:<name>` markers and
//! lands `<name>.png` in RunOpts.shots_dir). The difference is fan-out and
//! organization: we run the same tour once per (platform x device x locale) and
//! point each run's shots_dir at <out>/<platform>/<locale>/<device>/, so the
//! result drops straight into `fastlane deliver` / `supply`.
//!
//! Because the state signature is locale-invariant, ONE tour covers every locale
//! with no per-locale selectors. The verification gate (v1) checks that every
//! locale of a given platform/device produced the SAME set of shot names: a tour
//! that silently skipped a screen in one locale (a real navigation-drift failure
//! Maestro/snapshot ship blindly) fails here instead. Per-shot signature
//! assertion (the runner emitting the screen signature at SHOOT time) is the next
//! increment; today drive.rs does not see a per-screen signature.
//!
//! Capture support is per-runner: the Flutter iOS-sim path captures via simctl
//! today; web (Playwright) and Android (Appium/adb) capture are the next pieces.

use anyhow::{anyhow, bail, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::config::Loaded;
use crate::crosscut::{self, RunTarget};
use crate::orchestrator::{self, RunOpts};
use crate::{Ctx, ScopedEnv};

/// Resolved command knobs. Each Option/empty falls back to the `screenshots:`
/// config block; a CLI value overrides it.
pub struct Args {
    pub tour: Option<String>,
    pub out: Option<String>,
    pub locales: Vec<String>,
    pub targets: Vec<RunTarget>,
    pub devices: Vec<String>,
    /// None = use the config default (on); Some(false) = --no-verify.
    pub verify: Option<bool>,
}

/// One captured combo and the shot names it produced.
struct ShotRun {
    platform: String,
    device: String,
    locale: String,
    names: Vec<String>,
}

/// Returns Ok(true) when every combo ran and the verify gate (if on) passed.
pub async fn run(ctx: &Ctx, loaded: &Loaded, args: Args) -> Result<bool> {
    let cfg = &loaded.config;
    let sc = cfg.screenshots.as_ref();

    let tour = args
        .tour
        .or_else(|| sc.map(|s| s.tour.clone()))
        .ok_or_else(|| {
            anyhow!("no tour given: pass `reproit screenshots <tour>` or set screenshots.tour in reproit.yaml")
        })?;
    if !crate::journey::exists(&loaded.root, &tour) {
        bail!(
            "tour journey {:?} not found (expected {})",
            tour,
            crate::journey::journey_path(&loaded.root, &tour).display()
        );
    }

    let out = args
        .out
        .or_else(|| sc.map(|s| s.out.clone()))
        .unwrap_or_else(|| "fastlane/screenshots".to_string());
    let out_root = if Path::new(&out).is_absolute() {
        PathBuf::from(&out)
    } else {
        loaded.root.join(&out)
    };

    // CLI lists override config lists only when non-empty.
    let locales = if args.locales.is_empty() {
        sc.map(|s| s.locales.clone()).unwrap_or_default()
    } else {
        args.locales
    };
    let devices = if args.devices.is_empty() {
        sc.map(|s| s.devices.clone()).unwrap_or_default()
    } else {
        args.devices
    };
    let verify = args
        .verify
        .or_else(|| sc.map(|s| s.verify_signature))
        .unwrap_or(true);

    // None sentinels mean "one run with the app/config default".
    let target_runs: Vec<Option<RunTarget>> = if args.targets.is_empty() {
        vec![None]
    } else {
        args.targets.into_iter().map(Some).collect()
    };
    let device_runs: Vec<Option<String>> = if devices.is_empty() {
        vec![None]
    } else {
        devices.into_iter().map(Some).collect()
    };
    let locale_runs: Vec<Option<String>> = if locales.is_empty() {
        vec![None]
    } else {
        locales.into_iter().map(Some).collect()
    };

    ctx.say(format!(
        "screenshots: tour {tour} -> {} ({} platform x {} device x {} locale runs)",
        out_root.display(),
        target_runs.len(),
        device_runs.len(),
        locale_runs.len()
    ));

    let mut all_ok = true;
    let mut runs: Vec<ShotRun> = Vec::new();

    for tgt in &target_runs {
        let platform = match tgt {
            Some(RunTarget::Platform(t)) => t.as_str().to_string(),
            Some(RunTarget::Engine(e)) => e.clone(),
            None => cfg.app.platform.clone(),
        };
        for dev in &device_runs {
            for loc in &locale_runs {
                let loc_label = loc.as_deref().unwrap_or("default");
                let dev_label = dev.as_deref().unwrap_or("default");
                let shots_dir = out_root.join(&platform).join(loc_label).join(dev_label);

                // Per-run env: platform/engine + device, restored on drop so a
                // combo never leaks REPROIT_* into the next (same as run_targets).
                let mut env = Vec::new();
                match tgt {
                    Some(RunTarget::Engine(e)) => {
                        env.push(("REPROIT_PLATFORM".to_string(), "web".to_string()));
                        env.push(("REPROIT_ENGINE".to_string(), e.clone()));
                    }
                    Some(RunTarget::Platform(t)) => {
                        env.push(("REPROIT_PLATFORM".to_string(), t.as_str().to_string()));
                    }
                    None => {}
                }
                if let Some(d) = dev {
                    env.push(("REPROIT_DEVICE".to_string(), d.clone()));
                }
                let _guard = ScopedEnv::set(env);

                // Locale travels to the runner as a define (REPROIT_LOCALE), the
                // same contract `check`/`fuzz` use across locales.
                let defines: Vec<(String, String)> = match loc {
                    Some(l) => vec![(crosscut::LOCALE_ENV.to_string(), l.clone())],
                    None => Vec::new(),
                };

                ctx.say(format!(
                    "\n=== {platform} / locale {loc_label} / device {dev_label} ==="
                ));
                let outcome = orchestrator::run_journey(
                    cfg,
                    &loaded.root,
                    &tour,
                    &RunOpts {
                        shots_dir: Some(&shots_dir),
                        extra_defines: &defines,
                        ..Default::default()
                    },
                )
                .await?;
                all_ok &= outcome.passed;

                let names = list_shots(&shots_dir);
                if names.is_empty() {
                    ctx.say(format!(
                        "  warn: no shots captured in {}",
                        shots_dir.display()
                    ));
                } else {
                    ctx.say(format!("  {} shot(s): {}", names.len(), names.join(", ")));
                }
                runs.push(ShotRun {
                    platform: platform.clone(),
                    device: dev_label.to_string(),
                    locale: loc_label.to_string(),
                    names,
                });
            }
        }
    }

    if verify {
        all_ok &= verify_locale_consistency(ctx, &runs);
    }

    Ok(all_ok)
}

/// Collect the `.png` file stems in a shots dir, sorted.
fn list_shots(dir: &Path) -> Vec<String> {
    let mut names = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) == Some("png") {
                if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                    names.push(stem.to_string());
                }
            }
        }
    }
    names.sort();
    names
}

/// The v1 verification gate: within each platform/device, every locale must
/// produce the SAME set of shot names. A screen that drifted or was skipped in
/// one locale (so its SHOOT never fired) shows up as a missing name and fails.
/// Returns true when consistent (or when there is nothing to cross-check).
fn verify_locale_consistency(ctx: &Ctx, runs: &[ShotRun]) -> bool {
    let gaps = locale_gaps(runs);
    for (group, locale, missing) in &gaps {
        ctx.say(format!(
            "  verify FAIL [{group}] locale {locale} is missing shot(s): {}",
            missing.join(", ")
        ));
    }
    if gaps.is_empty() {
        ctx.say("\nverify: ok (every locale produced the same shot set)");
        true
    } else {
        ctx.say("\nverify: FAILED (some locales are missing shots; see above)");
        false
    }
}

/// Pure core of the verify gate: for each platform/device group with >= 2
/// locales, return every (group, locale, missing-names) where a locale lacks a
/// shot that another locale of the same group produced. Empty = consistent.
/// Split out from the reporting so it is unit-testable without a Ctx.
fn locale_gaps(runs: &[ShotRun]) -> Vec<(String, String, Vec<String>)> {
    let mut groups: BTreeMap<String, BTreeMap<String, BTreeSet<String>>> = BTreeMap::new();
    for r in runs {
        groups
            .entry(format!("{}/{}", r.platform, r.device))
            .or_default()
            .insert(r.locale.clone(), r.names.iter().cloned().collect());
    }
    let mut gaps = Vec::new();
    for (group, by_locale) in &groups {
        if by_locale.len() < 2 {
            continue; // single locale: nothing to cross-check
        }
        let union: BTreeSet<String> = by_locale.values().flatten().cloned().collect();
        for (locale, names) in by_locale {
            let missing: Vec<String> = union.difference(names).cloned().collect();
            if !missing.is_empty() {
                gaps.push((group.clone(), locale.clone(), missing));
            }
        }
    }
    gaps
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(platform: &str, device: &str, locale: &str, names: &[&str]) -> ShotRun {
        ShotRun {
            platform: platform.to_string(),
            device: device.to_string(),
            locale: locale.to_string(),
            names: names.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn consistent_locales_have_no_gaps() {
        let runs = vec![
            run("ios", "iphone", "en", &["home", "detail"]),
            run("ios", "iphone", "de", &["home", "detail"]),
        ];
        assert!(locale_gaps(&runs).is_empty());
    }

    #[test]
    fn a_locale_missing_a_shot_is_a_gap() {
        let runs = vec![
            run("ios", "iphone", "en", &["home", "detail"]),
            run("ios", "iphone", "de", &["home"]), // detail never fired in de
        ];
        let gaps = locale_gaps(&runs);
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].0, "ios/iphone");
        assert_eq!(gaps[0].1, "de");
        assert_eq!(gaps[0].2, vec!["detail".to_string()]);
    }

    #[test]
    fn a_single_locale_is_not_cross_checked() {
        let runs = vec![run("ios", "iphone", "default", &["home"])];
        assert!(locale_gaps(&runs).is_empty());
    }

    #[test]
    fn groups_are_independent() {
        // android missing a shot must not be masked by a complete ios group.
        let runs = vec![
            run("ios", "iphone", "en", &["home"]),
            run("ios", "iphone", "de", &["home"]),
            run("android", "pixel", "en", &["home", "settings"]),
            run("android", "pixel", "de", &["home"]),
        ];
        let gaps = locale_gaps(&runs);
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].0, "android/pixel");
        assert_eq!(gaps[0].2, vec!["settings".to_string()]);
    }
}
