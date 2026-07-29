//! Distinguish source-neutral compiled plans from action replay packages.

use crate::domain::repro;
use std::path::Path;

pub(super) fn has_compiled_plan(root: &Path, meta: &repro::Meta) -> bool {
    let directory = repro::repro_dir(root, &meta.id);
    let Ok(package) = std::fs::read(directory.join("package.json")) else {
        return false;
    };
    let Ok(package) = serde_json::from_slice::<serde_json::Value>(&package) else {
        return false;
    };
    package
        .get("plan")
        .is_some_and(serde_json::Value::is_object)
        && directory.join("plan.json").is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_action_package_does_not_override_standard_replay() {
        let root = std::env::temp_dir().join(format!(
            "reproit-check-package-routing-{}",
            std::process::id()
        ));
        let meta = repro::Meta {
            id: repro::repro_id(0, &["tap:key:save"]),
            alias: Some("cloud-action-replay".into()),
            status: repro::Status::Quarantined,
            seed: 0,
            created: "2026-07-29T00:00:00Z".into(),
            last_checked: None,
            last_result: None,
            trigger_index: Some(1),
            trigger_sig: Some("crash:save".into()),
            trigger_selector: None,
            trigger_fingerprint: None,
            oracle: Some("crash".into()),
            record_url: None,
            record_action: None,
        };
        let directory = repro::repro_dir(&root, &meta.id);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("package.json"), "{}").unwrap();
        assert!(!has_compiled_plan(&root, &meta));
        std::fs::write(directory.join("plan.json"), "{}").unwrap();
        assert!(!has_compiled_plan(&root, &meta));
        std::fs::write(directory.join("package.json"), r#"{"plan": {}}"#).unwrap();
        assert!(has_compiled_plan(&root, &meta));
        let _ = std::fs::remove_dir_all(root);
    }
}
