//! Failure analyzer v0: reads an evidence bundle and asks the configured LLM
//! provider for a classification and likely cause. Triage for humans; the
//! LLM never gates anything. Later versions add coverage spectra (fault
//! localization) and feed the fix generator.

use crate::config::Config;
use anyhow::{bail, Context, Result};
use llm::Task;
use std::path::{Path, PathBuf};

pub async fn analyze(cfg: &Config, root: &Path, run: Option<&str>) -> Result<()> {
    let runs_dir = root.join(&cfg.evidence.out_dir);
    let run_dir = match run {
        Some(name) => runs_dir.join(name),
        None => latest_run(&runs_dir)?,
    };
    if !run_dir.is_dir() {
        bail!("no run directory at {}", run_dir.display());
    }
    println!("  analyzing {}", run_dir.display());

    let manifest = std::fs::read_to_string(run_dir.join("manifest.json")).unwrap_or_default();
    // Include the journey's own source: most misclassifications come from
    // not checking whether the TEST's assumptions are valid for this app.
    let journey_src = serde_json::from_str::<serde_json::Value>(&manifest)
        .ok()
        .and_then(|m| m.get("journey").and_then(|j| j.as_str()).map(String::from))
        .map(|name| {
            let p = root
                .join(&cfg.app.project_dir)
                .join(&cfg.journeys.dir)
                .join(format!("journey_{name}.dart"));
            std::fs::read_to_string(p).unwrap_or_default()
        })
        .unwrap_or_default();
    let actions = tail(&run_dir.join("actions.jsonl"), 120);
    let exceptions = tail(&run_dir.join("exceptions.jsonl"), 40);
    let mut logs = String::new();
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&run_dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with("drive-") && n.ends_with(".log"))
                .unwrap_or(false)
        })
        .collect();
    entries.sort();
    for log in entries {
        let name = log
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        logs.push_str(&format!("\n--- {name} (tail) ---\n{}\n", tail(&log, 200)));
    }

    let prompt = format!(
        r#"Analyze this end-to-end test run (a Flutter integration journey driven on iOS
simulators by an external orchestrator) and produce a failure triage report.

Classify the failure as exactly one of:
- app bug (the application misbehaved)
- test bug (the journey's finders/assumptions are wrong or flaky by construction)
- environment (backend/stack down or misseeded, simulator trouble, build failure, reset incomplete)

Then:
1. Cite the specific log lines that support the classification.
2. State the likely root cause in one or two sentences.
3. Recommend the single next debugging step.
4. Say whether the evidence suggests deterministic failure or a race (and what to vary to confirm).

Be honest about uncertainty; do not invent evidence.

Triage discipline: when a load-bearing expect failed on the EXISTENCE of a finder or
widget type, your FIRST hypothesis must be a test bug: check the journey source below
and ask whether its assumption (widget type, exact text) is plausibly valid for this
app at all. Unrelated errors in the log (rate limits, retries) are often incidental
noise; do not let them anchor the classification unless they causally connect to the
failed assertion. The most common triage error is calling a test bug "environment".

=== journey test source ===
{journey_src}

=== exceptions.jsonl (structured: kind, message, source frames with file:line) ===
{exceptions}

=== manifest.json ===
{manifest}

=== actions.jsonl (tail) ===
{actions}
{logs}"#
    );

    let provider = llm::from_spec(&cfg.llm.to_spec())?;
    println!("  triaging via {} ...", provider.name());
    let report = provider
        .complete(&Task::new(prompt).system(
            "You are a precise test-failure triage assistant. Ground every claim in the \
provided logs; quote them. Output plain markdown.",
        ))
        .await?;

    let out = run_dir.join("analysis.md");
    std::fs::write(&out, &report)?;
    println!("\n{report}\n");
    println!("  wrote {}", out.display());
    Ok(())
}

fn latest_run(runs_dir: &Path) -> Result<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(runs_dir)
        .with_context(|| format!("no runs under {} (run a journey first)", runs_dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    dirs.pop()
        .with_context(|| format!("no runs under {}", runs_dir.display()))
}

fn tail(path: &Path, lines: usize) -> String {
    match std::fs::read_to_string(path) {
        Ok(s) => {
            let all: Vec<&str> = s.lines().collect();
            let start = all.len().saturating_sub(lines);
            all[start..].join("\n")
        }
        Err(_) => String::new(),
    }
}
