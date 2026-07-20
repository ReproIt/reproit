use super::*;

// AT-SPI enum constants (atspi-constants.h). Hardcoded numeric ids so the FFI
// needs no generated headers; these are stable ABI values. SHOWING == 25
// (verify against the installed atspi-constants.h; 27 is STALE, a different
// state).
pub(super) const ATSPI_STATE_SHOWING: c_int = 25;
pub(super) const ATSPI_COORD_TYPE_SCREEN: c_int = 0;
pub(super) const ATSPI_KEY_PRESSRELEASE: c_int = 2;
pub(super) const XKEYCODE_ESCAPE: c_long = 9; // X11 keycode for Escape

// libatspi / glib FFI (the official C library, hand-declared).
// Opaque GObjects are `*mut c_void`; every accessor that returns a new ref or a
// heap string is unref'd / g_free'd by the wrappers below, matching the
// ownership the Python `gi` binding managed via GC.

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
    pub(super) fn atspi_init() -> c_int;
    pub(super) fn atspi_get_desktop(i: c_int) -> *mut c_void;
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
    pub(super) fn atspi_accessible_get_component_iface(obj: *mut c_void) -> *mut c_void;
    fn atspi_component_get_extents(
        obj: *mut c_void,
        ctype: c_int,
        err: *mut *mut c_void,
    ) -> *mut AtspiRect;
    pub(super) fn atspi_component_grab_focus(obj: *mut c_void, err: *mut *mut c_void) -> c_int;
    pub(super) fn atspi_accessible_get_action_iface(obj: *mut c_void) -> *mut c_void;
    pub(super) fn atspi_action_get_n_actions(obj: *mut c_void, err: *mut *mut c_void) -> c_int;
    pub(super) fn atspi_action_do_action(
        obj: *mut c_void,
        i: c_int,
        err: *mut *mut c_void,
    ) -> c_int;
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
    pub(super) fn atspi_accessible_get_editable_text_iface(obj: *mut c_void) -> *mut c_void;
    pub(super) fn atspi_editable_text_set_text_contents(
        obj: *mut c_void,
        contents: *const c_char,
        err: *mut *mut c_void,
    ) -> c_int;
    pub(super) fn atspi_generate_keyboard_event(
        keyval: c_long,
        keystring: *const c_char,
        synth: c_int,
        err: *mut *mut c_void,
    ) -> c_int;
}

#[link(name = "gobject-2.0")]
extern "C" {
    fn g_object_ref(obj: *mut c_void) -> *mut c_void;
    pub(super) fn g_object_unref(obj: *mut c_void);
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
pub(super) struct Acc(*mut c_void);

impl Acc {
    pub(super) fn from_owned(p: *mut c_void) -> Option<Acc> {
        if p.is_null() {
            None
        } else {
            Some(Acc(p))
        }
    }
    pub(super) fn dup(&self) -> Acc {
        unsafe { Acc(g_object_ref(self.0)) }
    }
    pub(super) fn ptr(&self) -> *mut c_void {
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

pub(super) fn role_name(acc: &Acc) -> String {
    unsafe {
        take_gstr(atspi_accessible_get_role_name(acc.ptr(), ptr::null_mut()))
            .map(|s| s.trim().to_uppercase().replace([' ', '-'], "_"))
            .unwrap_or_default()
    }
}

pub(super) fn acc_name(acc: &Acc) -> String {
    unsafe {
        take_gstr(atspi_accessible_get_name(acc.ptr(), ptr::null_mut()))
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    }
}

pub(super) fn acc_id(acc: &Acc) -> Option<String> {
    unsafe {
        take_gstr(atspi_accessible_get_accessible_id(
            acc.ptr(),
            ptr::null_mut(),
        ))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    }
}

pub(super) fn acc_pid(acc: &Acc) -> u32 {
    unsafe { atspi_accessible_get_process_id(acc.ptr(), ptr::null_mut()).max(0) as u32 }
}

// Liveness probe shared by the scenario and single-seed walks: the AT-SPI bus
// reports pid 0 for an application object whose process has exited, so a 0 pid
// means the target died mid-walk and the walk must stop rather than record the
// now-empty tree as a normal state/edge.
pub(super) fn target_lost(pid: u32) -> bool {
    pid == 0
}

pub(super) fn acc_children(acc: &Acc) -> Vec<Acc> {
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

pub(super) fn is_showing(acc: &Acc) -> bool {
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

pub(super) fn is_live(acc: &Acc) -> bool {
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

pub(super) fn extents(acc: &Acc) -> Option<(i32, i32, i32, i32)> {
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

// AT-SPI role name -> canonical role vocabulary.
pub(super) fn atspi_role(role_name: &str) -> &'static str {
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

pub(super) const VALUE_ROLES: &[&str] = &[
    "textfield",
    "status",
    "log",
    "progressbar",
    "meter",
    "timer",
    "output",
];

// AT-SPI role names that respond to an action (the tappable set).
pub(super) const TAPPABLE_ROLE_NAMES: &[&str] = &[
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
pub(super) const TYPABLE_ROLE_NAMES: &[&str] = &[
    "ENTRY",
    "TEXT",
    "PASSWORD_TEXT",
    "EDITBAR",
    "TERMINAL",
    "SPIN_BUTTON",
    "AUTOCOMPLETE",
];

pub(super) fn input_type_for(role_name: &str, role: &str) -> Option<String> {
    if role != "textfield" {
        return None;
    }
    match role_name {
        "PASSWORD_TEXT" => Some("password".into()),
        "SPIN_BUTTON" => Some("number".into()),
        _ => None,
    }
}

// Role with the live-region / progressbar promotions applied (returns a
// &'static role token so build_node can hand it straight to Node::new).
pub(super) fn live_role(acc: &Acc, role_name: &str, role: &'static str) -> &'static str {
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

pub(super) fn fmt_value(cv: f64) -> String {
    if cv == cv.trunc() {
        format!("{}", cv as i64)
    } else {
        format!("{cv}")
    }
}

pub(super) fn read_value(acc: &Acc, role: &str) -> Option<String> {
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

pub(super) fn acc_key(acc: &Acc, role: &str) -> String {
    match acc_id(acc) {
        Some(id) => format!("id:{id}"),
        None => format!("role:{role}"),
    }
}

pub(super) fn anchor_of(app: &Acc) -> Option<String> {
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

// Walk a live AT-SPI accessible into a canonical Node tree, skipping children
// the toolkit marks off-screen (not SHOWING) so the structural signature
// matches the visible screen (the Qt hidden-widget fix).
pub(super) fn build_node(acc: &Acc, depth: usize) -> Node {
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

// CONTENT-BUG oracle (label-based, same classes as the web/UIA runners).
// First match wins. The undefined/null/NaN regexes are whole-word (so
// "annulled" never matches), but a whole-word hit alone is not proof of a leak:
// the same word occurs in ordinary prose (a dialog body "repro demo crash: null
// inventory record."). A leak artifact IS the label ("null", "Price: null");
// prose merely mentions the word inside a sentence. So each bare-word candidate
// then goes through a prose guard (label_looks_like_prose) before it is
// reported, the same length+sentence test the [object Object] class already
// used. Templates ({{..}}/${..}) are always artifacts and skip the guard.
pub(super) fn cb_regex() -> &'static [(Regex, &'static str)] {
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

// A stringify/template token is a leak only when it IS the label: bare
// ("null"), or after a short field-name prefix ("Price: null"). When the same
// token instead sits inside a longer sentence (multiple words, sentence-ending
// punctuation) it is prose that merely mentions the word, not an artifact
// reaching the screen. The test: remove the token, collapse whitespace, and
// treat what remains as prose when it is long (> 24 chars) or carries sentence
// punctuation (. ! ?). Mirrors the web/RN guard and is shared by every
// content-bug class here.
pub(super) fn label_looks_like_prose(text: &str, token: &str) -> bool {
    let stripped = text.replace(token, " ");
    let stripped = stripped.split_whitespace().collect::<Vec<_>>().join(" ");
    let has_sentence = stripped.chars().any(|c| c == '.' || c == '!' || c == '?');
    stripped.chars().count() > 24 || has_sentence
}

pub(super) fn content_bug_reason(text: &str) -> Option<&'static str> {
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

// BROKEN-ASSET oracle (tofu: rendered U+FFFD).
// Mirrors the tofu class of runners/web/hygiene-oracles.mjs brokenAssetScan: a
// rendered U+FFFD replacement character in an accessible's name is broken text
// encoding reaching the screen. U+FFFD is what a decoder emits on malformed
// input, never a glyph an app renders on purpose, so the test is a pure
// substring check with no false positives. AT-SPI exposes no image pixel
// status and no font load status, so tofu is the only broken-asset class with
// AT-SPI ground truth here (the img/font classes stay web-only). Returns a
// short clipped excerpt around the first U+FFFD (the human detail; the stable
// node key is the finding identity), or None when no replacement char rendered.
pub(super) fn tofu_detail(text: &str) -> Option<String> {
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
