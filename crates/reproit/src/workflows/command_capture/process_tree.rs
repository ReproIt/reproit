//! Bounded best-effort descendant observation for command capture.
//!
//! Polling improves attribution but cannot prove completeness because a child
//! may start and exit between samples. The capture capability remains partial.

use std::time::Instant;

#[cfg(unix)]
use std::time::Duration;

use reproit_protocol::{CaptureEventKind, ProcessIdentity};
use reproit_recorder::{EventContext, Recorder};

#[cfg(unix)]
const POLL_INTERVAL_MS: u64 = 100;
#[cfg(unix)]
const POLL_TIMEOUT_MS: u64 = 1_000;
#[cfg(unix)]
const MAX_POLLS: usize = 6_000;
#[cfg(unix)]
const MAX_PROCESSES_PER_POLL: usize = 4_096;
#[cfg(unix)]
const MAX_DESCENDANTS: usize = 4_096;
#[cfg(unix)]
const MAX_ANCESTRY_DEPTH: usize = 64;

pub(super) struct ObservedProcess {
    pub(super) process_id: u64,
    pub(super) parent_process_id: u64,
    pub(super) executable: String,
    pub(super) monotonic_ns: u64,
}

pub(super) struct Observation {
    pub(super) processes: Vec<ObservedProcess>,
    pub(super) defect: Option<String>,
}

pub(super) struct Observer {
    #[cfg(unix)]
    stop: tokio::sync::oneshot::Sender<()>,
    #[cfg(unix)]
    task: tokio::task::JoinHandle<Observation>,
}

pub(super) fn capability_detail() -> &'static str {
    #[cfg(unix)]
    {
        "descendants are polled every 100ms; short-lived children and IPC can be missed"
    }
    #[cfg(not(unix))]
    {
        "root process owned; native descendant observation is not installed"
    }
}

pub(super) fn observe(root_process_id: u64, started: Instant) -> Observer {
    #[cfg(unix)]
    {
        let (stop, receiver) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(observe_unix(root_process_id, started, receiver));
        Observer { stop, task }
    }
    #[cfg(not(unix))]
    {
        let _ = (root_process_id, started);
        Observer {}
    }
}

impl Observer {
    pub(super) async fn finish(self) -> Observation {
        #[cfg(unix)]
        {
            let _ = self.stop.send(());
            self.task.await.unwrap_or_else(|error| Observation {
                processes: Vec::new(),
                defect: Some(format!("descendant observer task failed: {error}")),
            })
        }
        #[cfg(not(unix))]
        {
            Observation {
                processes: Vec::new(),
                defect: None,
            }
        }
    }
}

pub(super) fn record_observation(
    recorder: &mut Recorder,
    observation: Observation,
    root_event_id: &str,
    started: Instant,
    root_process_id: u64,
) {
    for descendant in observation.processes {
        recorder.record(
            EventContext {
                monotonic_ns: descendant.monotonic_ns,
                wall_time: None,
                process_id: Some(descendant.process_id),
                causal_parent_ids: vec![root_event_id.to_string()],
                ..EventContext::default()
            },
            CaptureEventKind::ProcessStart {
                process: ProcessIdentity {
                    process_id: descendant.process_id,
                    executable: descendant.executable,
                    parent_process_id: Some(descendant.parent_process_id),
                    executable_hash: None,
                },
                arguments: None,
                working_directory: None,
            },
        );
    }
    if let Some(detail) = observation.defect {
        recorder.record(
            EventContext {
                monotonic_ns: started.elapsed().as_nanos().min(u64::MAX as u128) as u64,
                wall_time: None,
                process_id: Some(root_process_id),
                ..EventContext::default()
            },
            CaptureEventKind::Defect {
                defect: reproit_protocol::CaptureDefectKind::Truncated,
                detail,
                artifact_id: None,
            },
        );
    }
}

#[cfg(unix)]
async fn observe_unix(
    root_process_id: u64,
    started: Instant,
    mut stop: tokio::sync::oneshot::Receiver<()>,
) -> Observation {
    let mut observed = std::collections::BTreeMap::new();
    let mut polls = 0usize;
    let mut errors = 0usize;
    let mut interval = tokio::time::interval(Duration::from_millis(POLL_INTERVAL_MS));
    loop {
        tokio::select! {
            _ = &mut stop => break,
            _ = interval.tick(), if polls < MAX_POLLS => {
                polls += 1;
                match process_rows().await {
                    Ok(rows) => retain_descendants(root_process_id, started, &rows, &mut observed),
                    Err(()) => errors += 1,
                }
                if observed.len() >= MAX_DESCENDANTS {
                    break;
                }
            }
        }
        if polls >= MAX_POLLS {
            break;
        }
    }
    let defect = observation_defect(observed.len(), polls, errors);
    Observation {
        processes: observed.into_values().collect(),
        defect,
    }
}

#[cfg(unix)]
fn observation_defect(descendants: usize, polls: usize, errors: usize) -> Option<String> {
    if descendants >= MAX_DESCENDANTS {
        Some(format!(
            "descendant observation reached its {MAX_DESCENDANTS}-process limit"
        ))
    } else if polls >= MAX_POLLS {
        Some(format!(
            "descendant observation reached its {MAX_POLLS}-poll limit"
        ))
    } else if errors > 0 {
        Some(format!(
            "descendant observation failed on {errors}/{polls} polls"
        ))
    } else {
        None
    }
}

#[cfg(unix)]
async fn process_rows() -> Result<Vec<ProcessRow>, ()> {
    let output = tokio::time::timeout(
        Duration::from_millis(POLL_TIMEOUT_MS),
        tokio::process::Command::new("ps")
            .args(["-axo", "pid=,ppid=,comm="])
            .output(),
    )
    .await
    .map_err(|_| ())?
    .map_err(|_| ())?;
    if !output.status.success() || output.stdout.len() > 1024 * 1024 {
        return Err(());
    }
    Ok(parse_rows(&String::from_utf8_lossy(&output.stdout)))
}

#[cfg(unix)]
#[derive(Clone)]
struct ProcessRow {
    process_id: u64,
    parent_process_id: u64,
    executable: String,
}

#[cfg(unix)]
fn parse_rows(output: &str) -> Vec<ProcessRow> {
    output
        .lines()
        .take(MAX_PROCESSES_PER_POLL)
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let process_id = fields.next()?.parse().ok()?;
            let parent_process_id = fields.next()?.parse().ok()?;
            let executable = fields.next()?.chars().take(16 * 1024).collect::<String>();
            Some(ProcessRow {
                process_id,
                parent_process_id,
                executable,
            })
        })
        .collect()
}

#[cfg(unix)]
fn retain_descendants(
    root_process_id: u64,
    started: Instant,
    rows: &[ProcessRow],
    observed: &mut std::collections::BTreeMap<u64, ObservedProcess>,
) {
    let parents = rows
        .iter()
        .map(|row| (row.process_id, row.parent_process_id))
        .collect::<std::collections::BTreeMap<_, _>>();
    for row in rows {
        if observed.len() >= MAX_DESCENDANTS {
            break;
        }
        if row.process_id == root_process_id
            || !descends_from(row.process_id, root_process_id, &parents)
        {
            continue;
        }
        observed
            .entry(row.process_id)
            .or_insert_with(|| ObservedProcess {
                process_id: row.process_id,
                parent_process_id: row.parent_process_id,
                executable: row.executable.clone(),
                monotonic_ns: started.elapsed().as_nanos().min(u64::MAX as u128) as u64,
            });
    }
}

#[cfg(unix)]
fn descends_from(
    mut process_id: u64,
    root_process_id: u64,
    parents: &std::collections::BTreeMap<u64, u64>,
) -> bool {
    for _ in 0..MAX_ANCESTRY_DEPTH {
        let Some(parent) = parents.get(&process_id).copied() else {
            return false;
        };
        if parent == root_process_id {
            return true;
        }
        if parent == 0 || parent == process_id {
            return false;
        }
        process_id = parent;
    }
    false
}

#[cfg(all(test, unix))]
mod tests {
    use super::{capability_detail, descends_from, observe, parse_rows};
    use std::time::Instant;

    #[test]
    fn parsing_and_ancestry_are_bounded_and_transitive() {
        let rows = parse_rows(" 10 1 root\n 11 10 child\n 12 11 grandchild\n 20 1 other\n");
        let parents = rows
            .iter()
            .map(|row| (row.process_id, row.parent_process_id))
            .collect::<std::collections::BTreeMap<_, _>>();
        assert!(descends_from(11, 10, &parents));
        assert!(descends_from(12, 10, &parents));
        assert!(!descends_from(20, 10, &parents));
    }

    #[tokio::test]
    async fn observer_attributes_a_live_descendant_without_claiming_completeness() {
        let started = Instant::now();
        let mut root = tokio::process::Command::new("/bin/sh")
            .args(["-c", "sleep 0.5 & wait"])
            .spawn()
            .unwrap();
        let root_process_id = root.id().unwrap() as u64;
        let observer = observe(root_process_id, started);
        root.wait().await.unwrap();
        let observation = observer.finish().await;
        assert!(observation
            .processes
            .iter()
            .any(|process| process.executable.contains("sleep")));
        assert!(capability_detail().contains("can be missed"));
    }
}
