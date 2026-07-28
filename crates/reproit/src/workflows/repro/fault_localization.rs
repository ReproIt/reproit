//! Coverage snapshot discovery and fault-localization reporting.

use crate::domain::fault;
use std::path::{Path, PathBuf};

pub(crate) fn why(directory: &str, top: usize) {
    let mut files = Vec::new();
    collect_coverage_files(Path::new(directory), &mut files);
    let runs: Vec<fault::RunCoverage> = files
        .iter()
        .filter_map(|path| {
            let value: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
            Some(fault::RunCoverage {
                passed: value
                    .get("passed")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(true),
                covered: value
                    .get("covered")
                    .and_then(serde_json::Value::as_array)
                    .map(|entries| {
                        entries
                            .iter()
                            .filter_map(serde_json::Value::as_str)
                            .map(String::from)
                            .collect()
                    })
                    .unwrap_or_default(),
            })
        })
        .collect();
    let failed = runs.iter().filter(|run| !run.passed).count();
    println!(
        "fault localization over {} coverage snapshot(s) ({failed} failing):",
        runs.len()
    );
    let ranked = fault::ochiai(&runs);
    if ranked.is_empty() {
        println!("  nothing to localize (no failing runs, or no coverage)");
    }
    for (element, suspiciousness) in ranked.into_iter().take(top) {
        println!("  {suspiciousness:.3}  {element}");
    }
}

fn collect_coverage_files(directory: &Path, output: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_coverage_files(&path, output);
        } else if path.to_string_lossy().ends_with(".cov.json") {
            output.push(path);
        }
    }
}
