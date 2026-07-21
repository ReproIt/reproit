// Read the optional `value_nodes:` selector list from reproit.yaml (Layer 3). A
// tiny flat parser (a `value_nodes:` block of `- selector` items), so no YAML
// dep is pulled; a missing file/block yields an empty list.
pub(super) fn load_value_node_selectors() -> Vec<String> {
    let path = std::env::var("REPROIT_CONFIG").unwrap_or_else(|_| {
        std::env::current_dir()
            .map(|d| d.join("reproit.yaml").to_string_lossy().into_owned())
            .unwrap_or_else(|_| "reproit.yaml".into())
    });
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut in_block = false;
    for raw in text.lines() {
        let line = raw.trim_end();
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        if !line.starts_with(' ') && !line.starts_with('\t') {
            in_block = line.trim().trim_end_matches(':') == "value_nodes" && line.ends_with(':');
            continue;
        }
        if in_block {
            let item = line.trim();
            if let Some(sel) = item.strip_prefix('-') {
                let sel = sel.trim().trim_matches('"').trim_matches('\'');
                if !sel.is_empty() {
                    out.push(sel.to_string());
                }
            }
        }
    }
    out
}
