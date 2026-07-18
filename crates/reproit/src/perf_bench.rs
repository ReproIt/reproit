//! Feature-gated benchmark workloads over real production hot paths.

use crate::model::appmap::{Action, AppMap, Reversibility, State, StateSignature, Transition};
use crate::model::map::{GraphIndex, Visits};
use std::collections::BTreeMap;
use std::path::PathBuf;

fn state(index: usize) -> State {
    State {
        name: None,
        description: "benchmark state".to_string(),
        signature: StateSignature {
            screenshot_phash: None,
            semantics_hash: Some(format!("sig-{index:05}")),
            route: Some(format!("/state/{index}")),
        },
        elements: vec![],
        texts: vec![],
        parameters: vec![],
        operability_gaps: Default::default(),
    }
}

fn transition(from: usize, to: usize) -> Transition {
    Transition {
        from: format!("s_{from:05}"),
        to: format!("s_{to:05}"),
        action: Action::Tap {
            finder: format!("key:next-{from}"),
        },
        guards: vec![],
        reversibility: Reversibility::ProposedReversible,
        expected: None,
    }
}

fn chain(count: usize) -> (AppMap, Visits) {
    let mut map = AppMap::empty("benchmark".to_string());
    for index in 0..count {
        map.states.insert(format!("s_{index:05}"), state(index));
    }
    for index in 0..count.saturating_sub(1) {
        map.transitions.push(transition(index, index + 1));
    }
    let visits = Visits {
        map_revision: map.revision,
        start: (count > 0).then(|| "sig-00000".to_string()),
        counts: BTreeMap::new(),
        edge_counts: BTreeMap::new(),
    };
    (map, visits)
}

pub struct FrontierWorkload {
    map: AppMap,
    visits: Visits,
}

impl FrontierWorkload {
    pub fn new(states: usize) -> Self {
        let (map, visits) = chain(states);
        Self { map, visits }
    }

    pub fn run(&self) -> usize {
        crate::model::map::frontier_path(&self.map, &self.visits)
            .map(|(_, path)| path.len())
            .unwrap_or(0)
    }
}

pub struct LogWorkload {
    log: String,
}

impl LogWorkload {
    pub fn new(megabytes: usize) -> Self {
        let block = concat!(
            "EXPLORE:STATE {\"sig\":\"home\",\"route\":\"/home\",",
            "\"labels\":[\"Home\"],\"elements\":[]}\n",
            "FUZZ:ACT tap:key:next\n",
            "EXPLORE:STATE {\"sig\":\"detail\",\"route\":\"/detail\",",
            "\"labels\":[\"Detail\"],\"elements\":[]}\n",
            "EXPLORE:EDGE {\"from\":\"home\",\"action\":\"tap:key:next\",",
            "\"to\":\"detail\"}\n",
        );
        let target = megabytes * 1024 * 1024;
        let mut log = String::with_capacity(target + block.len());
        while log.len() < target {
            log.push_str(block);
        }
        Self { log }
    }

    pub fn run(&self) -> usize {
        let parsed = crate::model::runner::ParsedRun::new(&self.log, &[], false, false);
        parsed.events.len() + parsed.map.edges.len() + parsed.trace.len()
    }
}

pub struct MergeWorkload {
    observations: crate::model::map::RunObs,
}

impl MergeWorkload {
    pub fn new(states: usize) -> Self {
        let mut log = String::new();
        for index in 0..states {
            log.push_str(&format!(
                "EXPLORE:STATE {{\"sig\":\"sig-{index:05}\",\"labels\":[]}}\n"
            ));
            if index > 0 {
                log.push_str(&format!(
                    "EXPLORE:EDGE {{\"from\":\"sig-{:05}\",",
                    index - 1
                ));
                log.push_str(&format!(
                    "\"action\":\"tap:key:next\",\"to\":\"sig-{index:05}\"}}\n"
                ));
            }
        }
        Self {
            observations: crate::model::map::parse_run(&log),
        }
    }

    pub fn run(&self) -> usize {
        let mut map = AppMap::empty("benchmark".to_string());
        crate::model::map::merge(&mut map, &self.observations);
        map.states.len() + map.transitions.len()
    }
}

pub struct BatchWorkload {
    map: AppMap,
    visits: Visits,
    seeds: usize,
}

impl BatchWorkload {
    pub fn new(seeds: usize) -> Self {
        let (map, visits) = chain(1_000);
        Self { map, visits, seeds }
    }

    pub fn run(&self) -> usize {
        let weights = self.visits.edge_weights(&self.map);
        let graph = GraphIndex::new(&self.map);
        let path = crate::model::map::frontier_path_with_index(&self.map, &self.visits, &graph)
            .map(|(_, path)| path.len())
            .unwrap_or(0);
        (0..self.seeds)
            .map(|seed| weights.len() + path + seed)
            .sum()
    }
}

pub struct PermissionWorkload {
    observations: crate::model::map::RunObs,
}

impl PermissionWorkload {
    pub fn new(states: usize) -> Self {
        let mut log = String::new();
        for index in 0..states {
            log.push_str(&format!("EXPLORE:STATE {{\"sig\":\"sig-{index:05}\",",));
            log.push_str(&format!(
                "\"route\":\"/state/{index}\",\"labels\":[],\"elements\":[]}}\n"
            ));
            log.push_str(&format!(
                "EXPLORE:PERMISSIONWALK {{\"sig\":\"sig-{index:05}\"}}\n"
            ));
            if index > 0 {
                log.push_str(&format!(
                    "EXPLORE:EDGE {{\"from\":\"sig-{:05}\",",
                    index - 1
                ));
                log.push_str(&format!(
                    "\"action\":\"tap:key:next\",\"to\":\"sig-{index:05}\"}}\n"
                ));
            }
        }
        Self {
            observations: crate::model::map::parse_run(&log),
        }
    }

    pub fn run(&self) -> usize {
        crate::model::invariants::benchmark_permission_traps(&self.observations)
    }
}

pub struct PersistenceWorkload {
    root: PathBuf,
    map: AppMap,
    visits: Visits,
}

impl PersistenceWorkload {
    pub fn new(states: usize) -> Self {
        let root = std::env::temp_dir().join(format!(
            "reproit-perf-persist-{}-{states}",
            std::process::id()
        ));
        let (map, visits) = chain(states);
        Self { root, map, visits }
    }

    pub fn run(&mut self) -> usize {
        self.map.mark_changed();
        crate::model::map::benchmark_save_snapshot(&self.root, &self.map, &mut self.visits)
            .expect("benchmark snapshot");
        self.map.revision as usize
    }
}

impl Drop for PersistenceWorkload {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).ok();
    }
}

pub struct FingerprintWorkload {
    root: PathBuf,
}

impl FingerprintWorkload {
    pub fn new(files: usize) -> Self {
        let root = std::env::temp_dir().join(format!(
            "reproit-perf-fingerprint-{}-{files}",
            std::process::id()
        ));
        std::fs::create_dir_all(root.join("src")).expect("benchmark source directory");
        let body = vec![b'x'; 64 * 1024];
        for index in 0..files {
            std::fs::write(root.join(format!("src/file-{index:05}.rs")), &body)
                .expect("benchmark source file");
        }
        crate::model::map::benchmark_fingerprint(&root, 1).expect("prime fingerprint cache");
        Self { root }
    }

    pub fn run(&self) -> usize {
        crate::model::map::benchmark_fingerprint(&self.root, 1)
            .expect("benchmark fingerprint")
            .len()
    }
}

impl Drop for FingerprintWorkload {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).ok();
    }
}
