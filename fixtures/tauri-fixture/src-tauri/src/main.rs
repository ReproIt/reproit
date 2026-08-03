fn main() {
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("running Reproit Tauri fixture");
}
