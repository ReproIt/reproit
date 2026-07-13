const COMMANDS: &[&str] = &["action_index", "record_exchange"];
fn main() {
    tauri_plugin::Builder::new(COMMANDS).build();
}
