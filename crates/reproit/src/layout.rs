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
}
