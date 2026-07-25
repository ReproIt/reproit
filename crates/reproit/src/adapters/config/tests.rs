use super::{interpolate_value, load, synthesize_tui, synthesize_web};
use std::path::PathBuf;

// Interpolate a one-key YAML document and return the resolved scalar so the
// tests exercise the real parse-then-substitute path (type-preserving), not a
// raw text substitution.
fn expand(yaml: &str) -> anyhow::Result<serde_yaml::Value> {
    let mut value: serde_yaml::Value = serde_yaml::from_str(yaml).unwrap();
    interpolate_value(&mut value)?;
    Ok(value["v"].clone())
}

fn expand_str(yaml: &str) -> anyhow::Result<String> {
    Ok(expand(yaml)?.as_str().unwrap_or_default().to_string())
}

#[test]
fn synthesize_web_parses_to_a_valid_web_config() {
    let proj = std::env::temp_dir().join(format!("reproit_synth_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&proj);
    std::fs::create_dir_all(&proj).unwrap();
    let l = synthesize_web(
        "https://app.example.com/x:y",
        &PathBuf::from("/tmp/wr"),
        proj.clone(),
    )
    .expect("synthesized web config parses + validates");
    assert_eq!(l.config.app.platform, "web");
    assert_eq!(
        l.config.app.url.as_deref(),
        Some("https://app.example.com/x:y")
    );
    assert_eq!(l.config.app.web_runner_dir.as_deref(), Some("/tmp/wr"));
    assert_eq!(l.root, proj);
    // The journeys.doneMarkers validation (load's hard gate) must pass.
    assert!(!l.config.journeys.done_markers.is_empty());
    // The synthesized config is persisted so a later check/keep can replay.
    assert!(proj.join(".reproit").join("reproit.yaml").exists());
    let _ = std::fs::remove_dir_all(&proj);
}

#[test]
fn synthesize_tui_parses_to_a_valid_tui_config() {
    let proj = std::env::temp_dir().join(format!("reproit_tui_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&proj);
    std::fs::create_dir_all(&proj).unwrap();
    // A command with args + a quote, to exercise the JSON/YAML escaping.
    let l = synthesize_tui("lazygit --use-config \"x y\"", proj.clone())
        .expect("synthesized tui config parses + validates");
    assert_eq!(l.config.app.platform, "tui");
    assert_eq!(
        l.config.app.executable.as_deref(),
        Some("lazygit --use-config \"x y\"")
    );
    assert!(!l.config.journeys.done_markers.is_empty());
    assert_eq!(
        l.config.journeys.device_done_marker.as_deref(),
        Some("JOURNEY DONE")
    );
    assert!(proj.join(".reproit").join("reproit.yaml").exists());
    let _ = std::fs::remove_dir_all(&proj);
}

#[test]
fn zero_config_persists_and_reloads_rooted_at_cwd() {
    // The zero-config `fuzz <url>` papercut fix: synthesize_web persists its
    // config, and loading that persisted `.reproit/reproit.yaml` re-roots at
    // the PROJECT dir (not `.reproit/`), so a follow-up `reproit <id>` resolves
    // `.reproit/runs` and friends from the cwd and replays correctly.
    let proj = std::env::temp_dir().join(format!("reproit_reload_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&proj);
    std::fs::create_dir_all(&proj).unwrap();
    synthesize_web(
        "https://app.example.com",
        &PathBuf::from("/tmp/wr"),
        proj.clone(),
    )
    .expect("synthesize");
    let synth = proj.join(".reproit").join("reproit.yaml");
    assert!(synth.exists(), "config persisted under .reproit/");
    let reloaded = load(Some(&synth)).expect("reload persisted config");
    assert_eq!(
        reloaded.root,
        proj.canonicalize().unwrap(),
        "root is the project dir, not .reproit/"
    );
    assert_eq!(
        reloaded.config.app.url.as_deref(),
        Some("https://app.example.com")
    );
    let _ = std::fs::remove_dir_all(&proj);
}

// Each test uses a unique var name so parallel tests don't race on env state.
#[test]
fn bare_var_substitutes_or_empties() {
    std::env::set_var("RIT_TEST_BARE", "/runner");
    assert_eq!(expand_str("v: ${RIT_TEST_BARE}").unwrap(), "/runner");
    std::env::remove_var("RIT_TEST_BARE_UNSET");
    assert_eq!(expand_str("v: ${RIT_TEST_BARE_UNSET}").unwrap(), "");
}

#[test]
fn default_form_falls_back_when_unset() {
    std::env::remove_var("RIT_TEST_DEF");
    assert_eq!(
        expand_str("v: ${RIT_TEST_DEF:-./runners/web}").unwrap(),
        "./runners/web"
    );
    std::env::set_var("RIT_TEST_DEF", "/explicit");
    assert_eq!(
        expand_str("v: ${RIT_TEST_DEF:-./runners/web}").unwrap(),
        "/explicit"
    );
}

#[test]
fn required_form_errors_when_unset() {
    std::env::remove_var("RIT_TEST_REQ");
    let err = expand("v: ${RIT_TEST_REQ:?must be set}").unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("RIT_TEST_REQ"), "got: {msg}");
    assert!(msg.contains("must be set"), "got: {msg}");
}

#[test]
fn required_form_passes_when_set() {
    std::env::set_var("RIT_TEST_REQ_OK", "x");
    assert_eq!(expand_str("v: ${RIT_TEST_REQ_OK:?nope}").unwrap(), "x");
}

// The regression the parse-then-substitute design exists to prevent: an
// env-supplied value that looks numeric stays a string, so a `${VAR}` in an
// unquoted YAML scalar cannot be re-coerced into an int (which a downstream
// Json<String> extractor would reject with an opaque 422). A `${VAR}` in a
// comment is left untouched because comments are gone by parse time.
#[test]
fn substituted_numeric_value_stays_a_string() {
    std::env::set_var("RIT_TEST_PHONE", "+15551230001");
    let value = expand("v: ${RIT_TEST_PHONE} # ${RIT_TEST_UNDECLARED:?never}").unwrap();
    assert_eq!(value.as_str(), Some("+15551230001"));
    assert!(value.is_string(), "must stay a string, not coerce to int");
}

// End-to-end: app.webRunnerDir (the field from issue #1) resolves through the
// real loader, both the :-default fallback and an explicit override.
#[test]
fn loader_resolves_app_web_runner_dir() {
    let dir = std::env::temp_dir().join(format!("rit_cfg_e2e_wrd_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("reproit.yaml");
    std::fs::write(
        &path,
        "app:\n  platform: web\n  webRunnerDir: ${RIT_E2E_WRD:-./runners/web}\ndevices:\n  \
         namePrefix: x\njourneys:\n  driver: noop\n  doneMarkers: [done]\n",
    )
    .unwrap();

    std::env::remove_var("RIT_E2E_WRD");
    let loaded = super::load(Some(&path)).unwrap();
    assert_eq!(
        loaded.config.app.web_runner_dir.as_deref(),
        Some("./runners/web")
    );

    std::env::set_var("RIT_E2E_WRD", "/custom/runner");
    let loaded = super::load(Some(&path)).unwrap();
    assert_eq!(
        loaded.config.app.web_runner_dir.as_deref(),
        Some("/custom/runner")
    );

    std::env::remove_var("RIT_E2E_WRD");
    std::fs::remove_dir_all(&dir).ok();
}

fn examples_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/configs")
}

// Every shipped per-platform example must parse + resolve its platform +
// satisfy the schema. This is what would have caught the issue-#1 mistake (a
// top-level / misplaced field), and it guards every framework's example, so
// they can't silently rot as the schema evolves.
#[test]
fn all_example_configs_load() {
    let dir = examples_dir();
    let mut count = 0;
    for entry in std::fs::read_dir(&dir).expect("examples/configs") {
        let p = entry.unwrap().path();
        if p.extension().and_then(|e| e.to_str()) != Some("yaml") {
            continue;
        }
        super::load(Some(&p)).unwrap_or_else(|e| panic!("{} failed to load: {e:#}", p.display()));
        count += 1;
    }
    assert_eq!(count, 11, "expected 11 example configs");
}

// The desktop-toolkit example covers four platform ids in one file; verify
// each id actually loads (swap it into the example, load, assert ok).
#[test]
fn desktop_toolkit_ids_all_load() {
    let src = std::fs::read_to_string(examples_dir().join("reproit.desktop-toolkit.yaml"))
        .expect("toolkit example");
    let dir = std::env::temp_dir().join("rit_toolkit_ids");
    std::fs::create_dir_all(&dir).unwrap();
    for id in ["qt", "gtk", "avalonia", "wxwidgets"] {
        let yaml = src.replace("platform: qt", &format!("platform: {id}"));
        let path = dir.join("reproit.yaml");
        std::fs::write(&path, yaml).unwrap();
        super::load(Some(&path)).unwrap_or_else(|e| panic!("toolkit {id} failed: {e:#}"));
    }
    std::fs::remove_dir_all(&dir).ok();
}
