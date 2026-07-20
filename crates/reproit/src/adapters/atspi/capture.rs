use super::*;

pub(super) fn emit(s: &str) {
    println!("{s}");
    let _ = std::io::stdout().flush();
}

// APP-INVARIANT oracle (EXPLORE:INVARIANT, SDK-self-triggered).
//
// The app declares its own predicates via the reproit-linux SDK
// (`ReproIt.invariant("id", fn)`). Under the fuzzer the SDK evaluates them on
// its state-observe hook and writes the FAILURES to its stderr as a marker line
//   REPROIT_INVARIANT {"sig":"<sig-or-empty>","items":[{"id","message"}...]}
// A native Linux app is a separate process reproit launches, so its stderr is a
// clean diagnostic channel (unlike the TUI PTY, where stdout/stderr are the
// rendered frames): we pipe it, scrape the markers in a reader thread, and
// re-emit each as the CLI wire line `EXPLORE:INVARIANT` keyed on the signature
// the runner is CURRENTLY on, de-duped per state. The SDK's fuzzer-detection
// gate is `REPROIT_UNDER_FUZZER=1`, set on the launched child below; absent
// (production) the SDK registry stays inert.

/// Parse one line for the SDK marker `REPROIT_INVARIANT {json}`. Returns
/// `(sig, items)` with `items` the VIOLATED `(id, message)` pairs and `sig` the
/// SDK's own signature (empty when unknown). `None` for a non-marker line,
/// malformed json, or an empty list, so a clean settle stays silent.
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

/// Shared reader-thread state: the most recent violation set the SDK reported,
/// keyed by the SDK's own signature (plus an empty-sig fallback bucket).
#[derive(Default)]
pub(super) struct InvariantState {
    pub(super) by_sig: BTreeMap<String, Vec<(String, String)>>,
    pub(super) fallback: Option<Vec<(String, String)>>,
}

/// Scrapes an app-under-test's stderr for `REPROIT_INVARIANT` markers and lets
/// the walk re-emit them as `EXPLORE:INVARIANT` on the runner's current sig.
/// The SDK and the runner compute the SAME canonical a11y signature
/// (crate::domain::signature / the reproit-linux port), so a marker carrying the SDK's
/// sig matches the runner's identical sig; an empty-sig marker lands on the
/// next observed state. Per-sig de-dup keeps a standing violation from
/// repeating on every settle.
pub(super) struct InvariantScrape {
    pub(super) state: Arc<Mutex<InvariantState>>,
    pub(super) emitted: BTreeSet<String>,
}

impl InvariantScrape {
    /// Start scraping `reader` (the child's piped stderr) on a background
    /// thread.
    pub(super) fn spawn(reader: impl std::io::Read + Send + 'static) -> Self {
        let state = Arc::new(Mutex::new(InvariantState::default()));
        let sink = state.clone();
        std::thread::spawn(move || {
            let mut buf = std::io::BufReader::new(reader);
            let mut line = String::new();
            loop {
                line.clear();
                match std::io::BufRead::read_line(&mut buf, &mut line) {
                    Ok(0) | Err(_) => break, // EOF or a decode error ends the scrape
                    Ok(_) => {}
                }
                if let Some((sig, items)) = parse_invariant_marker(&line) {
                    let mut s = sink.lock().unwrap();
                    if sig.is_empty() {
                        s.fallback = Some(items);
                    } else {
                        s.by_sig.insert(sig, items);
                    }
                }
            }
        });
        InvariantScrape {
            state,
            emitted: BTreeSet::new(),
        }
    }

    /// The violations to report for `sig`, once. `None` when the app registered
    /// no failing invariant there, or it was already reported (per-sig de-dup).
    pub(super) fn pending_for(&mut self, sig: &str) -> Option<Vec<(String, String)>> {
        let items = {
            let mut s = self.state.lock().unwrap();
            s.by_sig.get(sig).cloned().or_else(|| s.fallback.take())
        };
        let items = items?;
        if items.is_empty() || !self.emitted.insert(sig.to_string()) {
            return None;
        }
        Some(items)
    }

    /// Re-emit `EXPLORE:INVARIANT` for `sig` if the app reported a violation
    /// there and it has not already been reported this run.
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

pub(super) struct Snapshot {
    pub(super) sig: String,
    pub(super) content: String,
    pub(super) labels: Vec<String>,
    pub(super) elements: Vec<serde_json::Value>,
    pub(super) tappables: Vec<String>,
    pub(super) nodes: HashMap<String, Acc>,
    pub(super) content_bugs: Vec<(String, &'static str, String)>,
    pub(super) broken_assets: Vec<(String, String)>,
}

pub(super) fn snapshot(app: &Acc, value_selectors: &[String], cap: &mut ValueCap) -> Snapshot {
    let anchor = anchor_of(app);
    let mut root = build_node(app, 0);
    apply_value_nodes(&mut root, value_selectors);
    let sig = cap.effective_signature(anchor.as_deref(), &root);
    let content = content_fingerprint(anchor.as_deref(), &root);

    let mut acc = Accum::default();
    visit(app, 0, &mut acc);

    acc.content_bugs
        .sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(b.1)));
    acc.broken_assets.sort_by(|a, b| a.0.cmp(&b.0));
    Snapshot {
        sig,
        content,
        labels: dedup(acc.labels),
        elements: acc.elements,
        tappables: dedup(acc.tappables),
        nodes: acc.nodes,
        content_bugs: acc.content_bugs,
        broken_assets: acc.broken_assets,
    }
}

#[derive(Default)]
struct Accum {
    pub(super) labels: Vec<String>,
    pub(super) elements: Vec<serde_json::Value>,
    pub(super) tappables: Vec<String>,
    nodes: HashMap<String, Acc>,
    content_bugs: Vec<(String, &'static str, String)>,
    content_bug_seen: HashSet<String>,
    broken_assets: Vec<(String, String)>,
    broken_asset_seen: HashSet<String>,
}

fn visit(acc: &Acc, depth: usize, a: &mut Accum) {
    if depth > 60 {
        return;
    }
    let rn = role_name(acc);
    let crole = if rn.is_empty() {
        "node"
    } else {
        atspi_role(&rn)
    };
    let is_tap = TAPPABLE_ROLE_NAMES.contains(&rn.as_str());
    let label = acc_name(acc);
    if crole == "textfield" {
        if let Some(id) = acc_id(acc).filter(|id| !id.is_empty()) {
            let sel = format!("key:{id}");
            let purpose = crate::domain::appmap::normalize_input_purpose(
                input_type_for(&rn, crole).as_deref(),
                &sel,
            );
            a.elements.push(serde_json::json!({
                "sel": sel, "role": crole, "label": label,
                "inputPurpose": purpose,
            }));
        }
    }
    if !label.is_empty() && label.chars().count() <= MAX_LABEL_LEN {
        a.labels.push(label.clone());
        if is_tap {
            a.tappables.push(label.clone());
            a.nodes.entry(label.clone()).or_insert_with(|| acc.dup());
        }
    }
    if !label.is_empty() {
        if let Some(reason) = content_bug_reason(&label) {
            let key = acc_key(acc, crole);
            let dedup = format!("{key}|{reason}");
            if a.content_bug_seen.insert(dedup) {
                let text: String = label.chars().take(80).collect();
                a.content_bugs.push((key, reason, text));
            }
        }
    }
    // BROKEN-ASSET (tofu) oracle: a rendered U+FFFD in this accessible's name
    // is broken text encoding on screen. Keyed by the stable node key, deduped,
    // so the marker is byte-identical run to run and addressed by id/role,
    // never the text.
    if let Some(detail) = tofu_detail(&label) {
        let key = acc_key(acc, crole);
        if a.broken_asset_seen.insert(key.clone()) {
            a.broken_assets.push((key, detail));
        }
    }
    for child in acc_children(acc) {
        if is_showing(&child) {
            visit(&child, depth + 1, a);
        }
    }
}

fn dedup(v: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for s in v {
        if seen.insert(s.clone()) {
            out.push(s);
        }
    }
    out
}

pub(super) fn crash(title: &str, detail: &str) {
    emit(&format!(
        "EXCEPTION CAUGHT BY REPROIT \u{2561} {title} \u{255e}"
    ));
    emit(&format!("The following condition was hit: {detail}"));
    emit(&"\u{2550}".repeat(8));
}

// LEAK sampler (MEMORY:SAMPLE, --soak): VmRSS from /proc/<pid>/status.
fn vmrss_bytes(pid: u32) -> Option<u64> {
    if pid == 0 {
        return None;
    }
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
            return Some(kb * 1024);
        }
    }
    None
}

pub(super) fn sample_rss(pid: u32, t_ms: u64) {
    if let Some(rss) = vmrss_bytes(pid) {
        emit(&format!(
            "MEMORY:SAMPLE {}",
            serde_json::json!({ "t_ms": t_ms, "heap_used": rss })
        ));
    }
}

pub(super) fn maybe_emit_hang(from_sig: &str, action: &str, elapsed_ms: u64) {
    if elapsed_ms >= HANG_FLOOR_MS {
        emit(&format!(
            "EXPLORE:HANG {}",
            serde_json::json!({ "from": from_sig, "action": action, "bucket": HANG_FLOOR_MS })
        ));
    }
}

// ── screenshot (SHOOT contract): external tools cropped to the window extents
// ─
fn sanitize_shot_name(raw: &str) -> String {
    raw.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '/' | '-'))
        .collect()
}

pub(super) fn app_window(app: &Acc) -> Acc {
    for child in acc_children(app) {
        let rn = role_name(&child);
        if rn == "FRAME" || rn == "WINDOW" || rn == "DIALOG" {
            return child;
        }
    }
    app.dup()
}

fn which(prog: &str) -> bool {
    std::env::var("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|p| p.join(prog).is_file()))
        .unwrap_or(false)
}

fn capture_window(window: &Acc, out_path: &std::path::Path) -> bool {
    let ext = extents(window);
    let ran_ok = |p: &std::path::Path| p.metadata().map(|m| m.len() > 0).unwrap_or(false);
    let run = |args: &[&str]| {
        let _ = std::process::Command::new(args[0])
            .args(&args[1..])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    };
    let out = out_path.to_string_lossy().into_owned();
    if which("gnome-screenshot") {
        run(&["gnome-screenshot", "-w", "-f", &out]);
        if ran_ok(out_path) {
            return true;
        }
    }
    if let Some((x, y, w, h)) = ext {
        if which("import") {
            run(&[
                "import",
                "-window",
                "root",
                "-crop",
                &format!("{w}x{h}+{x}+{y}"),
                &out,
            ]);
            if ran_ok(out_path) {
                return true;
            }
        }
        if which("grim") {
            run(&["grim", "-g", &format!("{x},{y} {w}x{h}"), &out]);
            if ran_ok(out_path) {
                return true;
            }
        }
        if which("scrot") {
            run(&["scrot", "-a", &format!("{x},{y},{w},{h}"), &out]);
            if ran_ok(out_path) {
                return true;
            }
        }
    }
    if which("import") {
        run(&["import", "-window", "root", &out]);
        if ran_ok(out_path) {
            return true;
        }
    }
    false
}

pub(super) fn shoot(app: &Acc, raw_name: &str) {
    let name = sanitize_shot_name(raw_name);
    if name.is_empty() {
        return;
    }
    if let Ok(dir) = std::env::var("REPROIT_SHOTS_DIR") {
        if !dir.is_empty() {
            let path = std::path::Path::new(&dir).join(format!("{name}.png"));
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = capture_window(&app_window(app), &path);
        }
    }
    emit(&format!("SHOOT:{name}"));
}

// fuzz config + graph helpers (same shape as backends/tui.rs and uia.rs).
