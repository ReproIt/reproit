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
    if let Some(project) = super::backend_target::find(config_path)? {
        return doctor_backend(ctx, &project).await;
    }
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

    cloud_checks(
        &mut checks,
        loaded.as_ref().map(|l| l.config.app.platform.as_str()),
    );

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

    finish(ctx, checks)
}

fn cloud_checks(checks: &mut Vec<DoctorCheck>, platform: Option<&str>) {
    let persisted =
        crate::adapters::cloud_profile::load_token(&crate::adapters::cloud_profile::token_path());
    let cloud_url = std::env::var("REPROIT_CLOUD_URL")
        .ok()
        .or_else(|| persisted.as_ref().and_then(|(_, u)| u.clone()));
    let cloud_key = std::env::var("REPROIT_CLOUD_KEY")
        .ok()
        .or_else(|| persisted.as_ref().map(|(t, _)| t.clone()));
    doctor_push(
        checks,
        "cloud url",
        cloud_url.is_some(),
        false,
        cloud_url.unwrap_or_else(|| "not configured".into()),
        Some("set REPROIT_CLOUD_URL or run `reproit login --key <sk_live_...>`".into()),
    );
    doctor_push(
        checks,
        "cloud key",
        cloud_key.is_some(),
        false,
        cloud_key
            .as_ref()
            .map(|k| format!("configured ({} chars), not printed", k.len()))
            .unwrap_or_else(|| "not configured".into()),
        Some("set REPROIT_CLOUD_KEY or run `reproit login --key <sk_live_...>`".into()),
    );
    if let Some(platform) = platform {
        let app_id = std::env::var("REPROIT_CLOUD_APP").ok();
        doctor_push(
            checks,
            "cloud app",
            app_id.is_some(),
            false,
            app_id.unwrap_or_else(|| format!("not set; local app platform is {platform}")),
            Some("run `reproit login` and select a project".into()),
        );
    }
}

fn finish(ctx: &Ctx, checks: Vec<DoctorCheck>) -> Result<()> {
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

/// Backend-project doctor: schema parses (operation count), target resolves
/// (same precedence as scan/fuzz, minus the flag) and answers, and the
/// adapter tier: one read-only traced request decides effect-level vs
/// black-box verdicts, with the one-line adapter mount for the detected
/// framework when the trail is absent.
async fn doctor_backend(ctx: &Ctx, project: &super::backend_target::BackendProject) -> Result<()> {
    use crate::domain::backend;
    let mut checks = Vec::new();
    doctor_push(
        &mut checks,
        "config",
        true,
        true,
        format!("backend project root {}", project.root.display()),
        None,
    );
    let mut document = None;
    match project.schema_path() {
        Ok(path) => match backend::load_service_document(&path) {
            Ok(parsed) => {
                let operations = backend::import_service_schema(&parsed).len();
                doctor_push(
                    &mut checks,
                    "schema",
                    operations > 0,
                    true,
                    format!("{} ({operations} operation(s))", path.display()),
                    Some("the schema parses but declares no executable operations".into()),
                );
                document = Some(parsed);
            }
            Err(e) => doctor_push(
                &mut checks,
                "schema",
                false,
                true,
                format!("{}: {e:#}", path.display()),
                Some(
                    "the schema must parse as OpenAPI, GraphQL introspection, or a protobuf \
                     descriptor"
                        .into(),
                ),
            ),
        },
        Err(e) => doctor_push(
            &mut checks,
            "schema",
            false,
            true,
            e.to_string(),
            Some(
                "point backend.schemas at a schema file, or run `reproit init <schema url>`".into(),
            ),
        ),
    }

    let env = std::env::var("REPROIT_BACKEND_URL").ok();
    let picked =
        super::backend_target::pick_target(None, env.as_deref(), project.config.target.as_deref())
            .map(|(url, source)| (url.to_string(), source))
            .or_else(|| {
                document
                    .as_ref()
                    .and_then(schema_servers_url)
                    .map(|url| (url, "schema servers entry"))
            });
    match picked {
        Some((url, source)) => {
            let valid = super::backend_target::validate_target_url(&url);
            let ok = valid.is_ok();
            doctor_push(
                &mut checks,
                "target",
                ok,
                true,
                format!("{url} (from {source})"),
                valid
                    .err()
                    .map(|e| format!("{e:#}; targets are absolute http(s) URLs")),
            );
            if ok {
                adapter_checks(&mut checks, &url, document.as_ref(), &project.root).await;
            }
        }
        None => doctor_push(
            &mut checks,
            "target",
            false,
            true,
            "no target: the schema has no servers entry",
            Some(
                "pass `--target <url>` to scan/fuzz, set REPROIT_BACKEND_URL, or set \
                 backend.target in reproit.yaml"
                    .into(),
            ),
        ),
    }
    cloud_checks(&mut checks, Some("backend"));
    finish(ctx, checks)
}

/// One bounded read-only GET with the scan-time trace headers: reachability,
/// and the adapter tier from the `x-reproit-events` response header.
async fn adapter_checks(
    checks: &mut Vec<DoctorCheck>,
    base_url: &str,
    document: Option<&serde_json::Value>,
    project_root: &std::path::Path,
) {
    let path = document
        .and_then(parameterless_get_path)
        .unwrap_or_default();
    let url = format!("{}{path}", base_url.trim_end_matches('/'));
    let response = probe_traced(&url).await;
    match response {
        Err(e) => doctor_push(
            checks,
            "target reachable",
            false,
            false,
            format!("GET {url}: {e:#}"),
            Some("start the service, or point --target / REPROIT_BACKEND_URL at it".into()),
        ),
        Ok((status, adapter)) => {
            doctor_push(
                checks,
                "target reachable",
                true,
                false,
                format!("GET {url} -> {status}"),
                None,
            );
            if adapter {
                doctor_push(
                    checks,
                    "adapter",
                    true,
                    false,
                    "adapter detected: effect-level verdicts enabled",
                    None,
                );
            } else {
                let snippet =
                    crate::adapters::project_scaffold::backend_detect::detect_backend_framework(
                        project_root,
                    )
                    .map(|found| format!("{} ({})", found.adapter_snippet, found.name))
                    .unwrap_or_else(|| {
                        "mount the ReproIt backend adapter for your framework (see the sdk/ \
                         READMEs)"
                            .into()
                    });
                doctor_push(
                    checks,
                    "adapter",
                    false,
                    false,
                    "no adapter response: black-box tier (response-level checks only)",
                    Some(snippet),
                );
            }
        }
    }
}

/// Send one GET with `x-reproit-trace` and report (status, adapter present).
/// The body is never read; only the response head matters here.
async fn probe_traced(url: &str) -> Result<(u16, bool)> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::limited(3))
        .build()?;
    let response = client
        .get(url)
        .header("x-reproit-trace", "doctor")
        .header("x-reproit-action", "1")
        .send()
        .await?;
    let adapter = response.headers().contains_key("x-reproit-events");
    Ok((response.status().as_u16(), adapter))
}

/// The schema `servers` fallback (first entry, as written; scan/fuzz resolve
/// variables at run time, doctor only reports the address).
fn schema_servers_url(document: &serde_json::Value) -> Option<String> {
    document
        .pointer("/servers/0/url")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

/// The least intrusive probe: the first OpenAPI GET path with no template
/// parameters, else the service root.
fn parameterless_get_path(document: &serde_json::Value) -> Option<String> {
    document
        .get("paths")
        .and_then(serde_json::Value::as_object)
        .and_then(|paths| {
            paths
                .iter()
                .find(|(path, item)| !path.contains('{') && item.get("get").is_some())
                .map(|(path, _)| path.clone())
        })
}
