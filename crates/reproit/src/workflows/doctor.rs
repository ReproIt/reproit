//! Environment and installation diagnostics for the doctor command.

use super::{config, exec, platform, Ctx};
use crate::interface::cli::target::command_on_path;
use anyhow::Result;
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Serialize)]
struct DoctorCheck {
    name: String,
    ok: bool,
    required: bool,
    detail: String,
    fix: Option<String>,
}

fn doctor_push(
    checks: &mut Vec<DoctorCheck>,
    name: impl Into<String>,
    ok: bool,
    required: bool,
    detail: impl Into<String>,
    fix: Option<String>,
) {
    checks.push(DoctorCheck {
        name: name.into(),
        ok,
        required,
        detail: detail.into(),
        fix,
    });
}

fn playwright_browser_cache_dirs(runner_dir: &Path) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(explicit) = std::env::var_os("PLAYWRIGHT_BROWSERS_PATH") {
        candidates.push(PathBuf::from(explicit));
    }
    candidates.push(runner_dir.join("node_modules/.cache/ms-playwright"));
    if cfg!(windows) {
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            candidates.push(PathBuf::from(local).join("ms-playwright"));
        }
    } else if cfg!(target_os = "macos") {
        if let Some(home) = std::env::var_os("HOME") {
            candidates.push(PathBuf::from(home).join("Library/Caches/ms-playwright"));
        }
    } else if let Some(cache) = std::env::var_os("XDG_CACHE_HOME") {
        candidates.push(PathBuf::from(cache).join("ms-playwright"));
    } else if let Some(home) = std::env::var_os("HOME") {
        candidates.push(PathBuf::from(home).join(".cache/ms-playwright"));
    }
    candidates
}

fn populated_directory(path: &Path) -> bool {
    path.is_dir() && std::fs::read_dir(path).is_ok_and(|mut entries| entries.next().is_some())
}

fn render_doctor(checks: &[DoctorCheck]) {
    for c in checks {
        let status = if c.ok {
            "ok"
        } else if c.required {
            "MISSING"
        } else {
            "warn"
        };
        println!("  {status:7} {}", c.name);
        if !c.detail.is_empty() {
            println!("          {}", c.detail);
        }
        if !c.ok {
            if let Some(fix) = &c.fix {
                println!("          fix: {fix}");
            }
        }
    }
}

fn doctor_optional_path(
    checks: &mut Vec<DoctorCheck>,
    name: &str,
    root: &std::path::Path,
    value: Option<&str>,
    fix: &str,
) {
    let Some(value) = value.filter(|s| !s.trim().is_empty()) else {
        doctor_push(
            checks,
            name,
            false,
            false,
            "not configured",
            Some(fix.into()),
        );
        return;
    };
    let path = std::path::Path::new(value);
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    let exists = resolved.exists() || command_on_path(value);
    doctor_push(
        checks,
        name,
        exists,
        false,
        resolved.display().to_string(),
        Some(fix.into()),
    );
}

pub(super) async fn doctor(config_path: Option<&std::path::Path>, ctx: &Ctx) -> Result<()> {
    let mut checks = Vec::new();
    let loaded = match config::load(config_path) {
        Ok(loaded) => {
            doctor_push(
                &mut checks,
                "config",
                true,
                true,
                format!("loaded project root {}", loaded.root.display()),
                None,
            );
            Some(loaded)
        }
        Err(e) => {
            doctor_push(
                &mut checks,
                "config",
                false,
                true,
                e.to_string(),
                Some(
                    "run from a project with reproit.yaml, pass --config, or start with `reproit \
                     init`"
                        .into(),
                ),
            );
            None
        }
    };

    let web = loaded
        .as_ref()
        .map(|l| l.config.app.platform == "web")
        .unwrap_or(false);

    if let Some(l) = &loaded {
        let toolchain = crate::runtime::toolchain::collect(&l.root, &l.config).await;
        let platform = platform::resolve(&l.config.app.platform);
        if let Some(p) = platform {
            doctor_push(
                &mut checks,
                "platform",
                true,
                true,
                format!("{} via {}", p.id, p.backend.as_str()),
                None,
            );
            let native_causal = matches!(
                l.config.app.platform.as_str(),
                "web" | "electron" | "flutter" | "swift-ios"
            );
            doctor_push(
                &mut checks,
                "causal replay",
                native_causal,
                false,
                if native_causal {
                    String::from(
                        "automatic HTTP capture + fail-closed replay is wired for this framework",
                    )
                } else {
                    String::from(
                        "UI reproduction works; network-dependent confirmation requires the \
                         ReproIt SDK transport hook",
                    )
                },
                (!native_causal).then(|| {
                    "enable the framework SDK causal transport; otherwise network-dependent \
                     observations remain candidates"
                        .into()
                }),
            );
            if let Some(required) = p.backend.required_os() {
                let host = std::env::consts::OS;
                doctor_push(
                    &mut checks,
                    "host os",
                    host == required,
                    false,
                    format!("host={host}, required={required}"),
                    Some(format!(
                        "run this project on {required} or in a matching VM"
                    )),
                );
            }
        }

        if web {
            let node_ok = exec::which("node").await;
            doctor_push(
                &mut checks,
                "node",
                node_ok,
                true,
                "required for the Playwright web runner",
                Some("install Node.js 18+ (`brew install node` on macOS)".into()),
            );
            match &l.config.app.url {
                Some(url) if url.starts_with("http://") || url.starts_with("https://") => {
                    doctor_push(&mut checks, "app url", true, true, url.clone(), None);
                }
                Some(url) => {
                    doctor_push(
                        &mut checks,
                        "app url",
                        false,
                        true,
                        format!("configured value is `{url}`"),
                        Some("set app.url to an http(s) URL reachable from this machine".into()),
                    );
                }
                None => {
                    doctor_push(
                        &mut checks,
                        "app url",
                        false,
                        true,
                        "app.url is missing",
                        Some(
                            "add app.url to reproit.yaml or run with --url where supported".into(),
                        ),
                    );
                }
            }

            match &l.config.app.web_runner_dir {
                Some(dir) => {
                    let runner_dir = l.root.join(dir);
                    let runner = runner_dir.join("runner.mjs");
                    let node_modules = runner_dir.join("node_modules");
                    let playwright = node_modules.join("playwright");
                    let browser_candidates = playwright_browser_cache_dirs(&runner_dir);
                    let browser = browser_candidates
                        .iter()
                        .find(|path| populated_directory(path));
                    doctor_push(
                        &mut checks,
                        "web runner",
                        runner.exists(),
                        true,
                        runner_dir.display().to_string(),
                        Some(
                            "set app.webRunnerDir to reproit-cli/runners/web or install the \
                             packaged runner"
                                .into(),
                        ),
                    );
                    doctor_push(
                        &mut checks,
                        "playwright package",
                        playwright.exists(),
                        true,
                        node_modules.display().to_string(),
                        Some("run `npm ci` in the web runner directory".into()),
                    );
                    doctor_push(
                        &mut checks,
                        "playwright browser",
                        browser.is_some(),
                        false,
                        browser
                            .unwrap_or(&browser_candidates[0])
                            .display()
                            .to_string(),
                        Some(
                            "run `npx playwright install chromium` in the web runner directory"
                                .into(),
                        ),
                    );
                }
                None => {
                    doctor_push(
                        &mut checks,
                        "web runner",
                        false,
                        false,
                        "app.webRunnerDir is not set; reproit will try its self-provisioned runner",
                        Some(
                            "set REPROIT_WEB_RUNNER_DIR for source checkouts if auto-provisioning \
                             fails"
                                .into(),
                        ),
                    );
                }
            }
        } else if let Some(p) = platform {
            match p.backend {
                platform::Backend::FlutterDrive => {
                    for (bin, why) in [
                        ("xcrun", "simulator control"),
                        ("ffmpeg", "video/evidence tooling"),
                    ] {
                        let found = exec::which(bin).await;
                        doctor_push(
                            &mut checks,
                            bin,
                            found,
                            true,
                            why,
                            Some(format!("install {bin} for Flutter/iOS simulator runs")),
                        );
                    }
                    for (name, required) in [("flutter", true), ("dart", true), ("xcode", false)] {
                        let executable = toolchain.resolved_executables.get(name);
                        let version = toolchain.versions.get(name);
                        let detail = match (executable, version) {
                            (Some(path), Some(version)) => format!("{path} | {version}"),
                            (Some(path), None) => format!("{path} | version query failed"),
                            (None, _) => "not found on PATH".into(),
                        };
                        doctor_push(
                            &mut checks,
                            format!("{name} toolchain"),
                            executable.is_some() && version.is_some(),
                            required,
                            detail,
                            Some(format!(
                                "install or select a consistent {name} toolchain on PATH"
                            )),
                        );
                    }
                    doctor_push(
                        &mut checks,
                        "dependency locks",
                        !toolchain.dependency_locks.is_empty(),
                        false,
                        format!(
                            "{} lockfile fingerprint(s) recorded per run",
                            toolchain.dependency_locks.len()
                        ),
                        Some(
                            "commit the platform dependency lockfiles for reproducible runs".into(),
                        ),
                    );
                    let sims = exec::run("xcrun", &["simctl", "list", "devices", "booted"]).await;
                    doctor_push(
                        &mut checks,
                        "simctl reachable",
                        sims.ok(),
                        true,
                        "can query booted simulators",
                        Some("install Xcode command line tools and accept Xcode licenses".into()),
                    );
                }
                platform::Backend::Appium => {
                    doctor_push(
                        &mut checks,
                        "appium url",
                        l.config.app.appium_url.is_some(),
                        true,
                        l.config
                            .app
                            .appium_url
                            .clone()
                            .unwrap_or_else(|| "missing".into()),
                        Some("set app.appiumUrl, usually http://127.0.0.1:4723".into()),
                    );
                    doctor_push(
                        &mut checks,
                        "appium caps",
                        l.config.app.appium_caps.is_some(),
                        true,
                        "desired capabilities present",
                        Some("set app.appiumCaps for the app or bundle under test".into()),
                    );
                }
                platform::Backend::WebCdp => {
                    let node_ok = exec::which("node").await;
                    doctor_push(
                        &mut checks,
                        "node",
                        node_ok,
                        true,
                        "required for Electron/Tauri/webview runners",
                        Some("install Node.js 18+ (`brew install node` on macOS)".into()),
                    );
                    doctor_optional_path(
                        &mut checks,
                        "executable",
                        &l.root,
                        l.config.app.executable.as_deref(),
                        "set app.executable to the built Electron/Tauri app",
                    );
                    doctor_optional_path(
                        &mut checks,
                        "runner dir",
                        &l.root,
                        l.config.app.runner_dir.as_deref(),
                        "set app.runnerDir to the directory containing reproit runners",
                    );
                }
                platform::Backend::DesktopAx
                | platform::Backend::DesktopUia
                | platform::Backend::DesktopAtspi
                | platform::Backend::Instrumented
                | platform::Backend::Tui => {
                    let target = l.config.app.executable.as_deref().or_else(|| {
                        (!l.config.app.bundle_id.trim().is_empty())
                            .then_some(l.config.app.bundle_id.as_str())
                    });
                    doctor_optional_path(
                        &mut checks,
                        "executable",
                        &l.root,
                        target,
                        "set app.executable (or bundleId on macOS) to the app under test",
                    );
                    doctor_optional_path(
                        &mut checks,
                        "runner dir",
                        &l.root,
                        l.config.app.runner_dir.as_deref(),
                        "set app.runnerDir to the directory containing reproit runners",
                    );
                }
            }
        }
    }

    let persisted =
        crate::adapters::cloud_profile::load_token(&crate::adapters::cloud_profile::token_path());
    let cloud_url = std::env::var("REPROIT_CLOUD_URL")
        .ok()
        .or_else(|| persisted.as_ref().and_then(|(_, u)| u.clone()));
    let cloud_key = std::env::var("REPROIT_CLOUD_KEY")
        .ok()
        .or_else(|| persisted.as_ref().map(|(t, _)| t.clone()));
    doctor_push(
        &mut checks,
        "cloud url",
        cloud_url.is_some(),
        false,
        cloud_url.unwrap_or_else(|| "not configured".into()),
        Some("set REPROIT_CLOUD_URL or run `reproit login --key <sk_live_...>`".into()),
    );
    doctor_push(
        &mut checks,
        "cloud key",
        cloud_key.is_some(),
        false,
        cloud_key
            .as_ref()
            .map(|k| format!("configured ({} chars), not printed", k.len()))
            .unwrap_or_else(|| "not configured".into()),
        Some("set REPROIT_CLOUD_KEY or run `reproit login --key <sk_live_...>`".into()),
    );
    if let Some(l) = &loaded {
        let app_id = std::env::var("REPROIT_CLOUD_APP").ok();
        doctor_push(
            &mut checks,
            "cloud app",
            app_id.is_some(),
            false,
            app_id.unwrap_or_else(|| {
                format!("not set; local app platform is {}", l.config.app.platform)
            }),
            Some("run `reproit login` and select a project".into()),
        );
    }

    match &loaded {
        Some(loaded) => match llm::from_spec(&loaded.config.llm.to_spec()) {
            Ok(b) => match b.check().await {
                Ok(()) => doctor_push(&mut checks, "llm", true, false, b.name(), None),
                Err(e) => doctor_push(
                    &mut checks,
                    "llm",
                    false,
                    false,
                    format!("{}: {e}; runner works, authoring will not", b.name()),
                    Some(
                        "install/configure the selected LLM provider or leave this for \
                         runner-only use"
                            .into(),
                    ),
                ),
            },
            Err(e) => doctor_push(
                &mut checks,
                "llm",
                false,
                false,
                e.to_string(),
                Some(
                    "fix the llm section in reproit.yaml if you use authoring/analyze commands"
                        .into(),
                ),
            ),
        },
        None => doctor_push(
            &mut checks,
            "llm",
            false,
            false,
            "no reproit.yaml found, skipping",
            None,
        ),
    }

    let ok = checks.iter().all(|c| c.ok || !c.required);
    if ctx.json {
        ctx.emit(&serde_json::json!({
            "command": "doctor",
            "ok": ok,
            "checks": checks,
        }));
    } else {
        render_doctor(&checks);
        println!(
            "\n{}",
            if ok {
                "doctor: required checks passed"
            } else {
                "doctor: required checks failed"
            }
        );
    }
    if !ok {
        std::process::exit(1);
    }
    Ok(())
}
