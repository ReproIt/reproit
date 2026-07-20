//! Bounded fuzz configuration and deterministic random choice for the TUI backend.

use super::{Clip, ACTION_BUDGET};
use std::collections::BTreeMap;

pub(super) struct Fuzz {
    pub(super) seed: u32,
    pub(super) budget: u32,
    pub(super) configured: bool,
    pub(super) replay: Option<Vec<String>>,
    pub(super) prefix: Option<Vec<String>>,
    pub(super) edge_weights: BTreeMap<String, BTreeMap<String, u64>>,
    /// --record-video clip plan (see Clip); armed only alongside a replay.
    pub(super) clip: Option<Clip>,
    // Production-seeded corpus: real user paths (from SDK telemetry) to replay
    // into a realistic deep state, then BRANCH outward from. Bugs cluster where
    // users actually go, and the costly part of fuzzing is reaching a valid deep
    // state, so a real path teleports us there for free.
    pub(super) seeds: Vec<Vec<String>>,
}

pub(super) fn load_fuzz() -> Fuzz {
    let mut f = Fuzz {
        seed: 0,
        budget: ACTION_BUDGET,
        configured: false,
        replay: None,
        prefix: None,
        edge_weights: BTreeMap::new(),
        clip: None,
        seeds: Vec::new(),
    };
    let Ok(path) = std::env::var("REPROIT_FUZZ_CONFIG") else {
        return f;
    };
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return f;
    };
    let Ok(j) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return f;
    };
    f.configured = true;
    if let Some(s) = j.get("seed").and_then(|v| v.as_u64()) {
        f.seed = s as u32;
    }
    if let Some(b) = j.get("budget").and_then(|v| v.as_u64()) {
        f.budget = b as u32;
    }
    f.replay = j.get("replay").and_then(|v| v.as_array()).map(|a| {
        a.iter()
            .filter_map(|x| x.as_str().map(String::from))
            .collect()
    });
    f.prefix = j.get("prefix").and_then(|v| v.as_array()).map(|a| {
        a.iter()
            .filter_map(|x| x.as_str().map(String::from))
            .collect()
    });
    // --record-video clip plan: {"clip":{"sel","label","oracle"}}. Only meaningful in
    // replay mode with REPROIT_VIDEO_DIR set; the driver checks both before arming.
    if let Some(c) = j.get("clip").and_then(|v| v.as_object()) {
        if let Some(sel) = c.get("sel").and_then(|v| v.as_str()) {
            f.clip = Some(Clip {
                sel: sel.to_string(),
                label: c
                    .get("label")
                    .and_then(|v| v.as_str())
                    .unwrap_or("finding")
                    .to_string(),
                oracle: c
                    .get("oracle")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            });
        }
    }
    if let Some(ew) = j.get("edgeWeights").and_then(|v| v.as_object()) {
        for (sig, m) in ew {
            if let Some(mm) = m.as_object() {
                let inner = mm
                    .iter()
                    .filter_map(|(k, v)| v.as_u64().map(|n| (k.clone(), n)))
                    .collect();
                f.edge_weights.insert(sig.clone(), inner);
            }
        }
    }
    // seeds: a corpus of real user paths (each an array of "key:Name" actions),
    // typically lifted from production SDK telemetry. We branch outward from
    // these instead of always launching cold.
    if let Some(arr) = j.get("seeds").and_then(|v| v.as_array()) {
        for path in arr {
            if let Some(steps) = path.as_array() {
                let p: Vec<String> = steps
                    .iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect();
                if !p.is_empty() {
                    f.seeds.push(p);
                }
            }
        }
    }
    f
}

/// xorshift32, same recurrence as every other runner; high-bit reduction so
/// small alphabets don't hit the low-bit weakness.
pub(super) struct Rng {
    s: u32,
}
impl Rng {
    pub(super) fn new(seed: u32) -> Self {
        Rng {
            s: if seed == 0 { 1 } else { seed },
        }
    }
    pub(super) fn step(&mut self) -> u32 {
        self.s ^= self.s << 13;
        self.s ^= self.s >> 17;
        self.s ^= self.s << 5;
        self.s
    }
    pub(super) fn unit(&mut self) -> f64 {
        (self.step() as f64) / (u32::MAX as f64)
    }
}
