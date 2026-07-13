//! Linux desktop runner (AT-SPI2 backend), dispatched as `reproit __atspi` by
//! drive.rs. The Linux twin of the macOS `swift macos-ax.swift` and the Windows
//! `reproit __uia` runners: it drives ANY native Linux app (GTK, Qt, and any
//! toolkit that publishes to AT-SPI) through the accessibility tree and prints the
//! framework-agnostic marker protocol every backend emits.
//!
//! Oracle exclusions (documented ground-truth gaps): the SAFE-AREA oracle does
//! not run here -- a desktop window has no device safe-area inset, so there is no
//! inset geometry to measure. The PERMISSION-WALK oracle does not run here either
//! -- a desktop app has no runtime OS permission the runner can DENY, so there is
//! no denial sweep.
//!
//! This is an in-process port of the former runners/linux-atspi.py. It binds the
//! OFFICIAL AT-SPI C library (libatspi.so.0, the exact library the Python `gi` /
//! `Atspi` binding wrapped) directly via hand-declared `#[link]` FFI, and REUSES
//! the canonical signature core (crate::signature) instead of re-implementing it,
//! so there is exactly one signature oracle in the binary. Localized name/text
//! NEVER enters the hash; it is kept only as a display-only label list.
//!
//! Env (set by drive.rs):
//!   REPROIT_TARGET             app name substring, or path to launch
//!   REPROIT_FUZZ_CONFIG        fuzz config json (single {seed,...} or {batch:[...]})
//!   REPROIT_SCENARIO_BARRIER   conductor base URL for a multi-actor scenario
//!   REPROIT_SHOTS_DIR          where a `shoot:` step writes <name>.png
//!   REPROIT_DEVICE             this actor's role label (scenario mode)

use anyhow::{Context, Result};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ffi::{CStr, CString};
use std::io::{Read, Write};
use std::os::raw::{c_char, c_int, c_long, c_void};
use std::ptr;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use regex::Regex;

use crate::signature::{
    apply_value_nodes, content_fingerprint, signature, structural_only, value_class, Node, ValueCap,
};

const ACTION_BUDGET: u32 = 36;
const MAX_LABEL_LEN: usize = 40;
const MAX_LABELS_PER_STATE: usize = 24;
const HANG_FLOOR_MS: u64 = 2000;

// AT-SPI enum constants (atspi-constants.h). Hardcoded numeric ids so the FFI
// needs no generated headers; these are stable ABI values. SHOWING == 25 (verify
// against the installed atspi-constants.h; 27 is STALE, a different state).
const ATSPI_STATE_SHOWING: c_int = 25;
const ATSPI_COORD_TYPE_SCREEN: c_int = 0;
const ATSPI_KEY_PRESSRELEASE: c_int = 2;
const XKEYCODE_ESCAPE: c_long = 9; // X11 keycode for Escape

// ── libatspi / glib FFI (the official C library, hand-declared) ─────────────
// Opaque GObjects are `*mut c_void`; every accessor that returns a new ref or a
// heap string is unref'd / g_free'd by the wrappers below, matching the ownership
// the Python `gi` binding managed via GC.

#[repr(C)]
#[derive(Clone, Copy)]
struct AtspiRect {
    x: c_int,
    y: c_int,
    width: c_int,
    height: c_int,
}

#[link(name = "atspi")]
extern "C" {
    fn atspi_init() -> c_int;
    fn atspi_get_desktop(i: c_int) -> *mut c_void;
    fn atspi_accessible_get_child_count(obj: *mut c_void, err: *mut *mut c_void) -> c_int;
    fn atspi_accessible_get_child_at_index(
        obj: *mut c_void,
        i: c_int,
        err: *mut *mut c_void,
    ) -> *mut c_void;
    fn atspi_accessible_get_role_name(obj: *mut c_void, err: *mut *mut c_void) -> *mut c_char;
    fn atspi_accessible_get_name(obj: *mut c_void, err: *mut *mut c_void) -> *mut c_char;
    fn atspi_accessible_get_accessible_id(obj: *mut c_void, err: *mut *mut c_void) -> *mut c_char;
    fn atspi_accessible_get_toolkit_name(obj: *mut c_void, err: *mut *mut c_void) -> *mut c_char;
    fn atspi_accessible_get_process_id(obj: *mut c_void, err: *mut *mut c_void) -> c_int;
    fn atspi_accessible_get_attributes(obj: *mut c_void, err: *mut *mut c_void) -> *mut c_void;
    fn atspi_accessible_get_state_set(obj: *mut c_void) -> *mut c_void;
    fn atspi_state_set_contains(set: *mut c_void, state: c_int) -> c_int;
    fn atspi_accessible_get_component_iface(obj: *mut c_void) -> *mut c_void;
    fn atspi_component_get_extents(
        obj: *mut c_void,
        ctype: c_int,
        err: *mut *mut c_void,
    ) -> *mut AtspiRect;
    fn atspi_component_grab_focus(obj: *mut c_void, err: *mut *mut c_void) -> c_int;
    fn atspi_accessible_get_action_iface(obj: *mut c_void) -> *mut c_void;
    fn atspi_action_get_n_actions(obj: *mut c_void, err: *mut *mut c_void) -> c_int;
    fn atspi_action_do_action(obj: *mut c_void, i: c_int, err: *mut *mut c_void) -> c_int;
    fn atspi_accessible_get_value_iface(obj: *mut c_void) -> *mut c_void;
    fn atspi_value_get_current_value(obj: *mut c_void, err: *mut *mut c_void) -> f64;
    fn atspi_accessible_get_text_iface(obj: *mut c_void) -> *mut c_void;
    fn atspi_text_get_character_count(obj: *mut c_void, err: *mut *mut c_void) -> c_int;
    fn atspi_text_get_text(
        obj: *mut c_void,
        start: c_int,
        end: c_int,
        err: *mut *mut c_void,
    ) -> *mut c_char;
    fn atspi_accessible_get_editable_text_iface(obj: *mut c_void) -> *mut c_void;
    fn atspi_editable_text_set_text_contents(
        obj: *mut c_void,
        contents: *const c_char,
        err: *mut *mut c_void,
    ) -> c_int;
    fn atspi_generate_keyboard_event(
        keyval: c_long,
        keystring: *const c_char,
        synth: c_int,
        err: *mut *mut c_void,
    ) -> c_int;
}

#[link(name = "gobject-2.0")]
extern "C" {
    fn g_object_ref(obj: *mut c_void) -> *mut c_void;
    fn g_object_unref(obj: *mut c_void);
}

#[link(name = "glib-2.0")]
extern "C" {
    fn g_free(p: *mut c_void);
    fn g_hash_table_lookup(table: *mut c_void, key: *const c_void) -> *mut c_void;
    fn g_hash_table_unref(table: *mut c_void);
}

/// An owned reference to an AT-SPI accessible (or the app/desktop node). Drop
/// releases the GObject ref, so the tree walk never leaks and the --soak RSS
/// reading stays honest.
struct Acc(*mut c_void);

impl Acc {
    fn from_owned(p: *mut c_void) -> Option<Acc> {
        if p.is_null() {
            None
        } else {
            Some(Acc(p))
        }
    }
    fn dup(&self) -> Acc {
        unsafe { Acc(g_object_ref(self.0)) }
    }
    fn ptr(&self) -> *mut c_void {
        self.0
    }
}

impl Drop for Acc {
    fn drop(&mut self) {
        unsafe { g_object_unref(self.0) }
    }
}

unsafe fn take_gstr(p: *mut c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    let s = CStr::from_ptr(p).to_string_lossy().into_owned();
    g_free(p as *mut c_void);
    Some(s)
}

fn role_name(acc: &Acc) -> String {
    unsafe {
        take_gstr(atspi_accessible_get_role_name(acc.ptr(), ptr::null_mut()))
            .map(|s| s.trim().to_uppercase().replace([' ', '-'], "_"))
            .unwrap_or_default()
    }
}

fn acc_name(acc: &Acc) -> String {
    unsafe {
        take_gstr(atspi_accessible_get_name(acc.ptr(), ptr::null_mut()))
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    }
}

fn acc_id(acc: &Acc) -> Option<String> {
    unsafe {
        take_gstr(atspi_accessible_get_accessible_id(
            acc.ptr(),
            ptr::null_mut(),
        ))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    }
}

fn acc_pid(acc: &Acc) -> u32 {
    unsafe { atspi_accessible_get_process_id(acc.ptr(), ptr::null_mut()).max(0) as u32 }
}

// Liveness probe shared by the scenario and single-seed walks: the AT-SPI bus
// reports pid 0 for an application object whose process has exited, so a 0 pid
// means the target died mid-walk and the walk must stop rather than record the
// now-empty tree as a normal state/edge.
fn target_lost(pid: u32) -> bool {
    pid == 0
}

fn acc_children(acc: &Acc) -> Vec<Acc> {
    let mut out = Vec::new();
    let n = unsafe { atspi_accessible_get_child_count(acc.ptr(), ptr::null_mut()) };
    for i in 0..n.max(0) {
        let p = unsafe { atspi_accessible_get_child_at_index(acc.ptr(), i, ptr::null_mut()) };
        if let Some(c) = Acc::from_owned(p) {
            out.push(c);
        }
    }
    out
}

fn is_showing(acc: &Acc) -> bool {
    unsafe {
        let ss = atspi_accessible_get_state_set(acc.ptr());
        if ss.is_null() {
            return true;
        }
        let has = atspi_state_set_contains(ss, ATSPI_STATE_SHOWING) != 0;
        g_object_unref(ss);
        has
    }
}

fn is_live(acc: &Acc) -> bool {
    unsafe {
        let t = atspi_accessible_get_attributes(acc.ptr(), ptr::null_mut());
        if t.is_null() {
            return false;
        }
        let mut live = false;
        for key in ["live", "container-live", "container_live"] {
            let ck = CString::new(key).unwrap();
            let v = g_hash_table_lookup(t, ck.as_ptr() as *const c_void);
            if !v.is_null() {
                let s = CStr::from_ptr(v as *const c_char)
                    .to_string_lossy()
                    .trim()
                    .to_lowercase();
                if !s.is_empty() && s != "off" {
                    live = true;
                    break;
                }
            }
        }
        g_hash_table_unref(t);
        live
    }
}

fn extents(acc: &Acc) -> Option<(i32, i32, i32, i32)> {
    unsafe {
        let comp = atspi_accessible_get_component_iface(acc.ptr());
        if comp.is_null() {
            return None;
        }
        let r = atspi_component_get_extents(comp, ATSPI_COORD_TYPE_SCREEN, ptr::null_mut());
        g_object_unref(comp);
        if r.is_null() {
            return None;
        }
        let rect = *r;
        g_free(r as *mut c_void);
        if rect.width < 1 || rect.height < 1 {
            None
        } else {
            Some((rect.x, rect.y, rect.width, rect.height))
        }
    }
}

fn grab_focus(acc: &Acc) {
    unsafe {
        let comp = atspi_accessible_get_component_iface(acc.ptr());
        if !comp.is_null() {
            let _ = atspi_component_grab_focus(comp, ptr::null_mut());
            g_object_unref(comp);
        }
    }
}

fn do_press(acc: &Acc) -> bool {
    unsafe {
        let ai = atspi_accessible_get_action_iface(acc.ptr());
        if ai.is_null() {
            return false;
        }
        let n = atspi_action_get_n_actions(ai, ptr::null_mut());
        let ok = n > 0 && atspi_action_do_action(ai, 0, ptr::null_mut()) != 0;
        g_object_unref(ai);
        ok
    }
}

fn set_text(acc: &Acc, value: &str) -> bool {
    unsafe {
        let et = atspi_accessible_get_editable_text_iface(acc.ptr());
        if et.is_null() {
            return false;
        }
        let c = CString::new(value).unwrap_or_default();
        let ok = atspi_editable_text_set_text_contents(et, c.as_ptr(), ptr::null_mut()) != 0;
        g_object_unref(et);
        ok
    }
}

fn send_escape() {
    unsafe {
        let _ = atspi_generate_keyboard_event(
            XKEYCODE_ESCAPE,
            ptr::null(),
            ATSPI_KEY_PRESSRELEASE,
            ptr::null_mut(),
        );
    }
}

// ── AT-SPI role name -> canonical role vocabulary ───────────────────────────
fn atspi_role(role_name: &str) -> &'static str {
    match role_name {
        "FRAME" | "WINDOW" | "APPLICATION" => "screen",
        "DIALOG" | "ALERT" | "FILE_CHOOSER" | "COLOR_CHOOSER" => "dialog",
        "HEADING" => "header",
        "PAGE_TAB_LIST" | "PAGE_TAB" => "tab",
        "LABEL" | "PARAGRAPH" | "STATIC" | "CAPTION" | "STATUS_BAR" => "text",
        "TEXT" | "ENTRY" | "PASSWORD_TEXT" | "SPIN_BUTTON" => "textfield",
        "PUSH_BUTTON" | "BUTTON" | "TOGGLE_BUTTON" => "button",
        "LINK" => "link",
        "IMAGE" | "ICON" => "image",
        "LIST" | "LIST_BOX" | "TABLE" | "TREE" | "TREE_TABLE" => "list",
        "LIST_ITEM" | "TABLE_ROW" | "TABLE_CELL" | "TREE_ITEM" => "listitem",
        "CHECK_BOX" | "CHECK_MENU_ITEM" => "checkbox",
        "RADIO_BUTTON" | "RADIO_MENU_ITEM" => "radio",
        "SWITCH" | "TOGGLE_SWITCH" => "switch",
        "SLIDER" => "slider",
        "SCROLL_BAR" | "SEPARATOR" => "node",
        "PROGRESS_BAR" => "progress",
        "SPINNER" | "BUSY_INDICATOR" => "spinner",
        "TOOL_TIP" => "tooltip",
        "NOTIFICATION" | "INFO_BAR" => "toast",
        "MENU" | "MENU_BAR" | "POPUP_MENU" => "menu",
        "MENU_ITEM" => "menuitem",
        "PANEL" | "FILLER" | "GROUPING" | "TOOL_BAR" | "VIEWPORT" | "SECTION" | "FORM"
        | "SCROLL_PANE" | "SPLIT_PANE" | "LAYERED_PANE" => "group",
        _ => "node",
    }
}

const VALUE_ROLES: &[&str] = &[
    "textfield",
    "status",
    "log",
    "progressbar",
    "meter",
    "timer",
    "output",
];

// AT-SPI role names that respond to an action (the tappable set).
const TAPPABLE_ROLE_NAMES: &[&str] = &[
    "PUSH_BUTTON",
    // ATSPI_ROLE_BUTTON: the generic button role name ("button") that current
    // at-spi2-core exposes and modern Qt6/GTK map ordinary push buttons to
    // (legacy stacks used PUSH_BUTTON / "push button"). Without this the runner
    // never classifies a plain button as tappable on a current Linux desktop.
    "BUTTON",
    "MENU_ITEM",
    "PAGE_TAB",
    "LIST_ITEM",
    "LINK",
    "CHECK_BOX",
    "RADIO_BUTTON",
    "TOGGLE_BUTTON",
];

// AT-SPI role names holding editable text (the `type:` targets).
const TYPABLE_ROLE_NAMES: &[&str] = &[
    "ENTRY",
    "TEXT",
    "PASSWORD_TEXT",
    "EDITBAR",
    "TERMINAL",
    "SPIN_BUTTON",
    "AUTOCOMPLETE",
];

fn input_type_for(role_name: &str, role: &str) -> Option<String> {
    if role != "textfield" {
        return None;
    }
    match role_name {
        "PASSWORD_TEXT" => Some("password".into()),
        "SPIN_BUTTON" => Some("number".into()),
        _ => None,
    }
}

// Role with the live-region / progressbar promotions applied (returns a &'static
// role token so build_node can hand it straight to Node::new).
fn live_role(acc: &Acc, role_name: &str, role: &'static str) -> &'static str {
    let mut r = role;
    // STATUS_BAR is always a status value-role; a live-region text/group is too.
    if role_name == "STATUS_BAR" || ((role == "text" || role == "node") && is_live(acc)) {
        r = "status";
    }
    if role == "progress" && role_name == "PROGRESS_BAR" {
        unsafe {
            let vi = atspi_accessible_get_value_iface(acc.ptr());
            if !vi.is_null() {
                g_object_unref(vi);
                return "progressbar";
            }
        }
    }
    r
}

fn fmt_value(cv: f64) -> String {
    if cv == cv.trunc() {
        format!("{}", cv as i64)
    } else {
        format!("{cv}")
    }
}

fn read_value(acc: &Acc, role: &str) -> Option<String> {
    if !VALUE_ROLES.contains(&role) {
        return None;
    }
    unsafe {
        let vi = atspi_accessible_get_value_iface(acc.ptr());
        if !vi.is_null() {
            let cv = atspi_value_get_current_value(vi, ptr::null_mut());
            g_object_unref(vi);
            return Some(fmt_value(cv));
        }
        let ti = atspi_accessible_get_text_iface(acc.ptr());
        if !ti.is_null() {
            let n = atspi_text_get_character_count(ti, ptr::null_mut());
            let end = if n >= 0 { n } else { -1 };
            let p = atspi_text_get_text(ti, 0, end, ptr::null_mut());
            g_object_unref(ti);
            if let Some(s) = take_gstr(p) {
                return Some(s);
            }
        }
    }
    if role == "status" {
        let nm = acc_name(acc);
        if !nm.is_empty() {
            return Some(nm);
        }
    }
    None
}

fn acc_key(acc: &Acc, role: &str) -> String {
    match acc_id(acc) {
        Some(id) => format!("id:{id}"),
        None => format!("role:{role}"),
    }
}

fn anchor_of(app: &Acc) -> Option<String> {
    for child in acc_children(app) {
        if let Some(id) = acc_id(&child) {
            return Some(id);
        }
    }
    if let Some(id) = acc_id(app) {
        return Some(id);
    }
    unsafe {
        take_gstr(atspi_accessible_get_toolkit_name(
            app.ptr(),
            ptr::null_mut(),
        ))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    }
}

// Walk a live AT-SPI accessible into a canonical Node tree, skipping children the
// toolkit marks off-screen (not SHOWING) so the structural signature matches the
// visible screen (the Qt hidden-widget fix).
fn build_node(acc: &Acc, depth: usize) -> Node {
    let rn = role_name(acc);
    let base = atspi_role(&rn);
    let role = live_role(acc, &rn, base);
    let mut node = Node::new(role);
    node.id = acc_id(acc);
    node.type_ = input_type_for(&rn, role);
    node.value = read_value(acc, role);
    if depth < 60 {
        for child in acc_children(acc) {
            if is_showing(&child) {
                node.children.push(build_node(&child, depth + 1));
            }
        }
    }
    node
}

// ── CONTENT-BUG oracle (label-based, same classes as the web/UIA runners) ───
// First match wins. The undefined/null/NaN regexes are whole-word (so "annulled"
// never matches), but a whole-word hit alone is not proof of a leak: the same word
// occurs in ordinary prose (a dialog body "repro demo crash: null inventory
// record."). A leak artifact IS the label ("null", "Price: null"); prose merely
// mentions the word inside a sentence. So each bare-word candidate then goes
// through a prose guard (label_looks_like_prose) before it is reported, the same
// length+sentence test the [object Object] class already used. Templates
// ({{..}}/${..}) are always artifacts and skip the guard.
fn cb_regex() -> &'static [(Regex, &'static str)] {
    static RE: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    RE.get_or_init(|| {
        vec![
            (Regex::new(r"\{\{[^}]*\}\}").unwrap(), "unrendered-template"),
            (Regex::new(r"\$\{[^}]*\}").unwrap(), "unrendered-template"),
            (
                Regex::new(r"(^|[\s:>(\[,])undefined($|[\s.,!?)\]<])").unwrap(),
                "undefined",
            ),
            (
                Regex::new(r"(^|[\s:>(\[,])null($|[\s.,!?)\]<])").unwrap(),
                "null",
            ),
            (
                Regex::new(r"(^|[\s:>(\[,])NaN($|[\s.,!?)\]<])").unwrap(),
                "nan",
            ),
        ]
    })
}

// A stringify/template token is a leak only when it IS the label: bare ("null"),
// or after a short field-name prefix ("Price: null"). When the same token instead
// sits inside a longer sentence (multiple words, sentence-ending punctuation) it
// is prose that merely mentions the word, not an artifact reaching the screen. The
// test: remove the token, collapse whitespace, and treat what remains as prose
// when it is long (> 24 chars) or carries sentence punctuation (. ! ?). Mirrors
// the web/RN guard and is shared by every content-bug class here.
fn label_looks_like_prose(text: &str, token: &str) -> bool {
    let stripped = text.replace(token, " ");
    let stripped = stripped.split_whitespace().collect::<Vec<_>>().join(" ");
    let has_sentence = stripped.chars().any(|c| c == '.' || c == '!' || c == '?');
    stripped.chars().count() > 24 || has_sentence
}

fn content_bug_reason(text: &str) -> Option<&'static str> {
    if text.is_empty() {
        return None;
    }
    // Fire only when the artifact IS the label (bare, or a short field-name prefix
    // like "Price: [object Object]"), not when prose merely mentions the phrase.
    if text.contains("[object Object]") && !label_looks_like_prose(text, "[object Object]") {
        return Some("object-object");
    }
    for (re, reason) in cb_regex() {
        if re.is_match(text) {
            // Templates are always artifacts; the bare-word classes
            // (undefined/null/NaN) get the same prose guard so a sentence that
            // merely mentions the word is not reported as a leak.
            if *reason == "unrendered-template" {
                return Some(reason);
            }
            let token = match *reason {
                "undefined" => "undefined",
                "null" => "null",
                _ => "NaN",
            };
            if !label_looks_like_prose(text, token) {
                return Some(reason);
            }
        }
    }
    None
}

// ── BROKEN-ASSET oracle (tofu: rendered U+FFFD) ─────────────────────────────
// Mirrors the tofu class of runners/web/hygiene-oracles.mjs brokenAssetScan: a
// rendered U+FFFD replacement character in an accessible's name is broken text
// encoding reaching the screen. U+FFFD is what a decoder emits on malformed
// input, never a glyph an app renders on purpose, so the test is a pure
// substring check with no false positives. AT-SPI exposes no image pixel
// status and no font load status, so tofu is the only broken-asset class with
// AT-SPI ground truth here (the img/font classes stay web-only). Returns a
// short clipped excerpt around the first U+FFFD (the human detail; the stable
// node key is the finding identity), or None when no replacement char rendered.
fn tofu_detail(text: &str) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    let hit = chars.iter().position(|&c| c == '\u{FFFD}')?;
    let start = hit.saturating_sub(20);
    let end = (hit + 21).min(chars.len());
    Some(
        chars[start..end]
            .iter()
            .collect::<String>()
            .trim()
            .to_string(),
    )
}

fn emit(s: &str) {
    println!("{s}");
    let _ = std::io::stdout().flush();
}

// ── APP-INVARIANT oracle (EXPLORE:INVARIANT, SDK-self-triggered) ────────────
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
fn parse_invariant_marker(line: &str) -> Option<(String, Vec<(String, String)>)> {
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
struct InvariantState {
    by_sig: BTreeMap<String, Vec<(String, String)>>,
    fallback: Option<Vec<(String, String)>>,
}

/// Scrapes an app-under-test's stderr for `REPROIT_INVARIANT` markers and lets
/// the walk re-emit them as `EXPLORE:INVARIANT` on the runner's current sig. The
/// SDK and the runner compute the SAME canonical a11y signature (crate::signature
/// / the reproit-linux port), so a marker carrying the SDK's sig matches the
/// runner's identical sig; an empty-sig marker lands on the next observed state.
/// Per-sig de-dup keeps a standing violation from repeating on every settle.
struct InvariantScrape {
    state: Arc<Mutex<InvariantState>>,
    emitted: BTreeSet<String>,
}

impl InvariantScrape {
    /// Start scraping `reader` (the child's piped stderr) on a background thread.
    fn spawn(reader: impl std::io::Read + Send + 'static) -> Self {
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
    fn pending_for(&mut self, sig: &str) -> Option<Vec<(String, String)>> {
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
    fn flush_for(&mut self, sig: &str) {
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

struct Snapshot {
    sig: String,
    content: String,
    labels: Vec<String>,
    elements: Vec<serde_json::Value>,
    tappables: Vec<String>,
    nodes: HashMap<String, Acc>,
    content_bugs: Vec<(String, &'static str, String)>,
    broken_assets: Vec<(String, String)>,
}

fn snapshot(app: &Acc, value_selectors: &[String], cap: &mut ValueCap) -> Snapshot {
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
    labels: Vec<String>,
    elements: Vec<serde_json::Value>,
    tappables: Vec<String>,
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
            let purpose =
                crate::appmap::normalize_input_purpose(input_type_for(&rn, crole).as_deref(), &sel);
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

fn crash(title: &str, detail: &str) {
    emit(&format!(
        "EXCEPTION CAUGHT BY REPROIT \u{2561} {title} \u{255e}"
    ));
    emit(&format!("The following condition was hit: {detail}"));
    emit(&"\u{2550}".repeat(8));
}

// ── LEAK sampler (MEMORY:SAMPLE, --soak): VmRSS from /proc/<pid>/status ──────
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

fn sample_rss(pid: u32, t_ms: u64) {
    if let Some(rss) = vmrss_bytes(pid) {
        emit(&format!(
            "MEMORY:SAMPLE {}",
            serde_json::json!({ "t_ms": t_ms, "heap_used": rss })
        ));
    }
}

fn maybe_emit_hang(from_sig: &str, action: &str, elapsed_ms: u64) {
    if elapsed_ms >= HANG_FLOOR_MS {
        emit(&format!(
            "EXPLORE:HANG {}",
            serde_json::json!({ "from": from_sig, "action": action, "bucket": HANG_FLOOR_MS })
        ));
    }
}

// ── screenshot (SHOOT contract): external tools cropped to the window extents ─
fn sanitize_shot_name(raw: &str) -> String {
    raw.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '/' | '-'))
        .collect()
}

fn app_window(app: &Acc) -> Acc {
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

fn shoot(app: &Acc, raw_name: &str) {
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

// ── fuzz config + graph helpers (same shape as backends/tui.rs and uia.rs) ──

fn load_fuzz_json() -> serde_json::Value {
    let Ok(path) = std::env::var("REPROIT_FUZZ_CONFIG") else {
        return serde_json::json!({});
    };
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(|| serde_json::json!({}))
}

/// The list of per-seed fuzz configs plus whether this is a multi-seed batch.
fn load_batch() -> (Vec<serde_json::Value>, bool) {
    let j = load_fuzz_json();
    if let Some(batch) = j.get("batch").and_then(|v| v.as_array()) {
        if !batch.is_empty() {
            return (batch.clone(), true);
        }
    }
    (vec![j], false)
}

struct Rng {
    s: u32,
}
impl Rng {
    fn new(seed: u32) -> Self {
        Rng {
            s: if seed == 0 { 1 } else { seed },
        }
    }
    fn step(&mut self) -> u32 {
        self.s ^= self.s << 13;
        self.s ^= self.s >> 17;
        self.s ^= self.s << 5;
        self.s
    }
    fn unit(&mut self) -> f64 {
        (self.step() & 0x7fff_ffff) as f64 / (0x8000_0000u32 as f64)
    }
}

fn str_array(j: &serde_json::Value, key: &str) -> Option<Vec<String>> {
    j.get(key).and_then(|v| v.as_array()).map(|a| {
        a.iter()
            .filter_map(|x| x.as_str().map(String::from))
            .collect()
    })
}

fn edge_key(sig: &str, action: &str) -> String {
    format!("{sig}|{action}")
}

fn remember_actions(m: &mut BTreeMap<String, Vec<String>>, sig: &str, actions: Vec<String>) {
    let known = m.entry(sig.to_string()).or_default();
    for a in actions {
        if !known.contains(&a) {
            known.push(a);
        }
    }
}

fn first_untried_action(
    m: &BTreeMap<String, Vec<String>>,
    tried: &BTreeSet<String>,
    sig: &str,
) -> Option<String> {
    m.get(sig).and_then(|actions| {
        actions
            .iter()
            .find(|a| !tried.contains(&edge_key(sig, a)))
            .cloned()
    })
}

fn has_frontier(m: &BTreeMap<String, Vec<String>>, tried: &BTreeSet<String>) -> bool {
    m.keys()
        .any(|sig| first_untried_action(m, tried, sig).is_some())
}

fn remember_edge(
    g: &mut BTreeMap<String, Vec<(String, String)>>,
    from: &str,
    action: &str,
    to: &str,
) {
    let edges = g.entry(from.to_string()).or_default();
    if !edges.iter().any(|(a, t)| a == action && t == to) {
        edges.push((action.to_string(), to.to_string()));
    }
}

fn path_to_frontier(
    g: &BTreeMap<String, Vec<(String, String)>>,
    m: &BTreeMap<String, Vec<String>>,
    tried: &BTreeSet<String>,
    from: &str,
) -> Option<Vec<String>> {
    if first_untried_action(m, tried, from).is_some() {
        return Some(Vec::new());
    }
    let mut seen = BTreeSet::new();
    let mut q = std::collections::VecDeque::new();
    seen.insert(from.to_string());
    q.push_back((from.to_string(), Vec::<String>::new()));
    while let Some((sig, path)) = q.pop_front() {
        if let Some(edges) = g.get(&sig) {
            for (action, to) in edges {
                if !seen.insert(to.clone()) {
                    continue;
                }
                let mut next = path.clone();
                next.push(action.clone());
                if first_untried_action(m, tried, to).is_some() {
                    return Some(next);
                }
                q.push_back((to.clone(), next));
            }
        }
    }
    None
}

// ── multi-actor scenario client (the conductor protocol) ────────────────────

fn barrier_hit(base: &str, method: &str, path: &str) -> Option<String> {
    let addr = base.trim_end_matches('/');
    let addr = addr.strip_prefix("http://").unwrap_or(addr);
    let mut sock = std::net::TcpStream::connect(addr).ok()?;
    sock.set_read_timeout(Some(Duration::from_secs(10))).ok()?;
    write!(
        sock,
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n"
    )
    .ok()?;
    sock.flush().ok()?;
    let mut raw = String::new();
    sock.read_to_string(&mut raw).ok()?;
    Some(
        raw.split_once("\r\n\r\n")
            .map(|(_, body)| body.trim().to_string())
            .unwrap_or_default(),
    )
}

fn find_typable(node: &Acc, finder: &str, depth: usize) -> Option<Acc> {
    if depth > 60 {
        return None;
    }
    let want = finder.strip_prefix("key:").unwrap_or(finder);
    let rn = role_name(node);
    if TYPABLE_ROLE_NAMES.contains(&rn.as_str()) {
        let ident = acc_id(node).unwrap_or_default();
        let label = acc_name(node);
        if (!ident.is_empty() && (ident == want || ident == finder))
            || (!label.is_empty() && label == want)
        {
            return Some(node.dup());
        }
    }
    for child in acc_children(node) {
        if let Some(hit) = find_typable(&child, finder, depth + 1) {
            return Some(hit);
        }
    }
    None
}

fn observe_scenario(
    app: &Acc,
    value_selectors: &[String],
    cap: &mut ValueCap,
    seen: &mut BTreeSet<String>,
) -> Snapshot {
    // LIFECYCLE-metamorphic oracles (rotation, background-restore) are NOT ported
    // to the Linux AT-SPI backend: a desktop window has no device orientation to
    // rotate, and this backend drives the app by walking the AT-SPI tree and
    // clicking -- it has no app-lifecycle background/foreground hook (minimizing
    // is a window-manager action, not a paused->resumed lifecycle), so the ground
    // truth those oracles need cannot be produced here.
    let snap = snapshot(app, value_selectors, cap);
    let observation_labels: Vec<&String> = snap.labels.iter().take(MAX_LABELS_PER_STATE).collect();
    emit(&format!(
        "FUZZ:OBS {}",
        serde_json::json!({ "sig": snap.sig, "labels": observation_labels, "elements": snap.elements })
    ));
    if seen.insert(snap.sig.clone()) {
        let labels: Vec<&String> = snap.labels.iter().take(MAX_LABELS_PER_STATE).collect();
        emit(&format!(
            "EXPLORE:STATE {}",
            serde_json::json!({ "sig": snap.sig, "labels": labels, "elements": snap.elements })
        ));
    }
    snap
}

fn run_scenario_actor(
    app: &Acc,
    value_selectors: &[String],
    cap: &mut ValueCap,
    base: &str,
) -> Result<()> {
    let mut role = std::env::var("REPROIT_DEVICE").unwrap_or_default();
    if role.is_empty() {
        role = match barrier_hit(base, "GET", "/claim") {
            Some(r) if !r.is_empty() && !r.starts_with("ERR") => r,
            _ => "a".to_string(),
        };
    }
    emit(&format!("JOURNEY claimed role={role}"));
    std::thread::sleep(Duration::from_millis(900));

    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut failed = false;
    let mut current = observe_scenario(app, value_selectors, cap, &mut seen);

    for _ in 0..100_000u32 {
        let body = match barrier_hit(base, "GET", &format!("/next?device={role}")) {
            Some(b) => b,
            None => {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
        };
        if body == "DONE" {
            break;
        }
        if body == "WAIT" {
            std::thread::sleep(Duration::from_millis(40));
            continue;
        }
        let act = body.strip_prefix("ACT\t").unwrap_or(&body).to_string();
        emit(&format!("FUZZ:ACT {role} {act}"));
        grab_focus(&app_window(app));

        if let Some(name) = act.strip_prefix("shoot:") {
            shoot(app, name);
        } else if let Some(a) = act.strip_prefix("assert:") {
            let fresh = snapshot(app, value_selectors, cap);
            let contents = fresh.labels.join("\n");
            if let Some(want) = a.strip_prefix("text=") {
                let ok = contents.contains(want);
                emit(&format!(
                    "FUZZ:ASSERT {} text={} actor={role}",
                    if ok { "pass" } else { "fail" },
                    serde_json::json!(want)
                ));
            } else if let Some(rest) = a.strip_prefix("count:") {
                let (finder, want) = rest.rsplit_once('=').unwrap_or((rest, "0"));
                let want: i64 = want.parse().unwrap_or(0);
                let got = if finder.is_empty() {
                    0
                } else {
                    contents.matches(finder).count() as i64
                };
                emit(&format!(
                    "FUZZ:ASSERT {} count {finder} want={want} got={got} actor={role}",
                    if got == want { "pass" } else { "fail" }
                ));
            } else {
                emit(&format!("FUZZ:ASSERT fail unsupported {a} actor={role}"));
            }
        } else if let Some(acct) = act.strip_prefix("auth:") {
            emit(&format!(
                "JOURNEY[a] step: auth-restore unsupported on desktop-atspi runner; \
                 drive the login UI explicitly for auth:{acct}"
            ));
        } else if act == "back" {
            send_escape();
            std::thread::sleep(Duration::from_millis(600));
        } else if let Some(b) = act.strip_prefix("type:") {
            let (finder, value) = b.rsplit_once('=').unwrap_or((b, ""));
            match find_typable(app, finder, 0) {
                Some(node) if set_text(&node, value) => {}
                _ => emit(&format!("FUZZ:MISS {role} {act}")),
            }
            std::thread::sleep(Duration::from_millis(600));
        } else if let Some(label) = act.strip_prefix("tap:") {
            let fresh = snapshot(app, value_selectors, cap);
            match fresh.nodes.get(label) {
                Some(node) if do_press(node) => {}
                _ => emit(&format!("FUZZ:MISS {role} {act}")),
            }
            std::thread::sleep(Duration::from_millis(700));
        } else {
            emit(&format!("FUZZ:MISS {role} {act}"));
        }

        // Crash oracle: the app process gone from the bus cannot continue.
        // Deliberately no /done ack, so the conductor names this actor+action.
        if target_lost(acc_pid(app)) {
            crash(
                "target lost",
                &format!("the AT-SPI target vanished during {act}"),
            );
            failed = true;
            break;
        }
        let nxt = observe_scenario(app, value_selectors, cap, &mut seen);
        if nxt.sig != current.sig {
            emit(&format!(
                "EXPLORE:EDGE {}",
                serde_json::json!({ "from": current.sig, "action": act, "to": nxt.sig })
            ));
        }
        current = nxt;
        let _ = barrier_hit(base, "POST", &format!("/done?device={role}"));
    }

    emit("JOURNEY DONE");
    emit(if failed {
        "Some tests failed"
    } else {
        "All tests passed"
    });
    Ok(())
}

fn find_app_by_name(desktop: &Acc, target: &str) -> Option<Acc> {
    let want = target.to_lowercase();
    acc_children(desktop)
        .into_iter()
        .find(|app| acc_name(app).to_lowercase().contains(&want))
}

fn find_app_by_pid(desktop: &Acc, pid: u32) -> Option<Acc> {
    acc_children(desktop)
        .into_iter()
        .find(|app| acc_pid(app) == pid)
}

// One seed's explore/replay walk (single-seed contract, per-seed coverage).
fn run_seed(
    app: &Acc,
    value_selectors: &[String],
    cap: &mut ValueCap,
    target_pid: u32,
    fuzz: &serde_json::Value,
    // App-invariant scrape of the launched child's stderr (None when we attached
    // to an already-running app by name, which exposes no stderr to scrape).
    mut inv: Option<&mut InvariantScrape>,
) -> bool {
    let seed = fuzz.get("seed").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let mut rng = Rng::new(seed);
    if seed != 0 {
        emit(&format!("JOURNEY[a] step: fuzz seed={seed}"));
    }

    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut tried: BTreeSet<String> = BTreeSet::new();
    let mut actions_by_state: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut graph: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();

    let mut observe = |app: &Acc, cap: &mut ValueCap, seen: &mut BTreeSet<String>| -> Snapshot {
        let snap = snapshot(app, value_selectors, cap);
        let observation_labels: Vec<&String> =
            snap.labels.iter().take(MAX_LABELS_PER_STATE).collect();
        emit(&format!(
            "FUZZ:OBS {}",
            serde_json::json!({ "sig": snap.sig, "labels": observation_labels, "elements": snap.elements })
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
                    .map(|(k, reason, text)| serde_json::json!({ "key": k, "reason": reason, "text": text }))
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
                    .map(|(k, detail)| serde_json::json!({ "key": k, "reason": "tofu", "detail": detail }))
                    .collect();
                emit(&format!(
                    "EXPLORE:BROKENASSET {}",
                    serde_json::json!({ "sig": snap.sig, "items": items })
                ));
            }
        }
        // APP-INVARIANT (EXPLORE:INVARIANT): re-emit any violation the app's SDK
        // reported for this state (scraped from the child's stderr). Runs every
        // settle, not just new states, so a violation that appears on a revisit
        // is still caught; the scrape de-dups per sig so it is reported once.
        if let Some(iv) = inv.as_deref_mut() {
            iv.flush_for(&snap.sig);
        }
        snap
    };

    let mut current = observe(app, cap, &mut seen);
    let launch_sig = current.sig.clone();
    let mut stuck = 0u32;
    let mut crashed = false;

    let prefix = str_array(fuzz, "prefix");
    let replay = str_array(fuzz, "replay");
    let prefix_len = prefix.as_ref().map(|p| p.len()).unwrap_or(0);
    let map_mode = replay.is_none() && prefix.is_none() && seed == 0;
    let configured = std::env::var("REPROIT_FUZZ_CONFIG").is_ok();
    let budget: usize = if let Some(r) = &replay {
        r.len()
    } else if map_mode && !configured {
        usize::MAX
    } else {
        fuzz.get("budget")
            .and_then(|v| v.as_u64())
            .unwrap_or(ACTION_BUDGET as u64) as usize
            + prefix_len
    };
    let edge_weights: BTreeMap<String, BTreeMap<String, u64>> = fuzz
        .get("edgeWeights")
        .and_then(|v| v.as_object())
        .map(|obj| {
            obj.iter()
                .filter_map(|(sig, m)| {
                    m.as_object().map(|mm| {
                        (
                            sig.clone(),
                            mm.iter()
                                .filter_map(|(k, v)| v.as_u64().map(|n| (k.clone(), n)))
                                .collect(),
                        )
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let is_soak = replay.is_some();
    let soak_start = Instant::now();
    if is_soak {
        sample_rss(target_pid, 0);
    }

    let mut i = 0usize;
    while i < budget && stuck < 3 {
        if is_soak && i > 0 {
            sample_rss(target_pid, soak_start.elapsed().as_millis() as u64);
        }
        let act: Option<String> = if let Some(r) = &replay {
            r.get(i).cloned()
        } else if prefix.as_ref().map(|p| i < p.len()).unwrap_or(false) {
            prefix.as_ref().and_then(|p| p.get(i).cloned())
        } else if seed != 0 {
            let mut taps: Vec<String> = current.tappables.clone();
            taps.sort();
            let ew = edge_weights.get(&current.sig);
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
        emit(&format!("FUZZ:ACT {act}"));

        if let Some(name) = act.strip_prefix("shoot:") {
            shoot(app, name);
            i += 1;
            continue;
        }
        if act == "back" {
            let from_sig = current.sig.clone();
            tried.insert(edge_key(&from_sig, "back"));
            send_escape();
            std::thread::sleep(Duration::from_millis(600));
            // Crash oracle: if the action killed the target, stop before recording
            // the now-empty tree as a state/edge (mirrors run_scenario_actor).
            if target_lost(acc_pid(app)) {
                crash(
                    "target lost",
                    &format!("the AT-SPI target vanished during {act}"),
                );
                crashed = true;
                break;
            }
            let observe_start = Instant::now();
            let nxt = observe(app, cap, &mut seen);
            maybe_emit_hang(
                &from_sig,
                "back",
                observe_start.elapsed().as_millis() as u64,
            );
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
        let press_start = Instant::now();
        let pressed = current.nodes.get(&label).map(do_press).unwrap_or(false);
        if !pressed {
            emit(&format!("FUZZ:MISS {act}"));
            stuck += 1;
            i += 1;
            continue;
        }
        std::thread::sleep(Duration::from_millis(700));
        // Crash oracle: a tap that killed the target ends the walk before the
        // empty tree is recorded as a state/edge (mirrors run_scenario_actor).
        if target_lost(acc_pid(app)) {
            crash(
                "target lost",
                &format!("the AT-SPI target vanished during {act}"),
            );
            crashed = true;
            break;
        }
        let nxt = observe(app, cap, &mut seen);
        let elapsed = press_start.elapsed().as_millis() as u64;
        maybe_emit_hang(
            &from_sig,
            &format!("tap:{label}"),
            elapsed.saturating_sub(700),
        );
        if nxt.sig != current.sig {
            emit(&format!(
                "EXPLORE:EDGE {}",
                serde_json::json!({ "from": current.sig, "action": format!("tap:{label}"), "to": nxt.sig })
            ));
            remember_edge(&mut graph, &current.sig, &format!("tap:{label}"), &nxt.sig);
        }
        if nxt.sig != current.sig || nxt.content != current.content {
            stuck = 0;
        }
        current = nxt;
        i += 1;
    }

    emit(&format!("JOURNEY[a] step: explored {} states", seen.len()));
    crashed
}

fn reset_to_root() {
    for _ in 0..4 {
        send_escape();
        std::thread::sleep(Duration::from_millis(200));
    }
    std::thread::sleep(Duration::from_millis(400));
}

pub fn run() -> Result<()> {
    let target = std::env::var("REPROIT_TARGET")
        .ok()
        .filter(|s| !s.is_empty())
        .context("REPROIT_TARGET (app name or launch path) required")?;

    let scenario_base = std::env::var("REPROIT_SCENARIO_BARRIER")
        .ok()
        .filter(|s| !s.is_empty());
    if scenario_base.is_none() {
        emit("JOURNEY claimed role=a");
    }

    unsafe {
        atspi_init();
    }

    let desktop = Acc::from_owned(unsafe { atspi_get_desktop(0) })
        .context("atspi_get_desktop(0) returned null (is the a11y bus running?)")?;

    // App-invariant scrape: only a child WE launch exposes a stderr we can pipe
    // (attaching to an already-running app by name does not), so this is set only
    // on the launch-by-path branch below.
    let mut invariant_scrape: Option<InvariantScrape> = None;

    // Launch if it looks like a path, then bind by pid (scenario) or by name.
    let app: Acc = {
        let looks_like_path =
            target.contains(std::path::MAIN_SEPARATOR) && std::path::Path::new(&target).exists();
        if looks_like_path {
            let mut child = std::process::Command::new(&target)
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
            std::thread::sleep(Duration::from_millis(2500));
            let by_pid = if scenario_base.is_some() {
                find_app_by_pid(&desktop, child.id())
            } else {
                None
            };
            let base = std::path::Path::new(&target)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| target.clone());
            match by_pid.or_else(|| find_app_by_name(&desktop, &base)) {
                Some(a) => a,
                None => {
                    crash(
                        "target not found",
                        &format!("no AT-SPI application matching {target:?}"),
                    );
                    std::process::exit(3);
                }
            }
        } else {
            match find_app_by_name(&desktop, &target) {
                Some(a) => a,
                None => {
                    crash(
                        "target not found",
                        &format!("no AT-SPI application matching {target:?}"),
                    );
                    std::process::exit(3);
                }
            }
        }
    };
    std::thread::sleep(Duration::from_secs(1));

    let target_pid = acc_pid(&app);
    let value_selectors = load_value_node_selectors();
    let mut cap = ValueCap::new();

    if let Some(base) = scenario_base {
        return run_scenario_actor(&app, &value_selectors, &mut cap, &base);
    }

    let (batch, is_batch) = load_batch();
    let mut any_crash = false;
    for fuzz in &batch {
        if is_batch {
            reset_to_root();
            let seed = fuzz.get("seed").and_then(|v| v.as_u64()).unwrap_or(0);
            emit(&format!("SEED:BEGIN {seed}"));
        }
        any_crash |= run_seed(
            &app,
            &value_selectors,
            &mut cap,
            target_pid,
            fuzz,
            invariant_scrape.as_mut(),
        );
        if is_batch {
            let seed = fuzz.get("seed").and_then(|v| v.as_u64()).unwrap_or(0);
            emit(&format!("SEED:END {seed}"));
        }
        // A dead target cannot be driven further by later seeds in the batch.
        if any_crash {
            break;
        }
    }

    emit("JOURNEY DONE");
    emit(if any_crash {
        "Some tests failed"
    } else {
        "All tests passed"
    });
    Ok(())
}

// Read the optional `value_nodes:` selector list from reproit.yaml (Layer 3).
fn load_value_node_selectors() -> Vec<String> {
    let path = std::env::var("REPROIT_CONFIG").unwrap_or_else(|_| {
        std::env::current_dir()
            .map(|d| d.join("reproit.yaml").to_string_lossy().into_owned())
            .unwrap_or_else(|_| "reproit.yaml".into())
    });
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut in_block = false;
    for raw in text.lines() {
        let line = raw.trim_end();
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        if !line.starts_with(' ') && !line.starts_with('\t') {
            in_block = line.trim().trim_end_matches(':') == "value_nodes" && line.ends_with(':');
            continue;
        }
        if in_block {
            let item = line.trim();
            if let Some(sel) = item.strip_prefix('-') {
                let sel = sel.trim().trim_matches('"').trim_matches('\'');
                if !sel.is_empty() {
                    out.push(sel.to_string());
                }
            }
        }
    }
    out
}

// Keep the shared imports honest on all builds.
#[allow(dead_code)]
fn _unused_reexports() {
    let _ = value_class("0");
    let _ = signature(None, &Node::new("screen"));
    let _ = structural_only(&Node::new("screen"));
}

// These tests pin the pure text scan; the libatspi-facing walk is exercised
// live by the operability-golden GTK/Qt CI jobs.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tofu_detail_flags_a_rendered_replacement_char_with_context() {
        // A rendered U+FFFD is broken text encoding: flagged, with a clipped
        // excerpt around the char as the human detail.
        assert_eq!(
            tofu_detail("glitch \u{FFFD} here").as_deref(),
            Some("glitch \u{FFFD} here")
        );
        // Long text clips to a bounded excerpt that still shows the char.
        let long = format!("{}{}{}", "a".repeat(60), '\u{FFFD}', "b".repeat(60));
        let ex = tofu_detail(&long).expect("long tofu text must flag");
        assert!(ex.chars().count() <= 41 && ex.contains('\u{FFFD}'));
    }

    #[test]
    fn tofu_detail_stays_silent_on_clean_text() {
        // No U+FFFD, no finding: plain, empty, and non-ASCII labels are clean.
        assert_eq!(tofu_detail(""), None);
        assert_eq!(tofu_detail("Save changes"), None);
        assert_eq!(tofu_detail("caf\u{e9} \u{4f60}\u{597d} \u{1f600}"), None);
    }

    #[test]
    fn target_lost_flags_a_dead_pid_only() {
        // AT-SPI reports pid 0 for an application whose process has exited: the
        // single liveness invariant shared by the scenario and single-seed walks.
        // (The FFI acc_pid read and the full AT-SPI walk are exercised live by the
        // Linux GTK/Qt CI jobs; only this pure decision is unit-tested here.)
        assert!(target_lost(0));
        assert!(!target_lost(1));
        assert!(!target_lost(4242));
    }

    #[test]
    fn content_bug_flags_leak_artifacts_but_not_prose() {
        // The classic artifacts ARE the label (bare, or a short field prefix): flag.
        assert_eq!(content_bug_reason("null"), Some("null"));
        assert_eq!(content_bug_reason("Price: null"), Some("null"));
        assert_eq!(content_bug_reason("undefined"), Some("undefined"));
        assert_eq!(content_bug_reason("Qty: undefined"), Some("undefined"));
        assert_eq!(content_bug_reason("NaN"), Some("nan"));
        assert_eq!(content_bug_reason("Total: NaN"), Some("nan"));
        // Prose that merely mentions the word inside a sentence is not a leak: a
        // dialog body that happens to contain the word.
        assert_eq!(
            content_bug_reason("repro demo crash: null inventory record."),
            None
        );
        assert_eq!(
            content_bug_reason("The undefined behavior here is intentional and documented."),
            None
        );
        assert_eq!(
            content_bug_reason("Parsing produced NaN because the field was blank, so we retried."),
            None
        );
        // Templates are always artifacts, guard or not; whole-word only, so a word
        // that merely contains the token ("annulled") is clean.
        assert_eq!(
            content_bug_reason("Hello {{name}}"),
            Some("unrendered-template")
        );
        assert_eq!(content_bug_reason("annulled"), None);
    }

    #[test]
    fn parse_invariant_marker_reads_violations_and_ignores_noise() {
        let (sig, items) = parse_invariant_marker(
            r#"REPROIT_INVARIANT {"sig":"s1","items":[{"id":"balance","message":"< 0"}]}"#,
        )
        .expect("a marker parses");
        assert_eq!(sig, "s1");
        assert_eq!(items, vec![("balance".into(), "< 0".into())]);
        // A plain log line, malformed json, and an empty item list are silent
        // (a clean settle emits no marker, so None is the clean direction).
        assert!(parse_invariant_marker("[reproit] some batch json").is_none());
        assert!(parse_invariant_marker("REPROIT_INVARIANT {oops").is_none());
        assert!(parse_invariant_marker(r#"REPROIT_INVARIANT {"items":[]}"#).is_none());
    }

    #[test]
    fn invariant_scrape_dedups_per_state_and_matches_sig() {
        // Build the tracker directly with a pre-populated shared state (bypassing
        // the reader thread) so the assertion is deterministic.
        let mut state = InvariantState::default();
        state
            .by_sig
            .insert("s1".into(), vec![("inv".into(), "boom".into())]);
        state.fallback = Some(vec![("g".into(), String::new())]);
        let mut scr = InvariantScrape {
            state: Arc::new(Mutex::new(state)),
            emitted: BTreeSet::new(),
        };
        // Violating state s1 fires once; a re-visit is de-duped; a clean state
        // consumes the empty-sig fallback (attributed to the current sig).
        assert_eq!(
            scr.pending_for("s1"),
            Some(vec![("inv".into(), "boom".into())])
        );
        assert_eq!(scr.pending_for("s1"), None, "no repeat on revisit");
        assert_eq!(
            scr.pending_for("s2"),
            Some(vec![("g".into(), String::new())]),
            "empty-sig fallback lands on the current runner sig"
        );
        assert_eq!(scr.pending_for("s3"), None, "fallback is consumed once");
    }
}
