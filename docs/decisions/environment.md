# Environment variables

Every `REPROIT_*` name found in the Rust sources, the JS runners, and the SDKs, with the file
that defines or documents it. The environment is the contract between the orchestrator and the
runner processes, so a variable that exists only in a runner's source is still part of the
product surface. Generated from a source sweep; when a row says "undocumented", the variable
works but its behavior is stated nowhere else, and the referenced file is the authority.

Names ending in `__` are not environment variables; they are template literals the CLI
substitutes into generated files (see the last section).

## CLI, cloud, and auth

| Variable | Reference | Meaning |
| --- | --- | --- |
| `REPROIT_CLOUD_KEY` | `crates/reproit/src/interface/cli/args/actions.rs` | Cloud/project key (`sk_live_...`) used when `auth` is not passed a key. |
| `REPROIT_CLOUD_URL` | `crates/reproit/src/interface/cli/args/actions.rs` | Cloud base URL override for auth/validation. |
| `REPROIT_CLOUD_APP` | `crates/reproit/src/workflows/cloud.rs` | Default cloud app id when the command omits one. |
| `REPROIT_API_KEY` | `crates/reproit/src/workflows/triage/setup.rs` | Legacy name; the CLI reads `REPROIT_CLOUD_KEY`, not this. Kept in docs so the mistake is findable. |
| `REPROIT_DISPATCH_TOKEN` | `crates/reproit/src/interface/cli/args/actions.rs` | GitHub fine-grained PAT the cloud uses to dispatch reproduction workflows. |
| `REPROIT_NO_UPDATE_CHECK` | `crates/reproit/src/adapters/update.rs` | Set to skip the CLI's update check. |
| `REPROIT_FIXED_COMMIT` | `crates/reproit/src/workflows/triage/setup.rs` | Commit under test in the generated repro CI workflow. |
| `REPROIT_APP_ID` | `crates/reproit/src/workflows/triage/mod.rs` | App id in the generated repro CI workflow. |

## Backend scan, fuzz, and gates

| Variable | Reference | Meaning |
| --- | --- | --- |
| `REPROIT_BACKEND_URL` | `crates/reproit/src/interface/cli/args.rs` | Backend service base URL (precedence: `--target` > this > `backend.target` > schema servers). |
| `REPROIT_BACKEND_RESET_URL` | `crates/reproit/src/workflows/backend_headless/reset.rs` | Legacy single reset URL; superseded by the config `reset:` block. |
| `REPROIT_GATE` | `crates/reproit/src/workflows/backend_headless/mod.rs` | Presence makes a run a CI gate: classify against the baseline without advancing it. |
| `REPROIT_GATE_BASELINE` | `crates/reproit/src/workflows/backend_headless/mod.rs` | In gate mode, explicitly re-baseline. |
| `REPROIT_GATE_JUNIT` | `crates/reproit/src/workflows/check.rs` | Path for a JUnit report of the gate run. |
| `REPROIT_EXTRA_HEADERS` | `crates/reproit/src/workflows/scan_command.rs` | Extra HTTP headers for scan requests. |
| `REPROIT_E2E_INGEST` | `sdk/reproit-backend-php/test/e2e_app.php` | Test fixture: stub ingest URL the PHP e2e app posts to. |
| `REPROIT_E2E_LOG` | `sdk/reproit-backend-php/test/e2e_ingest.php` | Test fixture: file the stub ingest appends received events to. |

## Orchestrator-to-runner contract (set by `adapters/drive.rs`)

| Variable | Reference | Meaning |
| --- | --- | --- |
| `REPROIT_PLATFORM` | `crates/reproit/src/workflows/device.rs` | Target platform for the spawned runner. |
| `REPROIT_DEVICE` | `crates/reproit/src/workflows/device.rs` | Target device for the spawned runner. |
| `REPROIT_ENGINE` | `crates/reproit/src/workflows/device.rs` | Target engine (browser) for the spawned runner. |
| `REPROIT_APP` | `crates/reproit/src/adapters/drive.rs` | App under test (binary path, bundle id, or URL depending on platform). |
| `REPROIT_TARGET` | `crates/reproit/src/adapters/atspi/mod.rs` | AT-SPI: app name substring or path to launch. |
| `REPROIT_URL` | `runners/web/jank-oracle.mjs` | Web: page under test. |
| `REPROIT_FUZZ_CONFIG` | `crates/reproit/src/adapters/drive.rs` | Fuzz config JSON (seed/budget/replay/prefix/edgeWeights); same contract on every backend. |
| `REPROIT_FUZZ_BUDGET` | `crates/reproit/src/adapters/uia/exploration.rs` | Action budget override for exploration. |
| `REPROIT_LOCALE` | `crates/reproit/src/workflows/repro.rs` | Locale a repro replays under; the runner reads it only from here. |
| `REPROIT_SHOTS_DIR` | `crates/reproit/src/adapters/drive.rs` | Where runner-side capture writes PNGs. |
| `REPROIT_VIDEO_DIR` | `crates/reproit/src/adapters/uia/mod.rs` | Where the run/replay video is written; arms clip capture in replay mode. |
| `REPROIT_CAPABILITIES_FILE` | `crates/reproit/src/adapters/drive.rs` | File the runner writes its capability report to. |
| `REPROIT_ACTION_FILE` | `sdk/reproit-tauri/src/lib.rs` | File the SDK reads injected actions from. |
| `REPROIT_INPUTS_FILE` | `crates/reproit/src/adapters/tui/session.rs` | TUI: file the SDK reads inputs from; absent in production, the registry is inert. |
| `REPROIT_INVARIANT_FILE` | `crates/reproit/src/adapters/tui/session.rs` | TUI: file the SDK writes `REPROIT_INVARIANT` markers to. |
| `REPROIT_CAPSULE` | `runners/source/react-native/part-06.mjs` | Path to the causal capsule staged for hermetic replay. |
| `REPROIT_CAPSULE_JSON` | `crates/reproit/src/adapters/drive.rs` | Capsule passed as a Flutter `--dart-define`. |
| `REPROIT_NETWORK_FILE` | `sdk/reproit-tui-go/causal.go` | Side file with provisioned network recordings for causal replay. |
| `REPROIT_CAUSAL` | `crates/reproit/src/adapters/drive.rs` | Set to 1 during a Repro It run so instrumentation installs before app bootstrap. |
| `REPROIT_UNDER_FUZZER` | `crates/reproit/src/adapters/atspi/capture.rs` | Set to 1 on the launched child; the SDK's fuzzer-detection gate. |
| `REPROIT_BACKEND` | `crates/reproit/src/adapters/drive.rs` | Set to 1 for backend runs. Undocumented beyond the call site. |
| `REPROIT_BACKEND_ORIGINS` | `crates/reproit/src/adapters/drive.rs` | Origins handed to the runner for backend correlation. Undocumented beyond the call site. |
| `REPROIT_SCENARIO_BARRIER` | `crates/reproit/src/adapters/tui/mod.rs` | Multi-actor scenario barrier base path; same contract as the web runner. |
| `REPROIT_INSPECT` | `crates/reproit/src/workflows/repro.rs` | Set to 1 to run a repro on the inspect (headless) tier. |
| `REPROIT_INSPECT_WAIT_MS` | `crates/reproit/src/workflows/repro.rs` | Inspect-tier wait budget in milliseconds. |
| `REPROIT_INSPECT_CONTROL` | `assets/scaffolds/flutter/.../config.dart` | Flutter explorer: control channel for simulator replay. |
| `REPROIT_RUNNERS` | `crates/reproit/src/adapters/config/mod.rs` | Directory holding the JS runner scripts; default is the bundled `runners/`. |
| `REPROIT_WEB_RUNNER_DIR` | `crates/reproit/src/adapters/config/web_runner.rs` | Override for the provisioned web-runner directory (normally not needed). |

## Web runner probes and oracles

| Variable | Reference | Meaning |
| --- | --- | --- |
| `REPROIT_ENGINES` | `runners/web/differential.mjs` | Differential: engines to compare (csv, default chromium,firefox,webkit). |
| `REPROIT_HEADLESS` | `runners/web/differential.mjs` | Differential: 1 forces headless (default headed + GPU). |
| `REPROIT_DIFF_OUT` | `runners/web/differential.mjs` | Differential: output directory. |
| `REPROIT_DIFF_FRAMES` | `runners/web/differential.mjs` | Differential: csv ms offsets to sample. |
| `REPROIT_DIFF_THRESHOLD` | `runners/web/differential.mjs` | Differential: max divergent-pixel ratio before a finding. |
| `REPROIT_DIFF_MEANDELTA` | `runners/web/differential.mjs` | Differential: mean channel delta separating divergence from AA noise. |
| `REPROIT_VIEWPORT` | `runners/web/differential.mjs` | Differential: viewport `WxH`. |
| `REPROIT_VIEWPORT_W` | `runners/source/web/part-07.mjs` | Web runner: viewport width override. |
| `REPROIT_VIEWPORT_H` | `runners/source/web/part-07.mjs` | Web runner: viewport height override. |
| `REPROIT_JANK_SECONDS` | `runners/web/jank.mjs` | Jank probe: capture duration. |
| `REPROIT_JANK_SELECTOR` | `runners/web/jank-oracle.mjs` | Jank oracle: selector of the animated element. |
| `REPROIT_JANK_DISPLAY` | `runners/web/jank-oracle.mjs` | Jank oracle: `isolated` gates the real-window mode to an isolated DISPLAY. |
| `REPROIT_PROBE` | `runners/web/probe.mjs` | 1 enables the side-effecting pixel probe for canvas/WebGL surfaces. |
| `REPROIT_DUPSUBMIT` | `crates/reproit/src/adapters/config/mod.rs` | 1 enables the double-dispatch (double-submit) runner probe. |
| `REPROIT_LISTENERLEAK` | `crates/reproit/src/adapters/config/mod.rs` | 1 enables the listener-leak revisit-loop probe. |
| `REPROIT_FLICKER_PIXELS` | `crates/reproit/src/domain/invariants/evaluate/edge.rs` | Enables the timing-sensitive flicker oracle. |
| `REPROIT_MAP_ACTION_BUDGET` | `runners/source/web/part-02.mjs` | Deterministic action budget for map exploration (default 72). |
| `REPROIT_BUILD` | `runners/web/runner.mjs` | Undocumented; read by the bundled web runner. |
| `REPROIT_CONFIG_CONTRACT` | `runners/web/runner.mjs` | Undocumented; read by the bundled web runner. |

## Platform runners

| Variable | Reference | Meaning |
| --- | --- | --- |
| `REPROIT_APPIUM_URL` | `runners/source/react-native/part-01.mjs` | Appium server base URL. |
| `REPROIT_APPIUM_CAPS` | `runners/source/react-native/part-05.mjs` | Per-actor Appium capabilities JSON; one device per actor. |
| `REPROIT_APPIUM_CONNECT_TIMEOUT_MS` | `runners/source/react-native/part-06.mjs` | Raises the webdriverio connect timeout (cold WebDriverAgent builds). |
| `REPROIT_DENY_PERMISSION` | `runners/source/react-native/part-06.mjs` | Permission-walk sweep: named permission is denied instead of granted. |
| `REPROIT_FUZZ` | `runners/source/react-native/part-06.mjs` | iOS: app-process flag (set via XCUITest) that tells the SDK it is under the fuzzer. |
| `REPROIT_APP_DIR` | `runners/source/electron/part-01.mjs` | Electron: dev app directory alternative to `REPROIT_APP`. |
| `REPROIT_ELECTRON_DISABLE_SANDBOX` | `runners/source/electron/part-01.mjs` | Electron: 1 disables the Chromium sandbox (CI containers). |
| `REPROIT_WEBDRIVER_URL` | `runners/source/tauri/part-01.mjs` | Tauri: endpoint of a running `tauri-driver`. |
| `REPROIT_MAC_ACTIVATE` | `runners/macos-ax/runtime.swift` | macOS AX: 0 skips activating the app (multi-actor focus safety). |
| `REPROIT_MAC_OFFSCREEN` | `runners/macos-ax/accessibility.swift` | macOS AX: 0 disables moving the window off-screen on the active Space. |
| `REPROIT_ALLOW_KEYS` | `runners/macos-ax/runtime.swift` | macOS AX: 1 permits synthetic key presses (fuzzing). |
| `REPROIT_CONFIG` | `runners/macos-ax/signature.swift` | macOS AX: path to `reproit.yaml` for value-node overrides. |
| `REPROIT_SELFTEST` | `runners/macos-ax/accessibility.swift` | macOS AX: 1 runs the golden-vector self-test in a DEBUG build. |
| `REPROIT_VECTORS` | `runners/macos-ax/accessibility.swift` | macOS AX: path to the signature golden vectors. |
| `REPROIT_TUI_CMD` | `crates/reproit/src/adapters/tui/mod.rs` | TUI: the terminal command to launch (via `sh -c`). |
| `REPROIT_TUI_CWD` | `crates/reproit/src/adapters/drive.rs` | TUI: project directory carried across the PTY intermediate process. |
| `REPROIT_TUI_EPS` | `crates/reproit/src/adapters/tui/mod.rs` | TUI: exploration epsilon (uniformity vs focus). |
| `REPROIT_TUI_FRAMES` | `crates/reproit/src/adapters/tui/mod.rs` | TUI: path to write per-action rendered frames. |
| `REPROIT_TUI_MOUSE` | `crates/reproit/src/adapters/tui/screen.rs` | TUI: 1 enables the SGR mouse signal. |
| `REPROIT_TUI_UNIFORM` | `crates/reproit/src/adapters/tui/mod.rs` | TUI: uniform-alphabet baseline mode for head-to-head measurement. |
| `REPROIT_VALUE_NODES` | `sdk/reproit_flutter/example/.../operability_fixture_model.dart` | Flutter: value-node override (`--dart-define`). |

## Secrets and credentials

| Variable | Reference | Meaning |
| --- | --- | --- |
| `REPROIT_VAULT_KEY` | `crates/reproit/src/adapters/credentials.rs` | AES-256-GCM key for the credentials vault. |
| `REPROIT_SECRET_<ACCOUNT>_<FIELD>` | `crates/reproit/src/adapters/credentials.rs` | Injected per-account secrets: `USERNAME`, `EMAIL`, `PHONE`, `PASSWORD`, `TOTP` (fresh code), `OTP` (fixed code), `STORAGE` (session blob). Journey `secret:` placeholders resolve to these names (`crates/reproit/src/workflows/journey/spec.rs`). |
| `REPROIT_BUNDLE_ENCRYPTION_KEY` | `crates/reproit/src/workflows/bundle.rs` | Support-bundle encryption key. |
| `REPROIT_BUNDLE_SIGNING_KEY` | `crates/reproit/src/workflows/bundle.rs` | Support-bundle signing key. |
| `REPROIT_BUNDLE_TRUSTED_SIGNER` | `crates/reproit/src/workflows/bundle.rs` | Trusted signer for bundle verification. |

## Capsule retention (domain-documented exception)

| Variable | Reference | Meaning |
| --- | --- | --- |
| `REPROIT_CAPSULE_KEY` | `crates/reproit/src/domain/capsule/retention.rs` | Team-held capsule key, 64 hex characters. Makes a shared capsule store replayable on another machine or in CI; read, never written, and it disables automatic rotation. |
| `REPROIT_CAPSULE_MAX_UNREFERENCED` | `crates/reproit/src/domain/capsule/retention.rs` | Max unreferenced capsules kept before pruning. |
| `REPROIT_CAPSULE_RETENTION_DAYS` | `crates/reproit/src/domain/capsule/retention.rs` | Age bound for unreferenced capsules. |
| `REPROIT_CAPSULE_KEY_ROTATION_DAYS` | `crates/reproit/src/domain/capsule/retention.rs` | Capsule key rotation interval. |

## Build, test, and development only

| Variable | Reference | Meaning |
| --- | --- | --- |
| `REPROIT_VERSION` | `crates/reproit/src/lib.rs` | Build-time (`env!`): version string baked into the binary. |
| `REPROIT_BUILD_VERSION` | `crates/reproit/src/workflows/command_capture.rs` | Version stamp recorded into a command capture. |
| `REPROIT_BUILD_COMMIT` | `crates/reproit/src/workflows/command_capture.rs` | Commit stamp recorded into a command capture. |
| `REPROIT_ESBUILD` | `sdk/build-reproit-web.mjs` | Path override for the esbuild binary. |
| `REPROIT_CLI_ROOT` | `sdk/reproit-backend-dotnet/.../CanonicalJsonTests.cs` | Repo root override for out-of-tree SDK test runs. |
| `REPROIT_WEB_SDK` | `sdk/test/environment_context_test.js` | SDK path override in SDK tests. |
| `REPROIT_TEST_BROWSER` | `runners/web/broken-asset.test.mjs` | Browser choice in runner tests. |
| `REPROIT_SCOPED_ENV_*_TEST`, `REPROIT_TEST_SCOPED_*` | `crates/reproit/src/adapters/scoped_env.rs`, `crates/reproit/src/workflows/tests.rs` | Sentinel keys used only inside unit tests. |

## Not environment variables

These `REPROIT_*` identifiers show up in the sweep but are not read from the environment:

- Template literals the CLI substitutes into generated files: `__REPROIT_APP_ID__`
  (`crates/reproit/src/workflows/triage/mod.rs`), `__REPROIT_ACTOR_LITERAL__`, `__REPROIT_CAPSULE_LITERAL__`
  (`sdk/reproit-tauri`), and the `REPROIT_A2UI_*__` page-global markers plus `__REPROIT_FIBER__`
  (runner-injected JS globals).
- Marker-line protocol words: `REPROIT_INVARIANT`, `REPROIT_RELATION`.
- Error codes: `REPROIT_A2UI_FAILURE`, `REPROIT_A2UI_PREFLIGHT_FAILED`.
- Source constants and macros: `REPROIT_GITIGNORE` (Rust const), `REPROIT_UA_TOKEN`,
  `REPROIT_UTF8` (JS consts), `REPROIT_CAUSAL_H`, `REPROIT_CAUSAL_IMPLEMENTATION`,
  `REPROIT_GTK_DEMO_MAIN`, `REPROIT_SWIFT_H` (C/ObjC include guards and build macros).
