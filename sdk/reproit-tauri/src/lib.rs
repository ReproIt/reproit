use std::io::Write;
use tauri::{
    plugin::{Builder, TauriPlugin},
    Runtime,
};

#[tauri::command]
fn action_index() -> u32 {
    std::env::var("REPROIT_ACTION_FILE")
        .ok()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(0)
}

#[tauri::command]
fn record_exchange(line: String) -> Result<(), String> {
    let _: serde_json::Value = serde_json::from_str(&line).map_err(|error| error.to_string())?;
    let path = std::env::var("REPROIT_NETWORK_FILE")
        .map_err(|_| "causal capture is not active".to_string())?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| error.to_string())?;
    writeln!(file, "{line}").map_err(|error| error.to_string())
}

fn merge_capabilities() {
    let Ok(path) = std::env::var("REPROIT_CAPABILITIES_FILE") else {
        return;
    };
    let mut value: serde_json::Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    let Some(map) = value.as_object_mut() else {
        return;
    };
    map.insert(
        "http".into(),
        serde_json::json!({
            "status": "captured",
            "detail": "Tauri document-start fetch + XMLHttpRequest plugin"
        }),
    );
    map.insert(
        "http_replay".into(),
        serde_json::json!({
            "status": "captured",
            "detail": "Tauri fail-closed fetch + XMLHttpRequest replay"
        }),
    );
    let _ = std::fs::write(path, value.to_string());
}

/// Install once in `tauri::Builder`: `.plugin(tauri_plugin_reproit::init())`.
/// The script runs at document start, before application JavaScript.
pub fn init<R: Runtime>() -> TauriPlugin<R> {
    let active = std::env::var_os("REPROIT_NETWORK_FILE").is_some()
        || std::env::var_os("REPROIT_CAPSULE").is_some();
    if active {
        merge_capabilities();
    }
    let capsule = std::env::var("REPROIT_CAPSULE")
        .ok()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .unwrap_or_default();
    let actor = std::env::var("REPROIT_DEVICE").unwrap_or_else(|_| "a".into());
    let script = if active {
        include_str!("init.js")
            .replace(
                "__REPROIT_CAPSULE_LITERAL__",
                &serde_json::to_string(&capsule).unwrap(),
            )
            .replace(
                "__REPROIT_ACTOR_LITERAL__",
                &serde_json::to_string(&actor).unwrap(),
            )
    } else {
        String::new()
    };
    Builder::new("reproit")
        .js_init_script(script)
        .invoke_handler(tauri::generate_handler![action_index, record_exchange])
        .build()
}
