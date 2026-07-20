//! Ground-truth state and terminal mouse interaction for the TUI backend.

use super::*;

/// One ground-truth element accumulated for a state. Serialized into the
/// `elements` array of an `EXPLORE:GROUNDTRUTH` line.
#[derive(Clone)]
pub(super) struct GtElement {
    pub(super) id: String,
    pub(super) gesture_kind: &'static str,
    /// false => emit a11y.inTabOrder:false + keyboardActivatable:false.
    pub(super) keyboard_operable: bool,
}

/// Accumulates `EXPLORE:GROUNDTRUTH` elements per state signature, and emits
/// one consolidated marker line per state whenever its element set changes.
/// Keyed by element id so a control rediscovered on a later visit does not
/// double-count.
pub(super) struct Groundtruth {
    /// sig -> (id -> element)
    pub(super) by_state: BTreeMap<String, BTreeMap<String, GtElement>>,
    /// sigs that have a focus trap observed (none, for now: TUIs have no Tab-
    /// ring we can prove trapped, so this stays false and is here for parity).
    focus_trap: BTreeSet<String>,
}

impl Groundtruth {
    pub(super) fn new() -> Self {
        Groundtruth {
            by_state: BTreeMap::new(),
            focus_trap: BTreeSet::new(),
        }
    }

    /// Record an element for `sig` and (re)emit the state's groundtruth line if
    /// the element was new or changed. Returns true if a line was emitted.
    pub(super) fn record(&mut self, sig: &str, el: GtElement) -> bool {
        let map = self.by_state.entry(sig.to_string()).or_default();
        let changed = match map.get(&el.id) {
            Some(prev) => {
                prev.gesture_kind != el.gesture_kind
                    || prev.keyboard_operable != el.keyboard_operable
            }
            None => true,
        };
        if changed {
            map.insert(el.id.clone(), el);
            self.emit(sig);
        }
        changed
    }

    /// Emit `EXPLORE:GROUNDTRUTH` for one state. The `sig` is byte-identical to
    /// the `EXPLORE:STATE` sig so the engine keys the gaps to the same node.
    /// Each element carries `operable:true` plus the a11y dims that are
    /// KNOWN-false; dims left out default to true at the engine, so we only
    /// ever ASSERT a failure we actually observed.
    fn emit(&self, sig: &str) {
        let Some(map) = self.by_state.get(sig) else {
            return;
        };
        let elements: Vec<serde_json::Value> = map
            .values()
            .map(|el| {
                let mut a11y = serde_json::Map::new();
                if !el.keyboard_operable {
                    // mouse-only control: not in the (nonexistent) keyboard tab
                    // order, and not keyboard-activatable.
                    a11y.insert("inTabOrder".into(), serde_json::Value::Bool(false));
                    a11y.insert("keyboardActivatable".into(), serde_json::Value::Bool(false));
                }
                serde_json::json!({
                    "id": el.id,
                    "operable": true,
                    "gestureKind": el.gesture_kind,
                    "a11y": serde_json::Value::Object(a11y),
                })
            })
            .collect();
        let payload = serde_json::json!({
            "sig": sig,
            "focusTrap": self.focus_trap.contains(sig),
            "elements": elements,
        });
        emit(&format!("EXPLORE:GROUNDTRUTH {payload}"));
    }
}

/// A deterministic mouse hotspot: a (row, col) cell to click, plus a stable id
/// describing where it came from. Signal B clicks these.
struct Hotspot {
    row: u16,
    col: u16,
    id: String,
}

/// Scan the grid for deterministic mouse hotspots, in a stable scan order:
///   - bracketed labels: `[ Save ]`, `[Yes]`, `<OK>` -> click the bracket
///     center.
///   - reverse-video (highlighted/selected) cell runs -> click the run center.
///   - footer hint tokens advertised as keys -> already keyboard-reachable, so
///     they are NOT added (a mouse click there would never be "mouse-only").
/// Returns at most `cap` hotspots so a dense screen cannot explode the budget.
fn mouse_hotspots(parser: &Arc<Mutex<vt100::Parser>>, cap: usize) -> Vec<Hotspot> {
    let p = parser.lock().unwrap();
    let screen = p.screen();
    let (rows, cols) = screen.size();
    let mut spots: Vec<Hotspot> = Vec::new();
    // Bracketed-label hotspots: an open bracket/angle, some text, a close.
    for r in 0..rows {
        let mut open: Option<u16> = None;
        for c in 0..cols {
            let ch = screen
                .cell(r, c)
                .and_then(|cell| cell.contents().chars().next())
                .unwrap_or(' ');
            match ch {
                '[' | '<' => open = Some(c),
                ']' | '>' => {
                    if let Some(o) = open.take() {
                        if c > o {
                            let mid = o + (c - o) / 2;
                            spots.push(Hotspot {
                                row: r,
                                col: mid,
                                id: format!("bracket:{r},{o}"),
                            });
                        }
                    }
                }
                _ => {}
            }
        }
    }
    // Reverse-video run hotspots: a maximal run of inverse cells is a highlighted
    // / selected widget; click its center.
    for r in 0..rows {
        let mut run_start: Option<u16> = None;
        for c in 0..=cols {
            let inv = c < cols
                && screen
                    .cell(r, c)
                    .map(|cell| cell.inverse())
                    .unwrap_or(false);
            match (inv, run_start) {
                (true, None) => run_start = Some(c),
                (false, Some(s)) => {
                    let end = c - 1;
                    let mid = s + (end - s) / 2;
                    spots.push(Hotspot {
                        row: r,
                        col: mid,
                        id: format!("reverse:{r},{s}"),
                    });
                    run_start = None;
                }
                _ => {}
            }
        }
    }
    spots.truncate(cap);
    spots
}

/// Mouse event encoding selected by the app's terminal mode requests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(super) enum MouseProtocol {
    None = 0,
    X10 = 1,
    Sgr = 2,
}

const MOUSE_TRACKING_ENABLED: u8 = 1 << 0;
const SGR_MOUSE_ENCODING_ENABLED: u8 = 1 << 1;

pub(super) fn observe_mouse_protocol(chunk: &[u8], state: &AtomicU8) {
    const MODE_SEQUENCE_LEN: usize = 8;
    if chunk.len() < MODE_SEQUENCE_LEN {
        return;
    }
    let mut value = state.load(Ordering::SeqCst);
    for offset in 0..=chunk.len() - MODE_SEQUENCE_LEN {
        match &chunk[offset..offset + MODE_SEQUENCE_LEN] {
            b"\x1b[?1000h" => value |= MOUSE_TRACKING_ENABLED,
            b"\x1b[?1000l" => value &= !MOUSE_TRACKING_ENABLED,
            b"\x1b[?1006h" => value |= SGR_MOUSE_ENCODING_ENABLED,
            b"\x1b[?1006l" => value &= !SGR_MOUSE_ENCODING_ENABLED,
            _ => continue,
        }
    }
    state.store(value, Ordering::SeqCst);
}

pub(super) fn observe_mouse_protocol_stream(chunk: &[u8], tail: &mut Vec<u8>, state: &AtomicU8) {
    const MODE_SEQUENCE_LEN: usize = 8;
    tail.extend_from_slice(chunk);
    observe_mouse_protocol(tail, state);
    if tail.len() >= MODE_SEQUENCE_LEN {
        tail.drain(..tail.len() - (MODE_SEQUENCE_LEN - 1));
    }
}

pub(super) fn mouse_protocol(state: &AtomicU8) -> MouseProtocol {
    let value = state.load(Ordering::SeqCst);
    if value & MOUSE_TRACKING_ENABLED == 0 {
        MouseProtocol::None
    } else if value & SGR_MOUSE_ENCODING_ENABLED != 0 {
        MouseProtocol::Sgr
    } else {
        MouseProtocol::X10
    }
}

pub(super) fn mouse_click_bytes(protocol: MouseProtocol, row: u16, col: u16) -> Vec<u8> {
    let (c, r) = (col + 1, row + 1);
    match protocol {
        MouseProtocol::None => Vec::new(),
        MouseProtocol::X10 => vec![
            0x1b,
            b'[',
            b'M',
            32,
            u8::try_from(c + 32).unwrap_or(u8::MAX),
            u8::try_from(r + 32).unwrap_or(u8::MAX),
            0x1b,
            b'[',
            b'M',
            35,
            u8::try_from(c + 32).unwrap_or(u8::MAX),
            u8::try_from(r + 32).unwrap_or(u8::MAX),
        ],
        MouseProtocol::Sgr => format!("\x1b[<0;{c};{r}M\x1b[<0;{c};{r}m").into_bytes(),
    }
}

/// Send one mouse click (press + release) at a 0-based (row, col) cell using
/// the encoding the app requested from the terminal.
fn send_mouse_click(
    writer: &Arc<Mutex<Box<dyn Write + Send>>>,
    state: &AtomicU8,
    row: u16,
    col: u16,
) {
    if let Ok(mut w) = writer.lock() {
        // Re-read under the write lock so a disable observed after hotspot
        // discovery suppresses the pending click.
        let bytes = mouse_click_bytes(mouse_protocol(state), row, col);
        let _ = w.write_all(&bytes);
        let _ = w.flush();
    }
}

/// Signal B driver: relaunch the app and click each
/// deterministic hotspot from the start screen, recording any state a click
/// reaches that NO keystroke did (a mouse-only / not-keyboard-operable
/// control). Deterministic: hotspots are scanned in a fixed order and clicked
/// once each, from a freshly relaunched start state per click so clicks don't
/// compound.
pub(super) fn mouse_probe(
    cmdline: &str,
    seen: &mut BTreeSet<String>,
    keyboard_reached: &BTreeSet<String>,
    gt: &mut Groundtruth,
) {
    // Cap on hotspots clicked, so a button-dense screen can't blow the budget.
    const MOUSE_BUDGET: usize = 12;
    emit("JOURNEY[a] step: mouse probe (Signal B) enabled");
    // Find the start-screen hotspots once (from a throwaway session), then click
    // each from its own fresh relaunch so the click is the ONLY input.
    let Ok((master0, mut child0, parser0, _w0, _e0, mouse0)) = spawn_session(cmdline) else {
        return;
    };
    std::thread::sleep(Duration::from_millis(900));
    let start_sig = snapshot(&parser0).0;
    let hotspots = mouse_hotspots(&parser0, MOUSE_BUDGET);
    let protocol = mouse_protocol(&mouse0);
    let _ = child0.kill();
    drop(master0);
    if hotspots.is_empty() {
        return;
    }
    for hs in &hotspots {
        let Ok((master, mut child, parser, writer, _erases, mouse)) = spawn_session(cmdline) else {
            continue;
        };
        std::thread::sleep(Duration::from_millis(900));
        let session_protocol = mouse_protocol(&mouse);
        if session_protocol == MouseProtocol::None || session_protocol != protocol {
            let _ = child.kill();
            drop(master);
            continue;
        }
        send_mouse_click(&writer, &mouse, hs.row, hs.col);
        std::thread::sleep(Duration::from_millis(300));
        if looks_crashed(&parser) {
            // A click that crashes the app is a finding, but Signal B is about
            // operability gaps, not the crash oracle; leave crash reporting to
            // the keyboard pass and just move on.
            let _ = child.kill();
            drop(master);
            continue;
        }
        let (sig, _fp, _labels) = snapshot(&parser);
        // Register a never-before-seen state so the engine knows the node, then
        // decide: a state the click reached that NO keystroke reached, and that
        // differs from the start screen, is mouse-only.
        // LIFECYCLE-metamorphic oracles (rotation, background-restore) are NOT
        // ported to the TUI backend: a terminal has no device orientation to
        // rotate (a PTY resize is not an orientation change) and no app-lifecycle
        // background/foreground transition (a TUI process is not sent to the
        // background and resumed the way a mobile/desktop app is), so the ground
        // truth those oracles assert cannot be produced here.
        if seen.insert(sig.clone()) {
            let labels = snapshot(&parser).2;
            let payload = serde_json::json!({
                "sig": sig,
                "labels": labels,
                "elements": structural_input_elements()
            });
            emit(&format!("EXPLORE:STATE {payload}"));
        }
        if sig != start_sig && !keyboard_reached.contains(&sig) {
            // The control on the START screen at this hotspot is mouse-only: it
            // led somewhere the keyboard never did. Emit it on the start sig.
            gt.record(
                &start_sig,
                GtElement {
                    id: hs.id.clone(),
                    gesture_kind: "mouse",
                    keyboard_operable: false,
                },
            );
        }
        let _ = child.kill();
        drop(master);
    }
}
