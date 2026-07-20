//! Conservative change-directed ordering for saved repro verification.

use crate::cli::context::Ctx;
use crate::model::repro;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::Path;
use std::process::Command;

const MAX_DIFF_BYTES: u64 = 4 * 1024 * 1024;
const MAX_CHANGED_PATHS: usize = 10_000;
const MAX_REPORT_BYTES: u64 = 4 * 1024 * 1024;

pub(super) fn prioritize(
    ctx: &Ctx,
    root: &Path,
    metas: Vec<repro::Meta>,
    base: &str,
) -> Vec<repro::Meta> {
    let changed = match changed_paths(root, base) {
        Ok(paths) if !paths.is_empty() => paths,
        Ok(_) => {
            ctx.say("  changed: no changed paths found; running the full suite in saved order");
            return metas;
        }
        Err(error) => {
            ctx.say(format!(
                "  changed: {error}; running the full suite in saved order"
            ));
            return metas;
        }
    };
    let dependencies = metas
        .iter()
        .map(|meta| (meta.id.clone(), repro_dependencies(root, meta)))
        .collect::<BTreeMap<_, _>>();
    let (mut affected, mut remaining): (Vec<_>, Vec<_>) = metas.into_iter().partition(|meta| {
        dependencies
            .get(&meta.id)
            .is_some_and(|paths| !paths.is_disjoint(&changed))
    });
    if affected.is_empty() {
        ctx.say(format!(
            "  changed: {} path(s), no exact source mapping; running the full suite in saved order",
            changed.len()
        ));
        return remaining;
    }
    ctx.say(format!(
        "  changed: running {} mapped repro(s) first, then all {} remaining repro(s)",
        affected.len(),
        remaining.len()
    ));
    affected.append(&mut remaining);
    affected
}

fn changed_paths(root: &Path, base: &str) -> Result<BTreeSet<String>, String> {
    if base.is_empty() || base.len() > 256 || base.contains('\0') {
        return Err("invalid change base".into());
    }
    let mut paths = BTreeSet::new();
    let range = format!("{base}...HEAD");
    for args in [
        vec!["diff", "--name-only", "--diff-filter=ACDMR", &range, "--"],
        vec!["diff", "--name-only", "--diff-filter=ACDMR", "HEAD", "--"],
        vec![
            "diff",
            "--cached",
            "--name-only",
            "--diff-filter=ACDMR",
            "HEAD",
            "--",
        ],
    ] {
        let output = Command::new("git")
            .args(&args)
            .current_dir(root)
            .output()
            .map_err(|error| format!("could not inspect changes: {error}"))?;
        if !output.status.success() {
            return Err(format!("git could not compare against `{base}`"));
        }
        if output.stdout.len() as u64 > MAX_DIFF_BYTES {
            return Err("changed path output exceeded its byte limit".into());
        }
        let text = std::str::from_utf8(&output.stdout)
            .map_err(|_| "changed paths were not valid UTF-8".to_string())?;
        for path in text
            .lines()
            .map(normalize_path)
            .filter(|path| !path.is_empty())
        {
            paths.insert(path);
            if paths.len() > MAX_CHANGED_PATHS {
                return Err("changed path count exceeded its limit".into());
            }
        }
    }
    Ok(paths)
}

fn repro_dependencies(root: &Path, meta: &repro::Meta) -> BTreeSet<String> {
    let report = repro::repro_dir(root, &meta.id).join("fuzz.md");
    let Ok(file) = std::fs::File::open(report) else {
        return BTreeSet::new();
    };
    let mut bytes = Vec::new();
    if file
        .take(MAX_REPORT_BYTES + 1)
        .read_to_end(&mut bytes)
        .is_err()
        || bytes.len() as u64 > MAX_REPORT_BYTES
    {
        return BTreeSet::new();
    }
    let Ok(report) = std::str::from_utf8(&bytes) else {
        return BTreeSet::new();
    };
    report
        .lines()
        .filter_map(crate::modes::deliver::suspected_source)
        .filter_map(|source| {
            source
                .rsplit_once(':')
                .map(|(path, _)| normalize_path(path))
        })
        .filter(|path| !path.is_empty())
        .collect()
}

fn normalize_path(path: &str) -> String {
    path.trim()
        .trim_start_matches("./")
        .trim_start_matches("package:")
        .replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_normalization_is_stable_across_report_formats() {
        assert_eq!(normalize_path("./lib/main.dart"), "lib/main.dart");
        assert_eq!(
            normalize_path("package:app/lib/main.dart"),
            "app/lib/main.dart"
        );
        assert_eq!(normalize_path("src\\main.rs"), "src/main.rs");
    }
}
