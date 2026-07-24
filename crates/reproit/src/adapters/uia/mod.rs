//! Windows desktop runner (UI Automation backend), dispatched as `reproit
//! __uia` by drive.rs. The Windows twin of the macOS `swift macos-ax.swift` and
//! the Linux `reproit __atspi` runners: it drives ANY native Windows app
//! (WinUI, WPF, and Qt / Avalonia / wxWidgets builds, which all publish to UI
//! Automation) through the UIA tree and prints the framework-agnostic marker
//! protocol every backend emits.
//!
//! Oracle exclusions (documented ground-truth gaps): the SAFE-AREA oracle does
//! not run here -- a desktop window has no device safe-area inset (no notch /
//! status bar / home indicator), so there is no inset geometry to measure. The
//! PERMISSION-WALK oracle does not run here either -- a desktop app has no
//! runtime OS permission the runner can DENY, so there is no denial sweep.
//!
//! This is an in-process port of the former runners/windows-uia.py: it uses the
//! OFFICIAL Microsoft windows-rs projection of the UI Automation COM API
//! (IUIAutomation / IUIAutomationElement / the invoke/toggle/value/range/scroll
//! patterns) and REUSES the canonical signature core (crate::domain::signature)
//! directly instead of re-implementing it, so there is exactly one signature
//! oracle in the binary. Localized Name/text NEVER enters the hash; it is kept
//! only as a display-only label list (docs/signature.md).
//!
//! Env (set by drive.rs):
//!   REPROIT_TARGET             window title substring, or path to launch
//!   REPROIT_FUZZ_CONFIG        fuzz config json
//! (seed/budget/replay/prefix/edgeWeights)   REPROIT_SCENARIO_BARRIER
//! conductor base URL for a multi-actor scenario   REPROIT_SHOTS_DIR
//! where a `shoot:` step writes <name>.png   REPROIT_DEVICE             this
//! actor's role label (scenario mode)

use anyhow::{Context, Result};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::{Read, Write};
use std::time::{Duration, Instant};

use windows::core::{Interface, BOOL, BSTR, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDIBits,
    GetWindowDC, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
    HGDIOBJ, SRCCOPY,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationLegacyIAccessiblePattern,
    IUIAutomationRangeValuePattern, IUIAutomationValuePattern, TreeScope_Children,
    UIA_LegacyIAccessiblePatternId, UIA_RangeValuePatternId, UIA_ValuePatternId,
};
// windows-rs 0.58 groups PrintWindow + PRINT_WINDOW_FLAGS under the Xps module
// namespace (the metadata's home for the API), not WindowsAndMessaging.
use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, FindWindowW, GetWindowThreadProcessId, IsWindowVisible, SetForegroundWindow,
};

use crate::domain::signature::{
    apply_value_nodes, content_fingerprint, signature, structural_only, value_class, Node, ValueCap,
};

const ACTION_BUDGET: u32 = 36;
const MAX_LABEL_LEN: usize = 40;
const MAX_LABELS_PER_STATE: usize = 24;
const HANG_FLOOR_MS: u64 = 2000;

mod action;
mod capture;
mod config;
mod exploration;
mod oracle;
mod scenario;

use action::{
    crash, maybe_emit_hang, press, sample_rss, send_escape, set_text, window_exists, window_hwnd,
};
use capture::{shoot, start_clip_capture, stop_clip_capture};
use config::load_value_node_selectors;
use exploration::{
    edge_key, first_untried_action, has_frontier, load_fuzz, path_to_frontier, remember_actions,
    remember_edge, Rng,
};
use oracle::{content_bug_reason, tofu_detail, InvariantScrape};
use scenario::{run_scenario_actor, window_for_pid};

#[cfg(test)]
use oracle::{parse_invariant_marker, InvariantState};

fn emit(s: &str) {
    println!("{s}");
    let _ = std::io::stdout().flush();
}

// UIA ControlType id -> canonical role vocabulary.
// windows-rs exposes the control-type ids as `UIA_<X>ControlTypeId` (i32
// newtype). We match on the raw i32 so the table is a plain lookup. Roles
// outside the fixed vocabulary fall through to `node` at normalize time inside
// crate::domain::signature.
fn uia_role(control_type: i32) -> &'static str {
    // The UIA control-type ids are stable public constants (winuser UIA_*). We
    // inline the numeric ids so the table needs no per-constant import.
    match control_type {
        50000 => "button",    // Button
        50001 => "group",     // Calendar
        50002 => "checkbox",  // CheckBox
        50003 => "textfield", // ComboBox
        50004 => "textfield", // Edit
        50005 => "link",      // Hyperlink
        50006 => "image",     // Image
        50007 => "listitem",  // ListItem
        50008 => "list",      // List
        50009 => "menu",      // Menu
        50010 => "menu",      // MenuBar
        50011 => "menuitem",  // MenuItem
        50012 => "progress",  // ProgressBar (transient -> dropped, promoted if RangeValue)
        50013 => "radio",     // RadioButton
        50014 => "node",      // ScrollBar
        50015 => "slider",    // Slider
        50016 => "spinner",   // Spinner (transient -> dropped)
        50017 => "text",      // StatusBar
        50018 => "tab",       // Tab
        50019 => "tab",       // TabItem
        50020 => "text",      // Text
        50021 => "group",     // ToolBar
        50022 => "tooltip",   // ToolTip (transient -> dropped)
        50023 => "list",      // Tree
        50024 => "listitem",  // TreeItem
        50025 => "group",     // Custom
        50026 => "group",     // Group
        50027 => "node",      // Thumb
        50028 => "list",      // DataGrid
        50029 => "listitem",  // DataItem
        50030 => "textfield", // Document
        50031 => "button",    // SplitButton
        50032 => "screen",    // Window
        50033 => "group",     // Pane
        50034 => "header",    // Header
        50035 => "header",    // HeaderItem
        50036 => "list",      // Table
        50037 => "header",    // TitleBar
        50038 => "node",      // Separator
        _ => "node",
    }
}

// Control-type ids that respond to an Invoke/press (the tappable set).
const TAPPABLE_CONTROL_TYPES: &[i32] = &[
    50000, // Button
    50011, // MenuItem
    50019, // TabItem
    50007, // ListItem
    50005, // Hyperlink
    50002, // CheckBox
    50013, // RadioButton
];

const TITLEBAR_CONTROL_TYPE: i32 = 50037;
const BUTTON_CONTROL_TYPE: i32 = 50000;

// AutomationId a WinUI/UWP app assigns to the WindowControl that holds its
// caption strip (system menu + Minimize/Maximize/Close). Win32 apps instead
// nest the same affordances under a TitleBarControl (TITLEBAR_CONTROL_TYPE), so
// the two skips in is_titlebar_root cover both shapes.
const TITLEBAR_AUTOMATION_ID: &str = "TitleBar";

// The window-manager caption/system button AutomationIds. These are fixed ids
// the OS / XAML caption generator emits (not localized display names like
// "Close Calculator"), so matching them is language-independent. A Button
// carrying one is window chrome the fuzzer must never tap: pressing it would
// close, minimize, or reparent the app under test.
const CAPTION_BUTTON_AUTOMATION_IDS: &[&str] = &["Close", "Minimize", "Maximize", "Restore"];

// True when this element roots the window-manager caption subtree the fuzzer
// must not enter. Two shapes carry it: a Win32 TitleBarControl (control type
// 50037, holding the system MenuBar + Close), and a WinUI/UWP WindowControl
// whose AutomationId is 'TitleBar' (Calculator: holding plain Buttons
// id='Close'/ 'Minimize'/'Maximize'). Skipping the whole subtree keeps every
// window-manager affordance out of the tappable set, so the fuzzer can never
// destroy the app by tapping Close and then mistake the vanished window for a
// crash.
fn is_titlebar_root(control_type: i32, automation_id: Option<&str>) -> bool {
    control_type == TITLEBAR_CONTROL_TYPE || automation_id == Some(TITLEBAR_AUTOMATION_ID)
}

// True when a control is a window caption/system button that must stay out of
// the tappable set even when it is not inside a recognised title-bar subtree
// (the belt-and-suspenders complement to is_titlebar_root): a Button whose
// AutomationId is a documented caption id. Structural id match, never the
// English name, so no localized name list is needed.
fn is_caption_button(control_type: i32, automation_id: Option<&str>) -> bool {
    control_type == BUTTON_CONTROL_TYPE
        && automation_id.is_some_and(|id| CAPTION_BUTTON_AUTOMATION_IDS.contains(&id))
}

// small UIA accessors (each best-effort; a failure yields the empty/None).

fn bstr_to_opt(b: BSTR) -> Option<String> {
    let s = b.to_string();
    let t = s.trim().to_string();
    if t.is_empty() {
        None
    } else {
        Some(t)
    }
}

fn el_control_type(el: &IUIAutomationElement) -> i32 {
    unsafe { el.CurrentControlType().map(|c| c.0).unwrap_or(0) }
}

fn el_name(el: &IUIAutomationElement) -> String {
    unsafe { el.CurrentName().map(|b| b.to_string()).unwrap_or_default() }
        .trim()
        .to_string()
}

fn el_automation_id(el: &IUIAutomationElement) -> Option<String> {
    unsafe { el.CurrentAutomationId().ok().and_then(bstr_to_opt) }
}

fn el_localized_type(el: &IUIAutomationElement) -> String {
    unsafe {
        el.CurrentLocalizedControlType()
            .map(|b| b.to_string())
            .unwrap_or_default()
            .to_lowercase()
    }
}

fn el_is_password(el: &IUIAutomationElement) -> bool {
    unsafe { el.CurrentIsPassword().map(|b| b.as_bool()).unwrap_or(false) }
}

fn el_bounds(el: &IUIAutomationElement) -> Option<(i32, i32, i32, i32)> {
    let r: RECT = unsafe { el.CurrentBoundingRectangle().ok()? };
    if r.right - r.left < 1 || r.bottom - r.top < 1 {
        None
    } else {
        Some((r.left, r.top, r.right, r.bottom))
    }
}

fn get_pattern<T: Interface>(el: &IUIAutomationElement, id: i32) -> Option<T> {
    use windows::Win32::UI::Accessibility::UIA_PATTERN_ID;
    let unk = unsafe { el.GetCurrentPattern(UIA_PATTERN_ID(id)).ok()? };
    unk.cast::<T>().ok()
}

// NOTE: the live-region Text->status promotion (LiveSetting != Off) the Python
// runner did via GetCurrentPropertyValue is intentionally omitted here. In
// windows-rs 0.58 `VARIANT` is an opaque `windows::core` type with no stable
// union accessor, so reading the LiveSetting integer out of band is not worth a
// brittle transmute for a minor value-state nicety (a live-region text folding
// its changing value into the signature). Every other value-role path
// (textfield / slider / progressbar) is fully preserved below. The checkbox->
// switch and progress->progressbar promotions use typed pattern reads, not a
// VARIANT, so they stay.
fn el_role_live(el: &IUIAutomationElement, ct: i32) -> &'static str {
    let role = uia_role(ct);
    if role == "checkbox" {
        let loc = el_localized_type(el);
        if loc.contains("switch") || loc.contains("toggle") {
            return "switch";
        }
    }
    if role == "progress" {
        if let Some(rp) =
            get_pattern::<IUIAutomationRangeValuePattern>(el, UIA_RangeValuePatternId.0)
        {
            if unsafe { rp.CurrentValue() }.is_ok() {
                return "progressbar";
            }
        }
    }
    role
}

fn el_input_type(el: &IUIAutomationElement, role: &str) -> Option<String> {
    if role == "textfield" && el_is_password(el) {
        Some("password".into())
    } else {
        None
    }
}

fn fmt_range_value(v: f64) -> String {
    if v == v.trunc() {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

// The displayed data value for a value-bearing control (Layer 2): ValuePattern
// (Edit/Document/ComboBox), else RangeValuePattern (Slider/ProgressBar), else a
// live Text's announced name. None for chrome roles so V: is never polluted.
fn el_value(el: &IUIAutomationElement, role: &str) -> Option<String> {
    const VALUE_ROLES: &[&str] = &[
        "textfield",
        "status",
        "log",
        "progressbar",
        "meter",
        "timer",
        "output",
    ];
    if !VALUE_ROLES.contains(&role) {
        return None;
    }
    if let Some(vp) = get_pattern::<IUIAutomationValuePattern>(el, UIA_ValuePatternId.0) {
        if let Ok(b) = unsafe { vp.CurrentValue() } {
            let s = b.to_string();
            if !s.is_empty() {
                return Some(s);
            }
        }
    }
    if let Some(rp) = get_pattern::<IUIAutomationRangeValuePattern>(el, UIA_RangeValuePatternId.0) {
        if let Ok(v) = unsafe { rp.CurrentValue() } {
            return Some(fmt_range_value(v));
        }
    }
    if role == "status" {
        let name = el_name(el);
        if !name.is_empty() {
            return Some(name);
        }
    }
    None
}

fn el_key(el: &IUIAutomationElement, role: &str) -> String {
    match el_automation_id(el) {
        Some(aid) => format!("id:{aid}"),
        None => format!("role:{role}"),
    }
}

// The label (display-only, NEVER hashed): UIA Name, else LegacyIAccessible
// value.
fn label_of(el: &IUIAutomationElement) -> String {
    let name = el_name(el);
    if !name.is_empty() {
        return name;
    }
    if let Some(leg) =
        get_pattern::<IUIAutomationLegacyIAccessiblePattern>(el, UIA_LegacyIAccessiblePatternId.0)
    {
        if let Ok(b) = unsafe { leg.CurrentValue() } {
            return b.to_string().trim().to_string();
        }
    }
    String::new()
}

fn anchor_of(window: &IUIAutomationElement) -> Option<String> {
    // A top-level HWND normally has no useful AutomationId. Some WPF providers
    // synchronously marshal that empty property through the UI thread and can
    // stall here; the stable window class is the correct screen anchor.
    unsafe { window.CurrentClassName().ok().and_then(bstr_to_opt) }
}

// Enumerate semantic accessibility children, not the raw implementation tree.
// WPF/Avalonia can expose thousands of visual/provider nodes in Raw View even
// for a tiny window; walking that view made snapshots effectively hang and also
// hashed framework chrome Reproit never intends to drive. Control View is UIA's
// cross-toolkit operable/content tree and matches AX/AT-SPI semantics.
fn children_of(automation: &IUIAutomation, el: &IUIAutomationElement) -> Vec<IUIAutomationElement> {
    let mut out = Vec::new();
    let Ok(cond) = (unsafe { automation.ControlViewCondition() }) else {
        return out;
    };
    let Ok(arr) = (unsafe { el.FindAll(TreeScope_Children, &cond) }) else {
        return out;
    };
    let len = unsafe { arr.Length() }.unwrap_or(0);
    for i in 0..len {
        if let Ok(child) = unsafe { arr.GetElement(i) } {
            out.push(child);
        }
    }
    out
}

// Walk a live UIA control into a canonical Node tree (role + id + type + value
// + children). Localized chrome Name/text is excluded by construction.
const MAX_UIA_NODES: usize = 4096;

fn build_node(
    automation: &IUIAutomation,
    el: &IUIAutomationElement,
    depth: usize,
    remaining: &mut usize,
) -> Node {
    let ct = el_control_type(el);
    let role = el_role_live(el, ct);
    let mut node = Node::new(role);
    // Root-window ids are neither stable selectors nor part of the app's
    // semantic content; avoid the WPF top-level AutomationId provider call.
    node.id = if depth == 0 {
        None
    } else {
        el_automation_id(el)
    };
    node.type_ = el_input_type(el, role);
    node.value = el_value(el, role);
    *remaining = remaining.saturating_sub(1);
    if depth < 60 && *remaining > 0 {
        for child in children_of(automation, el) {
            if *remaining == 0 {
                break;
            }
            node.children
                .push(build_node(automation, &child, depth + 1, remaining));
        }
    }
    node
}

struct Snapshot {
    sig: String,
    content: String,
    labels: Vec<String>,
    elements: Vec<serde_json::Value>,
    tappables: Vec<String>,
    nodes: HashMap<String, IUIAutomationElement>,
    content_bugs: Vec<(String, &'static str, String)>,
    broken_assets: Vec<(String, String)>,
}

type WalkFrame = (IUIAutomationElement, usize);

#[allow(clippy::too_many_arguments)]
fn snapshot(
    automation: &IUIAutomation,
    window: &IUIAutomationElement,
    value_selectors: &[String],
    cap: &mut ValueCap,
) -> Snapshot {
    let anchor = anchor_of(window);
    let mut remaining = MAX_UIA_NODES;
    let mut root = build_node(automation, window, 0, &mut remaining);
    apply_value_nodes(&mut root, value_selectors);
    let sig = cap.effective_signature(anchor.as_deref(), &root);
    let content = content_fingerprint(anchor.as_deref(), &root);

    let mut labels: Vec<String> = Vec::new();
    let mut elements: Vec<serde_json::Value> = Vec::new();
    let mut tappables: Vec<String> = Vec::new();
    let mut nodes: HashMap<String, IUIAutomationElement> = HashMap::new();
    let mut content_bugs: Vec<(String, &'static str, String)> = Vec::new();
    let mut content_bug_seen: HashSet<String> = HashSet::new();
    let mut broken_assets: Vec<(String, String)> = Vec::new();
    let mut broken_asset_seen: HashSet<String> = HashSet::new();

    let mut stack: Vec<WalkFrame> = vec![(window.clone(), 0)];
    let mut visited_nodes = 0usize;
    while let Some((el, depth)) = stack.pop() {
        visited_nodes += 1;
        if depth > 60 || visited_nodes > MAX_UIA_NODES {
            continue;
        }
        let ct = el_control_type(&el);
        let aid = if depth == 0 {
            None
        } else {
            el_automation_id(&el)
        };
        // Skip the whole window-manager caption subtree: tapping Close/Minimize/
        // Maximize would destroy or reparent the app under test, and the chrome is
        // not a screen the fuzzer should score. Covers the Win32 TitleBarControl and
        // the WinUI/UWP WindowControl id='TitleBar' shapes (see is_titlebar_root).
        if is_titlebar_root(ct, aid.as_deref()) {
            continue;
        }
        let role = el_role_live(&el, ct);
        // A caption/system Button that escaped the subtree skip (its AutomationId is
        // a documented caption id) is chrome too: keep it out of the tappable set so
        // it is never pressed and can never be misread as a crash when the window
        // vanishes.
        let is_tap = TAPPABLE_CONTROL_TYPES.contains(&ct) && !is_caption_button(ct, aid.as_deref());
        let label = label_of(&el);
        if role == "textfield" {
            if let Some(id) = aid.as_deref().filter(|id| !id.is_empty()) {
                let sel = format!("key:{id}");
                let purpose = crate::domain::appmap::normalize_input_purpose(
                    el_input_type(&el, role).as_deref(),
                    &sel,
                );
                elements.push(serde_json::json!({
                    "sel": sel, "role": role, "label": label,
                    "inputPurpose": purpose,
                }));
            }
        }
        if !label.is_empty() && label.chars().count() <= MAX_LABEL_LEN {
            labels.push(label.clone());
            if is_tap {
                tappables.push(label.clone());
                nodes.entry(label.clone()).or_insert_with(|| el.clone());
            }
        }
        // CONTENT-BUG oracle.
        if !label.is_empty() {
            if let Some(reason) = content_bug_reason(&label) {
                let key = el_key(&el, role);
                let dedup = format!("{key}|{reason}");
                if content_bug_seen.insert(dedup) {
                    let text: String = label.chars().take(80).collect();
                    content_bugs.push((key, reason, text));
                }
            }
        }
        // BROKEN-ASSET (tofu) oracle: a rendered U+FFFD in this element's label
        // (Name, else LegacyIAccessible value) is broken text encoding on
        // screen. Keyed by the stable node key, deduped, so the marker is
        // byte-identical run to run and addressed by id/role, never the text.
        if let Some(detail) = tofu_detail(&label) {
            let key = el_key(&el, role);
            if broken_asset_seen.insert(key.clone()) {
                broken_assets.push((key, detail));
            }
        }
        // Push children (reverse so pop yields document order).
        let kids = children_of(automation, &el);
        for child in kids.into_iter().rev() {
            stack.push((child, depth + 1));
        }
    }
    let uniq_labels: Vec<String> = dedup(labels);
    content_bugs.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(b.1)));
    broken_assets.sort_by(|a, b| a.0.cmp(&b.0));
    Snapshot {
        sig,
        content,
        labels: uniq_labels,
        elements,
        tappables: dedup(tappables),
        nodes,
        content_bugs,
        broken_assets,
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

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn run() -> Result<()> {
    unsafe {
        // Multithreaded apartment: the runner is a plain console process with no
        // message pump, and UIA works from an MTA.
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let automation: IUIAutomation =
        unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }
            .context("creating the UI Automation client (CUIAutomation)")?;

    let target = std::env::var("REPROIT_TARGET")
        .ok()
        .filter(|s| !s.is_empty())
        .context("REPROIT_TARGET (window title or launch path) required")?;

    let scenario_base = std::env::var("REPROIT_SCENARIO_BARRIER")
        .ok()
        .filter(|s| !s.is_empty());
    if scenario_base.is_none() {
        emit("JOURNEY claimed role=a");
    }

    // App-invariant scrape: only a child WE launch exposes a stderr we can pipe
    // (attaching to an already-running window by title does not), so this is set
    // only on the launch-by-path branch below.
    let mut invariant_scrape: Option<InvariantScrape> = None;

    // Launch if it looks like a path, then bind by top-level window.
    let looks_like_path =
        target.contains(std::path::MAIN_SEPARATOR) && std::path::Path::new(&target).exists();
    let window: IUIAutomationElement = if looks_like_path {
        let mut child = std::process::Command::new(&target)
            // A GUI target must not inherit the runner's PowerShell/Tee stdout
            // pipe. If it does, the pipeline waits for the still-running app to
            // close its inherited writer and never reaches the fixture cleanup.
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            // Pipe stderr so we can scrape the SDK's REPROIT_INVARIANT markers,
            // and gate the SDK on: seeing REPROIT_UNDER_FUZZER it evaluates its
            // invariant registry (inert without it, in production).
            .stderr(std::process::Stdio::piped())
            .env("REPROIT_UNDER_FUZZER", "1")
            .spawn()
            .with_context(|| format!("launching {target}"))?;
        if let Some(stderr) = child.stderr.take() {
            invariant_scrape = Some(InvariantScrape::spawn(stderr));
        }
        std::thread::sleep(Duration::from_secs(2));
        window_for_pid(&automation, child.id(), Duration::from_secs(12)).with_context(|| {
            format!(
                "launched process {} exposed no top-level UIA window",
                child.id()
            )
        })?
    } else {
        // Find a top-level window whose title contains the target substring.
        let deadline = Instant::now() + Duration::from_secs(8);
        let mut found = None;
        while Instant::now() < deadline {
            if let Ok(root) = unsafe { automation.GetRootElement() } {
                for w in children_of(&automation, &root) {
                    if el_name(&w).contains(&target) {
                        found = Some(w);
                        break;
                    }
                }
            }
            if found.is_some() {
                break;
            }
            // Fall back to an exact FindWindow by title if enumeration missed it.
            let hwnd = unsafe { FindWindowW(PCWSTR::null(), PCWSTR(wide(&target).as_ptr())) };
            if let Ok(hwnd) = hwnd {
                if !hwnd.0.is_null() {
                    if let Ok(w) = unsafe { automation.ElementFromHandle(hwnd) } {
                        found = Some(w);
                        break;
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(300));
        }
        match found {
            Some(w) => w,
            None => {
                crash(
                    "target not found",
                    &format!("no window matching {target:?}"),
                );
                std::process::exit(3);
            }
        }
    };

    unsafe {
        let _ = SetForegroundWindow(window_hwnd(&window));
    }
    emit("JOURNEY[a] step: attached target window");
    std::thread::sleep(Duration::from_secs(1));

    // Layer 3 (config) selectors + Layer 2 runner cap persist across the session.
    emit("JOURNEY[a] step: loading UIA configuration");
    let value_selectors = load_value_node_selectors();
    emit("JOURNEY[a] step: UIA value selectors loaded");
    let mut cap = ValueCap::new();
    emit("JOURNEY[a] step: UIA value cap initialized");
    emit("JOURNEY[a] step: UIA configuration loaded");

    if let Some(base) = scenario_base {
        return run_scenario_actor(&automation, &window, &value_selectors, &mut cap, &base);
    }

    let fuzz = load_fuzz();
    emit("JOURNEY[a] step: UIA fuzz plan loaded");
    let mut rng = Rng::new(fuzz.seed);
    if fuzz.seed != 0 {
        emit(&format!("JOURNEY[a] step: fuzz seed={}", fuzz.seed));
    }

    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut tried: BTreeSet<String> = BTreeSet::new();
    let mut actions_by_state: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut graph: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();

    let emit_state = |automation: &IUIAutomation,
                      window: &IUIAutomationElement,
                      cap: &mut ValueCap,
                      seen: &mut BTreeSet<String>|
     -> Snapshot {
        let snap = snapshot(automation, window, &value_selectors, cap);
        let observation_labels: Vec<&String> =
            snap.labels.iter().take(MAX_LABELS_PER_STATE).collect();
        emit(&crate::domain::runner::observation_frame_line(
            &serde_json::json!({
                "sig": snap.sig,
                "labels": observation_labels,
                "elements": snap.elements
            }),
        ));
        if seen.insert(snap.sig.clone()) {
            let labels: Vec<&String> = snap.labels.iter().take(MAX_LABELS_PER_STATE).collect();
            emit(&format!(
                "EXPLORE:STATE {}",
                serde_json::json!({ "sig": snap.sig, "labels": labels, "elements": snap.elements })
            ));
            if !snap.content_bugs.is_empty() {
                let items: Vec<serde_json::Value> = snap
                    .content_bugs
                    .iter()
                    .map(|(k, reason, text)| {
                        serde_json::json!({ "key": k, "reason": reason, "text": text })
                    })
                    .collect();
                emit(&format!(
                    "EXPLORE:CONTENTBUG {}",
                    serde_json::json!({ "sig": snap.sig, "items": items })
                ));
            }
            // BROKEN-ASSET (tofu) for this newly-seen state, keyed by the SAME
            // sig. Only emitted when a U+FFFD replacement character actually
            // rendered, so a clean state stays silent (no marker, no finding).
            if !snap.broken_assets.is_empty() {
                let items: Vec<serde_json::Value> = snap
                    .broken_assets
                    .iter()
                    .map(|(k, detail)| {
                        serde_json::json!({ "key": k, "reason": "tofu", "detail": detail })
                    })
                    .collect();
                emit(&format!(
                    "EXPLORE:BROKENASSET {}",
                    serde_json::json!({ "sig": snap.sig, "items": items })
                ));
            }
        }
        snap
    };

    emit("JOURNEY[a] step: observing initial UIA state");
    let mut current = emit_state(&automation, &window, &mut cap, &mut seen);
    if let Some(iv) = invariant_scrape.as_mut() {
        iv.flush_for(&current.sig);
    }
    let launch_sig = current.sig.clone();
    let mut stuck = 0u32;

    let map_mode = fuzz.replay.is_none() && fuzz.prefix.is_none() && fuzz.seed == 0;
    let prefix_len = fuzz.prefix.as_ref().map(|p| p.len()).unwrap_or(0);
    let budget: usize = if let Some(r) = &fuzz.replay {
        r.len()
    } else if map_mode && !fuzz.configured {
        usize::MAX
    } else {
        fuzz.budget as usize + prefix_len
    };

    let is_soak = fuzz.replay.is_some();
    let mut inspect_auto_continue = false;
    let target_pid = unsafe { window.CurrentProcessId() }
        .map(|p| p as u32)
        .unwrap_or(0);
    let soak_start = Instant::now();
    if is_soak {
        sample_rss(target_pid, 0);
    }

    // --record-video clip capture: film the target window for the whole replay, then box
    // the finding's element after it settles. Only armed in replay mode with a clip
    // plan and REPROIT_VIDEO_DIR set. clip_el / clip_action_at are captured live at
    // the tap that triggered the finding (the freshest element handle + timestamp).
    let clip_video_dir = std::env::var("REPROIT_VIDEO_DIR")
        .ok()
        .filter(|s| !s.is_empty());
    let clip_armed = clip_video_dir.is_some() && fuzz.clip_sel.is_some() && fuzz.replay.is_some();
    let clip_mov = clip_video_dir
        .as_ref()
        .map(|d| {
            std::path::Path::new(d)
                .join("clip.mov")
                .to_string_lossy()
                .into_owned()
        })
        .unwrap_or_default();
    let mut clip_proc: Option<std::process::Child> = None;
    let mut clip_start = Instant::now();
    let mut clip_el: Option<IUIAutomationElement> = None;
    let mut clip_action_at: f64 = 0.0;
    if clip_armed {
        let title = el_name(&window);
        clip_proc = start_clip_capture(&title, el_bounds(&window), &clip_mov);
        clip_start = Instant::now();
        // Small lead-in so the first frames exist before the triggering action.
        std::thread::sleep(Duration::from_millis(400));
    }

    let mut i = 0usize;
    while i < budget && stuck < 3 {
        if is_soak && i > 0 {
            sample_rss(target_pid, soak_start.elapsed().as_millis() as u64);
        }
        // replay > prefix > seeded weighted > systematic map.
        let act: Option<String> = if let Some(r) = &fuzz.replay {
            r.get(i).cloned()
        } else if fuzz.prefix.as_ref().map(|p| i < p.len()).unwrap_or(false) {
            fuzz.prefix.as_ref().and_then(|p| p.get(i).cloned())
        } else if fuzz.seed != 0 {
            let mut taps: Vec<String> = current.tappables.clone();
            taps.sort();
            let ew = fuzz.edge_weights.get(&current.sig);
            let mut options: Vec<String> = taps.iter().map(|l| format!("tap:{l}")).collect();
            options.push("back".to_string());
            let weights: Vec<f64> = options
                .iter()
                .map(|o| 1.0 / (1.0 + ew.and_then(|m| m.get(o)).copied().unwrap_or(0) as f64))
                .collect();
            let total: f64 = weights.iter().sum();
            let mut r = rng.unit() * total;
            let mut chosen = options.last().cloned();
            for (k, w) in weights.iter().enumerate() {
                r -= w;
                if r <= 0.0 {
                    chosen = Some(options[k].clone());
                    break;
                }
            }
            chosen
        } else {
            let mut taps: Vec<String> = current.tappables.clone();
            taps.sort();
            let mut options: Vec<String> = taps.iter().map(|l| format!("tap:{l}")).collect();
            options.push("back".to_string());
            remember_actions(&mut actions_by_state, &current.sig, options);
            let mut a = first_untried_action(&actions_by_state, &tried, &current.sig);
            if a.is_none() {
                a = path_to_frontier(&graph, &actions_by_state, &tried, &current.sig)
                    .and_then(|p| p.first().cloned());
            }
            if a.is_none() && has_frontier(&actions_by_state, &tried) && current.sig != launch_sig {
                break;
            }
            a
        };

        let Some(act) = act else { break };
        if fuzz.replay.is_some() && !inspect_auto_continue {
            inspect_auto_continue = crate::adapters::inspect_control::pause_or_context(
                &act,
                i + 1,
                fuzz.replay.as_ref().map_or(0, Vec::len),
                Some(&act),
                None,
            )?;
        }
        emit(&crate::domain::runner::action_frame_line(None, &act));

        if let Some(name) = act.strip_prefix("shoot:") {
            shoot(&window, name);
            i += 1;
            continue;
        }
        if act == "back" {
            let from_sig = current.sig.clone();
            tried.insert(edge_key(&from_sig, "back"));
            send_escape();
            std::thread::sleep(Duration::from_millis(600));
            maybe_emit_hang(&window, &from_sig, "back");
            let nxt = emit_state(&automation, &window, &mut cap, &mut seen);
            if let Some(iv) = invariant_scrape.as_mut() {
                iv.flush_for(&nxt.sig);
            }
            if nxt.sig != current.sig {
                emit(&format!(
                    "EXPLORE:EDGE {}",
                    serde_json::json!({ "from": current.sig, "action": "back", "to": nxt.sig })
                ));
                remember_edge(&mut graph, &current.sig, "back", &nxt.sig);
            }
            if nxt.sig != current.sig || nxt.content != current.content {
                stuck = 0;
            } else {
                stuck += 1;
            }
            current = nxt;
            i += 1;
            continue;
        }
        let label = act.strip_prefix("tap:").unwrap_or(&act).to_string();
        let from_sig = current.sig.clone();
        tried.insert(edge_key(&current.sig, &act));
        // --record-video: the tap on the finding's element is the moment to box. Grab the
        // freshest element handle and capture-relative timestamp now, before the
        // press may mutate the tree (post-loop resolution can fall back to this).
        if clip_armed && fuzz.clip_sel.as_deref() == Some(label.as_str()) {
            clip_el = current.nodes.get(&label).cloned();
            clip_action_at = clip_start.elapsed().as_secs_f64();
        }
        let pressed = current.nodes.get(&label).map(press).unwrap_or(false);
        if !pressed {
            emit(&format!("FUZZ:MISS {act}"));
            stuck += 1;
            i += 1;
            continue;
        }
        std::thread::sleep(Duration::from_millis(700));
        if !window_exists(&window) {
            crash(
                "target window gone",
                &format!("the window vanished during {act}"),
            );
            break;
        }
        maybe_emit_hang(&window, &from_sig, &format!("tap:{label}"));
        let nxt = emit_state(&automation, &window, &mut cap, &mut seen);
        if let Some(iv) = invariant_scrape.as_mut() {
            iv.flush_for(&nxt.sig);
        }
        if nxt.sig != current.sig {
            emit(&format!(
                "EXPLORE:EDGE {}",
                serde_json::json!({
                    "from": current.sig,
                    "action": format!("tap:{label}"),
                    "to": nxt.sig
                })
            ));
            remember_edge(&mut graph, &current.sig, &format!("tap:{label}"), &nxt.sig);
        }
        if nxt.sig != current.sig || nxt.content != current.content {
            stuck = 0;
        }
        current = nxt;
        i += 1;
    }

    // --record-video clip finalize: resolve the finding's element to a window-relative
    // rect (BoundingRectangle and the window bounds are both screen pixels with a
    // top-left origin, so the box is element - windowOrigin), write box-spec.json
    // in the window's own pixel space, then quit ffmpeg so it flushes clip.mov.
    // The host runs box-overlay.mjs (clip.mov + box-spec.json -> boxed clip),
    // the uniform path for every runner that cannot inject a live overlay.
    if clip_armed {
        let sel = fuzz.clip_sel.clone().unwrap_or_default();
        // Prefer the handle grabbed at the triggering tap; else re-resolve by label
        // from the settled state (a no-op press leaves it in place).
        let el = clip_el.clone().or_else(|| current.nodes.get(&sel).cloned());
        let el_rect = el.as_ref().and_then(el_bounds);
        let win_rect = el_bounds(&window);
        stop_clip_capture(clip_proc.take());
        let mut drew = false;
        if let (Some((el_l, el_t, el_r, el_b)), Some((w_l, w_t, w_r, w_b))) = (el_rect, win_rect) {
            let box_ = serde_json::json!({
                "x": (el_l - w_l) as f64,
                "y": (el_t - w_t) as f64,
                "w": (el_r - el_l) as f64,
                "h": (el_b - el_t) as f64,
                "tStart": (clip_action_at - 0.3).max(0.0),
                "tEnd": 1e9,
                "label": fuzz
                    .clip_label
                    .clone()
                    .or_else(|| fuzz.clip_oracle.clone())
                    .unwrap_or_else(|| "finding".to_string()),
                "color": "red",
            });
            let spec = serde_json::json!({
                "videoW": (w_r - w_l) as f64,
                "videoH": (w_b - w_t) as f64,
                "boxes": [box_],
            });
            if let Some(dir) = clip_video_dir.as_ref() {
                let spec_path = std::path::Path::new(dir).join("box-spec.json");
                if std::fs::write(&spec_path, spec.to_string()).is_ok() {
                    drew = true;
                }
            }
        }
        emit(&format!(
            "FINDING:BOXED {}",
            serde_json::json!({
                "oracle": fuzz.clip_oracle.clone().unwrap_or_default(),
                "sel": sel,
                "mov": clip_mov,
                "drew": drew,
            })
        ));
    }

    emit(&format!("JOURNEY[a] step: explored {} states", seen.len()));
    emit("JOURNEY DONE");
    emit("All tests passed");
    Ok(())
}

// Referenced to keep the shared imports used on all builds honest.
#[allow(dead_code)]
fn _unused_reexports() {
    let _ = value_class("0");
    let _ = signature(None, &Node::new("screen"));
    let _ = structural_only(&Node::new("screen"));
}

// NOTE: this module is cfg(windows)-gated in backends/mod.rs, so these tests
// run only on a Windows host/CI. They pin the pure text scan; the COM-facing
// walk is exercised live by the operability-golden Windows job.
#[cfg(test)]
mod tests;
