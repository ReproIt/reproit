use super::*;

/// The hosted-reproduction workflow `cloud setup` writes into the app repo. It
/// mirrors the cloud's `repository_dispatch` contract (event `reproit-repro`,
/// payload `{app, bucket, runId}`) and, unlike the older hand-copied template,
/// exports the key under the name the CLI actually reads (`REPROIT_CLOUD_KEY`,
/// NOT `REPROIT_API_KEY`), so the first hosted reproduction authenticates
/// instead of silently 401ing. Pull requests replay every committed production
/// repro and report the candidate fix commit back to Cloud. Cloud keeps the
/// linked issue open until that exact commit reaches production and remains
/// not_reproduced there.
pub(super) const REPRO_WORKFLOW: &str = r#"# Reproit hosted reproduction
# Runs in YOUR CI, on YOUR checkout.
#
# The Reproit cloud never has your source. When a bucket is reproduced (from the
# dashboard or POST /v1/apps/<app>/buckets/<bucket>/reproduce), the cloud fires a
# repository_dispatch at this repo with {app, bucket, runId}; this workflow
# reproduces the bug against your code and posts the verdict (and recording) back
# with ReproIt's private CI callback.
#
# Pull requests also run every production repro committed under
# .reproit/repros. A not_reproduced replay verifies the candidate fix on the PR commit.
# Cloud closes the linked issue only after that commit is deployed and production
# evidence confirms the bug stays gone.
#
# ReproIt wrote this file, bound this repo on the cloud side, and
# persisted your project key. The one manual step left is adding your sk_live_...
# project key as the REPROIT_CLOUD_KEY repo secret (the setup output prints the
# exact `gh secret set` command). Self-hosters also set a REPROIT_CLOUD_URL
# secret pointing at their deployment.

name: reproit-repro

on:
  pull_request:
  repository_dispatch:
    types: [reproit-repro]
  # Smoke-test the loop by hand with an app + bucket id from `reproit bugs`.
  workflow_dispatch:
    inputs:
      app:
        description: "app id"
        required: true
      bucket:
        description: "bucket id"
        required: true

jobs:
  reproduce:
    if: github.event_name != 'pull_request'
    runs-on: ubuntu-latest
    timeout-minutes: 25
    steps:
      - uses: actions/checkout@v4

      - name: Install reproit
        run: curl -fsSL https://reproit.com/install.sh | sh

      - name: Reproduce the bucket deterministically
        env:
          # The CLI reads REPROIT_CLOUD_KEY; the repo secret holds your sk_live_ key.
          REPROIT_CLOUD_KEY: ${{ secrets.REPROIT_CLOUD_KEY }}
          # Optional: self-hosters point this at their own deployment.
          REPROIT_CLOUD_URL: ${{ secrets.REPROIT_CLOUD_URL }}
        run: |
          APP="${{ github.event.client_payload.app || github.event.inputs.app }}"
          BUCKET="${{ github.event.client_payload.bucket || github.event.inputs.bucket }}"
          RUN_ID="${{ github.event.client_payload.runId }}"
          ~/.local/bin/reproit __cloud-internal __replay-dispatch \
            --app "$APP" \
            --bucket "$BUCKET" \
            --as "$BUCKET" \
            --run \
            ${RUN_ID:+--run-id "$RUN_ID"}

  verify-production-repros:
    if: github.event_name == 'pull_request'
    runs-on: ubuntu-latest
    timeout-minutes: 25
    steps:
      - uses: actions/checkout@v4

      - name: Install reproit
        run: curl -fsSL https://reproit.com/install.sh | sh

      - name: Replay committed production repros
        id: replay
        continue-on-error: true
        run: |
          set +e
          OUTPUT="$(~/.local/bin/reproit check --strict --runs 3 2>&1)"
          CODE=$?
          printf '%s\n' "$OUTPUT"
          if printf '%s\n' "$OUTPUT" | grep -q '^check:'; then
            touch /tmp/reproit-check-complete
          fi
          exit "$CODE"

      - name: Report candidate fix evidence to Cloud
        if: always()
        env:
          REPROIT_CLOUD_KEY: ${{ secrets.REPROIT_CLOUD_KEY }}
          REPROIT_CLOUD_URL: ${{ secrets.REPROIT_CLOUD_URL }}
          REPROIT_APP_ID: "__REPROIT_APP_ID__"
          REPROIT_FIXED_COMMIT: ${{ github.event.pull_request.head.sha }}
        run: |
          python3 - <<'PY'
          import json
          import os
          import pathlib
          import urllib.error
          import urllib.request

          key = os.environ.get("REPROIT_CLOUD_KEY", "").strip()
          if not key:
              print("Cloud reporting skipped: REPROIT_CLOUD_KEY is unavailable")
              raise SystemExit(0)
          if not pathlib.Path("/tmp/reproit-check-complete").is_file():
              raise SystemExit("Cloud reporting refused: replay verification did not complete")

          base = os.environ.get("REPROIT_CLOUD_URL", "").strip() or "https://ingest.reproit.com"
          default_app = os.environ["REPROIT_APP_ID"]
          commit = os.environ["REPROIT_FIXED_COMMIT"]
          reported = 0

          for origin_path in pathlib.Path(".reproit/repros").glob("*/cloud.json"):
              origin = json.loads(origin_path.read_text())
              meta_path = origin_path.with_name("meta.json")
              if not meta_path.is_file():
                  continue
              meta = json.loads(meta_path.read_text())
              result = str(meta.get("last_result", "stale")).lower()
              status = {
                  "pass": "not_reproduced",
                  "fail": "reproduced",
                  "flaky": "flaky",
                  "stale": "stale",
              }.get(result, "stale")
              failures = 0 if status == "not_reproduced" else (3 if status == "reproduced" else 1)
              app = origin.get("appId") or default_app
              bucket = origin["bucketId"]
              body = {
                  "status": status,
                  "runs": 3,
                  "failures": failures,
              }
              if status == "not_reproduced":
                  body["fixedInBuild"] = commit
              req = urllib.request.Request(
                  f"{base.rstrip('/')}/v1/apps/{app}/buckets/{bucket}/replay-results",
                  data=json.dumps(body).encode(),
                  headers={
                      "Authorization": f"Bearer {key}",
                      "Content-Type": "application/json",
                  },
                  method="POST",
              )
              try:
                  with urllib.request.urlopen(req) as response:
                      response.read()
              except urllib.error.HTTPError as error:
                  print(error.read().decode(errors="replace"))
                  raise
              reported += 1
              print(f"reported {bucket}: {status}")

          print(f"reported {reported} production repro(s)")
          PY

      - name: Require every production repro to pass
        if: steps.replay.outcome != 'success'
        run: exit 1
"#;

/// Parse an `owner/repo` slug out of a git remote URL, across the forms git
/// actually emits: `git@host:owner/repo.git`, `https://host/owner/repo(.git)`,
/// `ssh://git@host/owner/repo.git`, with or without a trailing `.git` or `/`.
/// Host-agnostic (the dispatch binding is just `owner/repo`). Pure, so it is
/// unit-tested below.
pub(super) fn parse_git_remote_slug(url: &str) -> Option<String> {
    let u = url.trim();
    let after_host = if let Some(rest) = u
        .strip_prefix("ssh://git@")
        .or_else(|| u.strip_prefix("git@"))
    {
        // rest = host:owner/repo.git  OR  host/owner/repo.git
        rest.split_once([':', '/'])
            .map(|(_, tail)| tail)?
            .to_string()
    } else if let Some((_, rest)) = u.split_once("://") {
        // rest = [user@]host/owner/repo(.git)
        let rest = rest.rsplit('@').next().unwrap_or(rest);
        rest.split_once('/').map(|(_, tail)| tail)?.to_string()
    } else {
        return None;
    };
    let path = after_host.trim_end_matches('/').trim_end_matches(".git");
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    (segs.len() >= 2).then(|| format!("{}/{}", segs[segs.len() - 2], segs[segs.len() - 1]))
}

/// The git repository root of the current directory, where `.github/workflows`
/// must live. `cloud setup` roots itself here (not at a `reproit.yaml`, which
/// may be nested or absent) so the workflow lands at the repo top. None when
/// the cwd is not inside a git repo.
pub fn git_toplevel() -> Option<std::path::PathBuf> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then(|| std::path::PathBuf::from(s))
}

/// Best-effort detect the GitHub `owner/repo` for the repo at `root` from its
/// `origin` remote. None when there is no git repo or no origin.
fn detect_github_repo(root: &Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["remote", "get-url", "origin"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_git_remote_slug(&String::from_utf8_lossy(&out.stdout))
}

/// Print the platform-appropriate one-liner to start the SDK, with the real app
/// id filled in. Keeps the web/JS shape as the concrete example (what most will
/// recognize) and points at the per-SDK README for the exact call and endpoint.
fn print_sdk_hint(platform: Option<&str>, app: &str, publishable_key: &str, endpoint: &str) {
    let sdk = match platform {
        Some("web" | "electron" | "tauri") => "sdk/reproit-web.js",
        Some("react-native") => "sdk/reproit-react-native",
        Some("flutter") => "sdk/reproit_flutter",
        Some("ios" | "macos" | "swift-ios" | "swift-macos") => "sdk/reproit-ios",
        Some("android") => "sdk/reproit-android",
        Some("winui" | "wpf" | "windows") => "sdk/reproit-windows",
        _ => "sdk/",
    };
    println!(
        "     ReproIt.start({{ appId: '{app}', key: '{publishable_key}', endpoint: '{endpoint}' \
         }});"
    );
    println!("     (that is the web shape; see {sdk}/README for your platform's exact call)");
}

/// Internal setup helper: wire an existing Cloud project into this repo in one
/// step. Validates + persists the project key, binds this GitHub repo for
/// `repository_dispatch` on the cloud side (via `PUT
/// /v1/apps/:app/integrations`, reachable with just the project key), writes
/// the reproduction workflow, and prints the remaining manual steps (the repo
/// secret + the SDK start call). Project creation stays a dashboard action, so
/// `--app` names an existing one.
#[allow(clippy::too_many_arguments)]
pub async fn setup(
    root: &Path,
    app: &str,
    cloud: Option<String>,
    key: Option<String>,
    dispatch_token: Option<String>,
    repo_override: Option<String>,
    workflow_path: Option<String>,
    write_workflow: bool,
    platform_hint: Option<String>,
) -> Result<()> {
    let base = cloud
        .clone()
        .or_else(|| std::env::var("REPROIT_CLOUD_URL").ok())
        .unwrap_or_else(|| "https://cloud.reproit.com".to_string());
    let base = base.trim_end_matches('/').to_string();
    let project_key = key
        .clone()
        .or_else(|| std::env::var("REPROIT_CLOUD_KEY").ok())
        .or_else(|| {
            crate::adapters::cloud_profile::load_token(
                    &crate::adapters::cloud_profile::token_path(),
                )
                .map(|(t, _)| t)
        });
    let Some(project_key) = project_key else {
        anyhow::bail!(
            "no project key. Create a project in the dashboard ({base}), copy its sk_live_... \
             key, then re-run with --key <key> or set REPROIT_CLOUD_KEY. Project creation is a \
             dashboard step; setup wires an existing project into this repo."
        );
    };

    println!("ReproIt project setup");
    println!("  cloud:    {base}");
    println!("  app:      {app}");

    // Validate against the app before persisting an unusable key.
    match validate_login(&base, &project_key, Some(app)).await {
        Ok(desc) => println!("  key:      {desc}"),
        Err(e) => anyhow::bail!("key check failed: {e}"),
    }
    let tok_path = crate::adapters::cloud_profile::token_path();
    crate::adapters::cloud_profile::save_token(&tok_path, &project_key, &base)?;
    println!("  login:    persisted to {}", tok_path.display());

    // Bind the repo for repository_dispatch. Token uses the endpoint's
    // keep/replace semantics: present replaces, absent leaves any existing one.
    let repo = repo_override.or_else(|| detect_github_repo(root));
    // If the user already authenticated `gh`, reuse that credential for the
    // repository_dispatch binding. This removes a PAT-copying step while still
    // leaving explicit --dispatch-token as the highest-precedence override.
    let gh = which_gh();
    let dispatch_token = dispatch_token
        .or_else(|| std::env::var("REPROIT_DISPATCH_TOKEN").ok())
        .or_else(|| gh_auth_token(gh));
    match &repo {
        Some(r) => {
            let mut body = serde_json::json!({
                "provider": "github",
                "repo": r,
                "dispatchRepo": r,
            });
            if let Some(t) = &dispatch_token {
                body["dispatchToken"] = serde_json::json!(t);
                // The authenticated GitHub token can also file and maintain the
                // bucket's linked issue. One setup command should configure the
                // whole lifecycle, not dispatch while silently omitting tickets.
                body["token"] = serde_json::json!(t);
            }
            let c = Cloud::new(cloud.clone(), Some(project_key.clone()));
            c.put(&format!("/v1/apps/{app}/integrations"), &body)
                .await
                .with_context(|| format!("binding {r} for hosted reproduction"))?;
            if dispatch_token.is_some() {
                println!("  github:   bound {r} (dispatch and linked issues enabled)");
            } else {
                println!(
                    "  dispatch: bound {r} (no token yet: pass --dispatch-token <PAT> or set \
                     REPROIT_DISPATCH_TOKEN so the cloud can trigger this repo; a fine-grained \
                     PAT with Contents read/write on this repo)"
                );
            }
        }
        None => println!(
            "  dispatch: no GitHub repo detected. Pass --repo owner/name to enable hosted \
             reproduction, or run setup from inside the app's git checkout."
        ),
    }

    // Write the reproduction workflow (never clobber a customized one).
    if write_workflow {
        let wf_rel =
            workflow_path.unwrap_or_else(|| ".github/workflows/reproit-repro.yml".to_string());
        let wf_path = root.join(&wf_rel);
        let workflow = REPRO_WORKFLOW.replace("__REPROIT_APP_ID__", app);
        if wf_path.exists() {
            let existing = std::fs::read_to_string(&wf_path)
                .with_context(|| format!("reading {}", wf_path.display()))?;
            if existing.starts_with("# Reproit hosted reproduction:") {
                std::fs::write(&wf_path, workflow)
                    .with_context(|| format!("updating {}", wf_path.display()))?;
                println!("  workflow: updated {wf_rel}");
            } else {
                println!("  workflow: {wf_rel} is customized, left unchanged");
            }
        } else {
            if let Some(parent) = wf_path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
            std::fs::write(&wf_path, workflow)
                .with_context(|| format!("writing {}", wf_path.display()))?;
            println!("  workflow: wrote {wf_rel}");
        }
    }

    // Rotate and retrieve a browser-safe, project-pinned publishable key. The
    // management sk_live key stays only in the CLI/CI credential stores.
    let c = Cloud::new(Some(base.clone()), Some(project_key.clone()));
    let key_response = c
        .post(
            &format!("/v1/apps/{app}/publishable-key"),
            &serde_json::json!({}),
        )
        .await
        .context("minting the write-only SDK key")?;
    let publishable_key = key_response["publishableKey"]
        .as_str()
        .context("cloud did not return publishableKey")?
        .to_string();

    // Persist the selected project alongside the validated secret. Every common
    // command can now infer it (`reproit bugs`, `reproit pull bkt_...`).
    crate::adapters::cloud_profile::save_cloud_profile(&tok_path, &project_key, &base, Some(app))?;

    // Prove ingest + project routing without opening a fake bug: a synthetic
    // structural edge exercises authentication, tenancy, and storage, then a
    // graph read verifies that the exact edge came back. No error/oracle event is
    // sent, so onboarding can never create a false production alert.
    let verify_from = "reproit-setup-start";
    let verify_to = "reproit-setup-ready";
    let batch = reproit_protocol::EventBatch {
        version: reproit_protocol::VERSION,
        batch_id: format!("setup-{}", chrono::Utc::now().timestamp_millis()),
        app_id: app.to_string(),
        deployment: None,
        frames: vec![reproit_protocol::EventFrame {
            run_id: "setup".into(),
            sequence: 1,
            scope: reproit_protocol::EvidenceScope::Shared,
            event: reproit_protocol::Event::GraphEdge {
                from: verify_from.into(),
                action: "setup:verify".into(),
                to: verify_to.into(),
            },
        }],
        evidence: vec![],
    };
    c.post("/v1/events", &serde_json::to_value(batch)?)
        .await
        .context("sending the setup verification event")?;
    let graph = c
        .get(&format!("/v1/graph/{app}"))
        .await
        .context("reading back the setup verification event")?;
    let graph_text = serde_json::to_string(&graph)?;
    if !graph_text.contains(verify_from) || !graph_text.contains(verify_to) {
        anyhow::bail!(
            "cloud accepted setup telemetry but did not return it from the project graph"
        );
    }
    println!("  verify:   SDK ingest + project graph round-trip passed");

    // Install the CI secret automatically when gh is authenticated. Feed it via
    // stdin so the secret never appears in argv, logs, or shell history.
    if let Some(r) = &repo {
        if gh {
            set_gh_secret(r, "REPROIT_CLOUD_KEY", &project_key)
                .with_context(|| format!("setting REPROIT_CLOUD_KEY on {r}"))?;
            println!("  secret:   installed REPROIT_CLOUD_KEY on {r}");
        }
    }

    // Remaining manual steps.
    println!();
    println!("Next steps");
    match &repo {
        Some(r) => {
            if gh {
                println!("  1. GitHub Actions authentication is configured on {r}.");
            } else {
                println!("  1. Add your project key as the REPROIT_CLOUD_KEY secret on {r}:");
                println!("     add it in the repo's Settings -> Secrets and variables -> Actions.");
            }
        }
        None => println!(
            "  1. Add your sk_live_ project key as a REPROIT_CLOUD_KEY repo secret in the app \
             repo."
        ),
    }
    println!("  2. Start the SDK in your app so crashes report to the cloud:");
    let endpoint = if base == "https://cloud.reproit.com" {
        "https://ingest.reproit.com/v1/events".to_string()
    } else {
        format!("{base}/v1/events")
    };
    print_sdk_hint(platform_hint.as_deref(), app, &publishable_key, &endpoint);
    println!("  3. Ship a crash, then list your production bugs:");
    println!("       reproit bugs");
    Ok(())
}

/// Whether the `gh` CLI is on PATH (used only to print the friendlier secret
/// command when it is available).
fn which_gh() -> bool {
    std::process::Command::new("gh")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn gh_auth_token(gh_available: bool) -> Option<String> {
    if !gh_available {
        return None;
    }
    let out = Command::new("gh").args(["auth", "token"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let token = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!token.is_empty()).then_some(token)
}

fn set_gh_secret(repo: &str, name: &str, value: &str) -> Result<()> {
    let mut child = Command::new("gh")
        .args(["secret", "set", name, "--repo", repo])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .context("starting gh secret set")?;
    child
        .stdin
        .as_mut()
        .context("opening gh stdin")?
        .write_all(value.as_bytes())?;
    let status = child.wait()?;
    if !status.success() {
        anyhow::bail!("gh secret set exited with {status}");
    }
    Ok(())
}
