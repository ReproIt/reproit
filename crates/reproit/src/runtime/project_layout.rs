//! Canonical `.reproit/` storage layout.
//!
//! Keep path construction here so command code does not grow its own filesystem
//! contract. Public/project state is reviewable (`map/`, `repros/`); generated
//! local state is ignored (`runs/`, `recordings/`, `tmp/`, vault/log files).

use std::path::{Path, PathBuf};

pub(crate) fn reproit_dir(root: &Path) -> PathBuf {
    root.join(".reproit")
}

pub(crate) fn config_path(root: &Path) -> PathBuf {
    reproit_dir(root).join("reproit.yaml")
}

pub(crate) fn map_dir(root: &Path) -> PathBuf {
    reproit_dir(root).join("map")
}

pub(crate) fn appmap_path(root: &Path) -> PathBuf {
    map_dir(root).join("appmap.json")
}

pub(crate) fn visits_path(root: &Path) -> PathBuf {
    map_dir(root).join("visits.json")
}

pub(crate) fn candidate_map_path(root: &Path) -> PathBuf {
    map_dir(root).join("candidate_map.json")
}

pub(crate) fn default_runs_dir_rel() -> &'static str {
    ".reproit/runs"
}

pub(crate) fn recordings_dir(root: &Path) -> PathBuf {
    reproit_dir(root).join("recordings")
}

/// Immutable, human-authored original captures. These may contain sensitive
/// screen media and therefore remain local until an explicit upload.
pub(crate) fn captures_dir(root: &Path) -> PathBuf {
    reproit_dir(root).join("captures")
}

pub(crate) fn scan_recordings_dir(root: &Path, scan_run: &str) -> PathBuf {
    recordings_dir(root).join("scan").join(scan_run)
}

pub(crate) fn repro_recording_dir(root: &Path, id: &str) -> PathBuf {
    recordings_dir(root).join("repro").join(id)
}

pub(crate) fn repro_video_path(root: &Path, id: &str, ext: &str) -> PathBuf {
    repro_recording_dir(root, id).join(format!("video.{ext}"))
}

pub(crate) fn repros_dir(root: &Path) -> PathBuf {
    reproit_dir(root).join("repros")
}

pub(crate) fn findings_dir(root: &Path) -> PathBuf {
    reproit_dir(root).join("findings")
}

pub(crate) fn finding_dir(root: &Path, id: &str) -> PathBuf {
    findings_dir(root).join(id)
}

/// Follow a bounded provisional-to-confirmed finding alias chain. Alias files
/// contain only validated raw content ids and never escape the findings root.
pub(crate) fn canonical_finding_id(root: &Path, id: &str) -> String {
    let mut current = id.to_string();
    for _ in 0..4 {
        let Ok(next) = std::fs::read_to_string(finding_dir(root, &current).join("promoted-to"))
        else {
            break;
        };
        let next = next.trim();
        if next.len() != 12 || !next.chars().all(|character| character.is_ascii_hexdigit()) {
            break;
        }
        current = next.to_string();
    }
    current
}

pub(crate) fn tools_dir(root: &Path) -> PathBuf {
    reproit_dir(root).join("tools")
}

pub(crate) fn tool_dir(root: &Path, name: &str) -> PathBuf {
    tools_dir(root).join(name)
}

pub(crate) fn capsules_dir(root: &Path) -> PathBuf {
    reproit_dir(root).join("capsules")
}

pub(crate) fn capsule_dir(root: &Path, id: &str) -> PathBuf {
    capsules_dir(root).join(id)
}

pub(crate) fn capsule_key_path(root: &Path) -> PathBuf {
    reproit_dir(root).join("capsule.key")
}

pub(crate) fn repro_dir(root: &Path, id: &str) -> PathBuf {
    repros_dir(root).join(id)
}

pub(crate) fn secrets_vault_path(root: &Path) -> PathBuf {
    reproit_dir(root).join("secrets.vault")
}

pub(crate) fn tmp_dir(root: &Path) -> PathBuf {
    reproit_dir(root).join("tmp")
}

pub(crate) fn fuzz_config_path(root: &Path) -> PathBuf {
    tmp_dir(root).join("fuzz_config.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_contract_paths_are_canonical() {
        let root = Path::new("/project");

        assert_eq!(reproit_dir(root), PathBuf::from("/project/.reproit"));
        assert_eq!(
            config_path(root),
            PathBuf::from("/project/.reproit/reproit.yaml")
        );
        assert_eq!(map_dir(root), PathBuf::from("/project/.reproit/map"));
        assert_eq!(
            appmap_path(root),
            PathBuf::from("/project/.reproit/map/appmap.json")
        );
        assert_eq!(
            visits_path(root),
            PathBuf::from("/project/.reproit/map/visits.json")
        );
        assert_eq!(
            candidate_map_path(root),
            PathBuf::from("/project/.reproit/map/candidate_map.json")
        );
        assert_eq!(
            root.join(default_runs_dir_rel()),
            PathBuf::from("/project/.reproit/runs")
        );
        assert_eq!(
            scan_recordings_dir(root, "run-1"),
            PathBuf::from("/project/.reproit/recordings/scan/run-1")
        );
        assert_eq!(
            repro_video_path(root, "abc123", "webm"),
            PathBuf::from("/project/.reproit/recordings/repro/abc123/video.webm")
        );
        assert_eq!(repros_dir(root), PathBuf::from("/project/.reproit/repros"));
        assert_eq!(
            finding_dir(root, "abc123"),
            PathBuf::from("/project/.reproit/findings/abc123")
        );
        assert_eq!(
            tool_dir(root, "grpcurl-1.9.3"),
            PathBuf::from("/project/.reproit/tools/grpcurl-1.9.3")
        );
        assert_eq!(
            capsules_dir(root),
            PathBuf::from("/project/.reproit/capsules")
        );
        assert_eq!(
            repro_dir(root, "rep_123"),
            PathBuf::from("/project/.reproit/repros/rep_123")
        );
        assert_eq!(
            secrets_vault_path(root),
            PathBuf::from("/project/.reproit/secrets.vault")
        );
        assert_eq!(
            fuzz_config_path(root),
            PathBuf::from("/project/.reproit/tmp/fuzz_config.json")
        );
    }

    #[test]
    fn old_paths_are_not_canonical() {
        let root = Path::new("/project");
        let canonical = [
            appmap_path(root),
            visits_path(root),
            candidate_map_path(root),
            scan_recordings_dir(root, "run-1"),
            fuzz_config_path(root),
        ];
        let old_paths = [
            root.join(".reproit/appmap.json"),
            root.join(".reproit/visits.json"),
            root.join(".reproit/candidate_map.json"),
            root.join(".reproit/scan-clips/run-1"),
            root.join(".reproit/media/repro/abc123/video.webm"),
            root.join(".reproit/fuzz_config.json"),
        ];

        for path in old_paths {
            assert!(
                !canonical.contains(&path),
                "old path is still treated as canonical: {}",
                path.display()
            );
        }
    }

    #[test]
    fn finding_aliases_resolve_without_leaving_the_findings_store() {
        let root =
            std::env::temp_dir().join(format!("reproit-finding-alias-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let alias = finding_dir(&root, "aaaaaaaaaaaa");
        std::fs::create_dir_all(&alias).unwrap();
        std::fs::write(alias.join("promoted-to"), "bbbbbbbbbbbb\n").unwrap();
        assert_eq!(canonical_finding_id(&root, "aaaaaaaaaaaa"), "bbbbbbbbbbbb");
        std::fs::write(alias.join("promoted-to"), "../../outside\n").unwrap();
        assert_eq!(canonical_finding_id(&root, "aaaaaaaaaaaa"), "aaaaaaaaaaaa");
        let _ = std::fs::remove_dir_all(root);
    }
}
