use super::*;

pub(super) fn structural_input_elements() -> Vec<serde_json::Value> {
    let Ok(raw) = std::fs::read_to_string(input_file_path()) else {
        return Vec::new();
    };
    let mut by_selector = BTreeMap::<String, String>::new();
    for line in raw.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(sel) = v.get("sel").and_then(|x| x.as_str()) else {
            continue;
        };
        let Some(purpose) = v.get("inputPurpose").and_then(|x| x.as_str()) else {
            continue;
        };
        if let Some(canonical) = crate::domain::appmap::normalize_input_purpose(Some(purpose), sel)
        {
            by_selector.insert(sel.to_string(), canonical);
        }
    }
    by_selector
        .into_iter()
        .map(|(sel, input_purpose)| {
            serde_json::json!({
                "sel": sel, "role": "textfield", "label": "", "inputPurpose": input_purpose
            })
        })
        .collect()
}

/// Parse one line for the SDK marker `REPROIT_INVARIANT {json}`. Returns
/// `(sig, items)` where `items` is the list of VIOLATED `(id, message)` pairs
/// and `sig` is the SDK's own signature (or empty when it does not know it).
/// `None` for a non-marker line, malformed json, or an empty item list, so a
/// clean settle (no marker) and a garbled line both stay silent.
pub(super) fn parse_invariant_marker(line: &str) -> Option<(String, Vec<(String, String)>)> {
    const MARK: &str = "REPROIT_INVARIANT ";
    let idx = line.find(MARK)?;
    let json: serde_json::Value = serde_json::from_str(line[idx + MARK.len()..].trim()).ok()?;
    let items: Vec<(String, String)> = json
        .get("items")?
        .as_array()?
        .iter()
        .filter_map(|it| {
            let id = it.get("id").and_then(|v| v.as_str())?.to_string();
            let message = it
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Some((id, message))
        })
        .collect();
    if items.is_empty() {
        return None;
    }
    let sig = json
        .get("sig")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    Some((sig, items))
}

/// Incrementally scrapes the marker file and hands the runner the violations to
/// re-emit for its current state. The SDK and the runner compute the SAME
/// canonical TUI signature (reproit-tui-sig, golden-pinned), so a marker that
/// carries the SDK's own sig is matched to the runner's identical sig; an
/// empty-sig marker is attributed to the runner's next observed state. Per-sig
/// de-dup keeps a standing violation from being reported on every settle.
pub(super) struct InvariantScrape {
    path: String,
    pub(super) offset: u64,
    pending: Vec<u8>, // bytes of a not-yet-terminated trailing line across reads
    by_sig: BTreeMap<String, Vec<(String, String)>>,
    fallback: Option<Vec<(String, String)>>,
    emitted: BTreeSet<String>,
}

impl InvariantScrape {
    pub(super) fn new(path: &str) -> Self {
        InvariantScrape {
            path: path.to_string(),
            offset: 0,
            pending: Vec::new(),
            by_sig: BTreeMap::new(),
            fallback: None,
            emitted: BTreeSet::new(),
        }
    }

    /// Fold any newly appended marker lines into the pending maps. Reads bytes
    /// (not a String) and decodes only COMPLETE lines, so a read that lands
    /// mid-codepoint or mid-line never drops a marker.
    pub(super) fn ingest(&mut self) {
        use std::io::{Read as _, Seek, SeekFrom};
        let Ok(mut f) = std::fs::File::open(&self.path) else {
            return;
        };
        if f.seek(SeekFrom::Start(self.offset)).is_err() {
            return;
        }
        let mut buf = Vec::new();
        let Ok(n) = f.read_to_end(&mut buf) else {
            return;
        };
        self.offset += n as u64;
        self.pending.extend_from_slice(&buf);
        while let Some(nl) = self.pending.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=nl).collect();
            let text = String::from_utf8_lossy(&line);
            if let Some((sig, items)) = parse_invariant_marker(&text) {
                if sig.is_empty() {
                    self.fallback = Some(items);
                } else {
                    self.by_sig.insert(sig, items);
                }
            }
        }
    }

    /// The violations to report for `sig`, once (ingesting first). `None` when
    /// the app registered no failing invariant for this state, or it was
    /// already reported (per-sig de-dup).
    pub(super) fn pending_for(&mut self, sig: &str) -> Option<Vec<(String, String)>> {
        self.ingest();
        let items = self
            .by_sig
            .get(sig)
            .cloned()
            .or_else(|| self.fallback.take());
        let items = items?;
        if items.is_empty() || !self.emitted.insert(sig.to_string()) {
            return None;
        }
        Some(items)
    }

    /// Re-emit `EXPLORE:INVARIANT` for `sig` if the app reported a violation
    /// there.
    pub(super) fn flush_for(&mut self, sig: &str) {
        let Some(items) = self.pending_for(sig) else {
            return;
        };
        let arr: Vec<serde_json::Value> = items
            .iter()
            .map(|(id, message)| serde_json::json!({ "id": id, "message": message }))
            .collect();
        emit(&format!(
            "EXPLORE:INVARIANT {}",
            serde_json::json!({ "sig": sig, "items": arr })
        ));
    }
}
