// reproit_imgui.h - in-app hook for fuzzing Dear ImGui (immediate-mode).
//
// Immediate-mode GUIs have no retained widget tree and no OS accessibility, so
// they cannot be driven from outside; the app must cooperate. This header is
// that cooperation, and it is tiny. You swap interactive ImGui calls for the
// reproit:: wrappers (a find/replace), and call reproit::Frame() once per frame.
// The wrappers do two things:
//   1. build a CANONICAL STRUCTURAL NODE TREE (role + stable id + type + icon +
//      nesting) from the widget calls, then compute the canonical screen
//      signature exactly as docs/signature.md specifies.
//   2. when the fuzzer picks a widget, the wrapper RETURNS TRUE for one frame
//      -> the app's own `if (Button(...))` branch fires. No synthetic input,
//      no OS event queue: deterministic and instant.
//
// The signature is structural, NOT a hash of visible text: ImGui labels carry
// both a display string and a stable id (the part after "##", or the whole
// label if none). Only the STABLE ID enters the descriptor; the display text is
// excluded (rule 1). So an English vs Japanese build of the same UI, written as
// reproit::Button("Play##play") vs reproit::Button("\xe5\x86\x8d\xe7\x94\x9f##play"),
// hashes identically. This is what makes an ImGui finding bucket to the same
// node a production SDK crash hits.
//
// Usage (one translation unit defines the impl):
//   #define REPROIT_IMGUI_IMPLEMENTATION
//   #include "reproit_imgui.h"
//   ... each frame:
//   ImGui::NewFrame();
//   reproit::Frame();                       // begin capture
//   if (reproit::Begin("MainWindow")) {     // window -> anchor + group scope
//     reproit::Text("Title##title");        // instead of ImGui::Text
//     if (reproit::Button("Play##play")) { ... }
//   }
//   reproit::End();
//   reproit::FrameEnd();                    // emit markers, pick next action
//   ImGui::Render();
//   if (reproit::Done()) break;             // budget exhausted
//
// Config via REPROIT_FUZZ_CONFIG (json path): {"seed":N,"budget":N}. Output is
// the marker protocol on stdout, parsed by reproit exactly like every backend.
//
// PRODUCTION TELEMETRY (optional, OFF by default): define REPROIT_TELEMETRY and
// the SAME reproit:: wrappers double as a production SDK. Every frame the tree
// they build is signed with the existing canonical core and reported as a live
// usage graph (states + edges) plus crash signatures, in the same
// {appId,sentAt,ctx?,events} contract the other SDKs POST. You supply a transport
// callback (wire it to libcurl/your HTTP), or the built-in transport spools
// newline-delimited JSON to a file/FD. A crash hook flushes the last signature +
// edge path on SIGSEGV/SIGABRT (async-signal-safe). With telemetry on the fuzz
// driver does not run: the app reports its real sessions, it is not driven. See
// the "PRODUCTION TELEMETRY CORE" block below and reproit::TelemetryInit().
//
// The signature core is parity-tested against signature_vectors.json
// (runners/test_signature.c). The telemetry layer never modifies that core; it
// only CALLS ReproIt_Signature, so parity is preserved.

// ===========================================================================
// CANONICAL STRUCTURAL SIGNATURE CORE (docs/signature.md)
//
// IDENTICAL to the core block in runners/reproit_clay.h (shared guard, so if
// both headers land in one TU the core appears once). Self-contained plain C;
// the parity test in runners/test_signature.c asserts it against the golden
// vectors. MUST stay byte-for-byte equivalent to the Rust oracle in
// crates/reproit/src/model/signature.rs.
// ===========================================================================
#ifndef REPROIT_SIGNATURE_CORE_H
#define REPROIT_SIGNATURE_CORE_H
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#ifndef REPROIT_SIG_MAX_CHILDREN
#define REPROIT_SIG_MAX_CHILDREN 64
#endif
#ifndef REPROIT_SIG_MAX_DESC
#define REPROIT_SIG_MAX_DESC 16384
#endif

// A normalized accessibility node: the input to the signature. Mirrors the
// `Node` type in the Rust oracle. Strings that are NULL or empty are treated as
// absent. Localized chrome text is excluded from the descriptor by construction
// (rule 1); the only text that can enter is a bounded, locale-safe VALUE-CLASS
// for value-bearing nodes (docs/signature.md "Value-state", Layer 2), carried in
// `value` and folded into the separate V: section.
typedef struct ReproItSig_Node {
    const char* role;                                   // required
    const char* id;                                     // optional, NULL = none
    const char* type;                                   // optional input refinement
    const char* icon;                                   // optional icon identity
    bool transient;                                     // explicit transient marker
    const char* value;                                  // optional displayed value (Layer 2); NULL = none
    bool value_node;                                    // opt-in value-bearing flag (Layer 3)
    int n_children;
    struct ReproItSig_Node* children[REPROIT_SIG_MAX_CHILDREN];
} ReproItSig_Node;

// The fixed, language-independent role vocabulary. Anything outside it
// normalizes to "node".
static const char* const REPROIT_SIG_ROLES[] = {
    "screen", "header", "text", "button", "link", "textfield", "image", "icon",
    "list", "listitem", "tab", "switch", "checkbox", "radio", "slider", "menu",
    "menuitem", "dialog", "group", "node",
};
// Roles that flicker in and out and are dropped before hashing (rule 2).
// "progress" is the role name for spinner/progress.
static const char* const REPROIT_SIG_TRANSIENT[] = {
    "toast", "snackbar", "spinner", "progress", "tooltip", "badge",
};
// Value-role set (docs/signature.md "Value-state"). A node is value-bearing only
// if it has a `value` AND its RAW role is one of these (or it is value_node-
// flagged, Layer 3). Several of these (status, log, progressbar, meter, timer,
// output) are NOT in REPROIT_SIG_ROLES, so they normalize to "node" in the body;
// the value-role test therefore uses the RAW role, not the normalized one. Chrome
// roles (button/label/header/text/link) are NEVER value-bearing.
static const char* const REPROIT_SIG_VALUE_ROLES[] = {
    "textfield", "status", "log", "progressbar", "meter", "timer", "output",
};

static bool reproit_sig_str_empty(const char* s) { return !s || !s[0]; }

static const char* reproit_sig_normalize_role(const char* role) {
    if (!role) return "node";
    for (size_t i = 0; i < sizeof REPROIT_SIG_ROLES / sizeof *REPROIT_SIG_ROLES; i++) {
        if (strcmp(role, REPROIT_SIG_ROLES[i]) == 0) return REPROIT_SIG_ROLES[i];
    }
    return "node";
}

static bool reproit_sig_is_transient(const ReproItSig_Node* n) {
    if (n->transient) return true;
    if (!n->role) return false;
    for (size_t i = 0; i < sizeof REPROIT_SIG_TRANSIENT / sizeof *REPROIT_SIG_TRANSIENT; i++) {
        if (strcmp(n->role, REPROIT_SIG_TRANSIENT[i]) == 0) return true;
    }
    return false;
}

// A bounded append-only string buffer with truncation guard.
typedef struct {
    char buf[REPROIT_SIG_MAX_DESC];
    size_t len;
} ReproItSig_Buf;

static void reproit_sig_buf_init(ReproItSig_Buf* b) { b->len = 0; b->buf[0] = 0; }
static void reproit_sig_putc(ReproItSig_Buf* b, char c) {
    if (b->len + 1 < sizeof b->buf) { b->buf[b->len++] = c; b->buf[b->len] = 0; }
}
static void reproit_sig_puts(ReproItSig_Buf* b, const char* s) {
    if (!s) return;
    for (; *s; s++) reproit_sig_putc(b, *s);
}
static void reproit_sig_put_uint(ReproItSig_Buf* b, unsigned v) {
    char tmp[16];
    int i = 0;
    if (v == 0) { reproit_sig_putc(b, '0'); return; }
    while (v && i < (int)sizeof tmp) { tmp[i++] = (char)('0' + v % 10); v /= 10; }
    while (i--) reproit_sig_putc(b, tmp[i]);
}

// Emit one node's token body (everything after "<depth>:"), without the repeat
// marker: <role>[:<type>][#<icon>][@<id>]. Field order is fixed.
static void reproit_sig_token_body(const ReproItSig_Node* n, ReproItSig_Buf* b) {
    reproit_sig_puts(b, reproit_sig_normalize_role(n->role));
    if (!reproit_sig_str_empty(n->type)) { reproit_sig_putc(b, ':'); reproit_sig_puts(b, n->type); }
    if (!reproit_sig_str_empty(n->icon)) { reproit_sig_putc(b, '#'); reproit_sig_puts(b, n->icon); }
    if (!reproit_sig_str_empty(n->id))   { reproit_sig_putc(b, '@'); reproit_sig_puts(b, n->id); }
}

// Emit one token "<depth>:<body>[*]". `first` is true for the very first token
// emitted into `b` (no leading ';'); otherwise a ';' separates from the prior
// token. Returns false so callers can keep threading the "first" flag.
static bool reproit_sig_emit_token(const ReproItSig_Node* n, unsigned depth,
                                   bool repeated, bool first, ReproItSig_Buf* b) {
    if (!first) reproit_sig_putc(b, ';');
    reproit_sig_put_uint(b, depth);
    reproit_sig_putc(b, ':');
    reproit_sig_token_body(n, b);
    if (repeated) reproit_sig_putc(b, '*');
    return false;
}

// Build the canonical subtree descriptor for collapse comparison (rule 3): the
// pre-order token list of this subtree with depths re-based to start at 0, so
// two sibling subtrees at the same level compare equal regardless of absolute
// depth. Transients are dropped. Mirrors walk_key in the oracle.
static void reproit_sig_subtree_key(const ReproItSig_Node* n, unsigned depth,
                                    bool* first, ReproItSig_Buf* b) {
    *first = reproit_sig_emit_token(n, depth, false, *first, b);
    for (int i = 0; i < n->n_children; i++) {
        if (reproit_sig_is_transient(n->children[i])) continue;
        reproit_sig_subtree_key(n->children[i], depth + 1, first, b);
    }
}

static void reproit_sig_subtree_key_str(const ReproItSig_Node* n, char* out) {
    ReproItSig_Buf b;
    reproit_sig_buf_init(&b);
    bool first = true;
    reproit_sig_subtree_key(n, 0, &first, &b);
    memcpy(out, b.buf, b.len + 1);
}

// Forward decl: the serializer recurses through children-with-collapse.
static bool reproit_sig_serialize_node(const ReproItSig_Node* n, unsigned depth,
                                       bool repeated, bool first, ReproItSig_Buf* b);

// Walk a run of retained siblings, collapsing maximal runs of >= 2 consecutive
// children whose subtree_key is identical into one emission with the `*` marker
// (count dropped). Threads the "first token" flag through.
static bool reproit_sig_serialize_children(struct ReproItSig_Node* const* children,
                                           int n, unsigned depth, bool first,
                                           ReproItSig_Buf* b) {
    // Filter out transient children up front so collapse runs see only retained
    // siblings (a transient between two identical nodes must not break the run).
    const ReproItSig_Node* kept[REPROIT_SIG_MAX_CHILDREN];
    int nk = 0;
    for (int i = 0; i < n && nk < REPROIT_SIG_MAX_CHILDREN; i++) {
        if (!reproit_sig_is_transient(children[i])) kept[nk++] = children[i];
    }
    int i = 0;
    while (i < nk) {
        char key[REPROIT_SIG_MAX_DESC];
        reproit_sig_subtree_key_str(kept[i], key);
        int j = i + 1;
        while (j < nk) {
            char k2[REPROIT_SIG_MAX_DESC];
            reproit_sig_subtree_key_str(kept[j], k2);
            if (strcmp(k2, key) != 0) break;
            j++;
        }
        int run = j - i;
        first = reproit_sig_serialize_node(kept[i], depth, run >= 2, first, b);
        i = j;
    }
    return first;
}

// Emit one node's token (optionally repeat-marked) then recurse into children
// with collapse applied across the run.
static bool reproit_sig_serialize_node(const ReproItSig_Node* n, unsigned depth,
                                       bool repeated, bool first, ReproItSig_Buf* b) {
    first = reproit_sig_emit_token(n, depth, repeated, first, b);
    first = reproit_sig_serialize_children(n->children, n->n_children, depth + 1, first, b);
    return first;
}

// --- Layer 2: value-state (docs/signature.md "Value-state") ----------------

// Strict ^[+-]?[0-9]+(\.[0-9]+)?$: optional sign, one or more ASCII digits,
// optionally a period followed by one or more ASCII digits. No grouping, no
// exponent, no leading/trailing dot. Locale-safe by construction.
static bool reproit_sig_is_strict_decimal(const char* s) {
    size_t i = 0, len = strlen(s);
    if (i < len && (s[i] == '+' || s[i] == '-')) i++;
    size_t int_start = i;
    while (i < len && s[i] >= '0' && s[i] <= '9') i++;
    if (i == int_start) return false;            // need at least one integer digit
    if (i < len && s[i] == '.') {
        i++;
        size_t frac_start = i;
        while (i < len && s[i] >= '0' && s[i] <= '9') i++;
        if (i == frac_start) return false;       // trailing dot with no fraction
    }
    return i == len;
}

// Map a value string to a bounded, deterministic, locale-safe value-class token.
static const char* reproit_sig_value_class(const char* s) {
    if (!s) return "EMPTY";
    // Trim leading/trailing ASCII whitespace into a local copy of the core span.
    const char* a = s;
    while (*a == ' ' || *a == '\t' || *a == '\n' || *a == '\r' || *a == '\f' || *a == '\v') a++;
    const char* e = a + strlen(a);
    while (e > a) {
        char c = e[-1];
        if (c == ' ' || c == '\t' || c == '\n' || c == '\r' || c == '\f' || c == '\v') e--;
        else break;
    }
    size_t n = (size_t)(e - a);
    if (n == 0) return "EMPTY";
    char tmp[64];
    if (n >= sizeof tmp) return "NONEMPTY";      // too long to be our short numeric grammar
    memcpy(tmp, a, n);
    tmp[n] = 0;
    if (!reproit_sig_is_strict_decimal(tmp)) return "NONEMPTY";
    // Parse is safe: the grammar is a subset of strtod's accepted syntax.
    double v = strtod(tmp, NULL);
    double abs = v < 0 ? -v : v;
    if (v == 0.0) return "ZERO";
    if (v < 0.0) return "NEG";
    if (abs < 10.0) return "POS1";
    if (abs < 100.0) return "POS2";
    if (abs < 1000.0) return "POS3";
    return "POSL";
}

// True if this node carries a canonical value-class in the V: section: it has a
// non-NULL `value` AND it is value-bearing (RAW role in the value-role set OR
// value_node-flagged). The raw role is used deliberately (status/meter normalize
// to "node" but are still value-roles).
static bool reproit_sig_is_value_bearing(const ReproItSig_Node* n) {
    if (!n->value) return false;
    if (n->value_node) return true;
    if (!n->role) return false;
    for (size_t i = 0; i < sizeof REPROIT_SIG_VALUE_ROLES / sizeof *REPROIT_SIG_VALUE_ROLES; i++) {
        if (strcmp(n->role, REPROIT_SIG_VALUE_ROLES[i]) == 0) return true;
    }
    return false;
}

// Emit the V:-section key for a value-bearing node: "key:<id>" if it has an id,
// otherwise the structural fallback "role:<normalized-role>#<idx>".
static void reproit_sig_value_key(const ReproItSig_Node* n, unsigned idx, ReproItSig_Buf* b) {
    if (!reproit_sig_str_empty(n->id)) {
        reproit_sig_puts(b, "key:");
        reproit_sig_puts(b, n->id);
    } else {
        reproit_sig_puts(b, "role:");
        reproit_sig_puts(b, reproit_sig_normalize_role(n->role));
        reproit_sig_putc(b, '#');
        reproit_sig_put_uint(b, idx);
    }
}

// One collected V: entry: a "key=valueclass" string, kept for sorting by key.
typedef struct {
    char key[REPROIT_SIG_MAX_DESC];
    const char* cls;
} ReproItSig_VEntry;

// Collect (value_key, value_class) for every value-bearing node in pre-order,
// skipping transient subtrees (rule 2) so the V: section is consistent with the
// body. A keyless node's structural index is its position among same-(normalized-)
// role non-transient siblings under the same parent (the root gets index 0).
static void reproit_sig_collect_values(const ReproItSig_Node* n, unsigned idx,
                                       ReproItSig_VEntry* out, int* count, int cap) {
    if (reproit_sig_is_transient(n)) return;
    if (reproit_sig_is_value_bearing(n) && *count < cap) {
        ReproItSig_Buf kb;
        reproit_sig_buf_init(&kb);
        reproit_sig_value_key(n, idx, &kb);
        memcpy(out[*count].key, kb.buf, kb.len + 1);
        out[*count].cls = reproit_sig_value_class(n->value);
        (*count)++;
    }
    // Assign per-parent structural indices among same-normalized-role, non-
    // transient children, then recurse.
    const char* roles[REPROIT_SIG_MAX_CHILDREN];
    unsigned counts[REPROIT_SIG_MAX_CHILDREN];
    int nr = 0;
    for (int i = 0; i < n->n_children; i++) {
        const ReproItSig_Node* c = n->children[i];
        if (reproit_sig_is_transient(c)) continue;
        const char* role = reproit_sig_normalize_role(c->role);
        unsigned cidx = 0;
        int found = -1;
        for (int r = 0; r < nr; r++) if (strcmp(roles[r], role) == 0) { found = r; break; }
        if (found >= 0) { cidx = counts[found]; counts[found]++; }
        else if (nr < REPROIT_SIG_MAX_CHILDREN) { roles[nr] = role; counts[nr] = 1; nr++; }
        reproit_sig_collect_values(c, cidx, out, count, cap);
    }
}

// Build the V: section suffix. Returns nothing appended when there are NO value-
// bearing nodes, keeping the descriptor byte-identical to a pre-value-state tree
// (backward-compatible). Otherwise appends "\nV:" + key=class;... sorted by key.
static void reproit_sig_value_section(const ReproItSig_Node* root, ReproItSig_Buf* out) {
    static ReproItSig_VEntry entries[REPROIT_SIG_MAX_CHILDREN * 4];
    int cap = (int)(sizeof entries / sizeof *entries);
    int count = 0;
    reproit_sig_collect_values(root, 0, entries, &count, cap);
    if (count == 0) return;
    // Insertion sort by key (stable, small n).
    for (int i = 1; i < count; i++) {
        ReproItSig_VEntry tmp = entries[i];
        int j = i - 1;
        while (j >= 0 && strcmp(entries[j].key, tmp.key) > 0) { entries[j + 1] = entries[j]; j--; }
        entries[j + 1] = tmp;
    }
    reproit_sig_puts(out, "\nV:");
    for (int i = 0; i < count; i++) {
        if (i) reproit_sig_putc(out, ';');
        reproit_sig_puts(out, entries[i].key);
        reproit_sig_putc(out, '=');
        reproit_sig_puts(out, entries[i].cls);
    }
}

// Build the exact UTF-8 descriptor string that gets hashed:
//   "A:" + anchor + "\n" + tokens.join(";") + V-section
// The "A:" prefix line is always present, even with no anchor. If the root is
// transient (or there are no retained nodes) the body is empty. The V: section
// (Layer 2 value-classes) is appended only when at least one value-bearing node
// exists; otherwise the descriptor is byte-identical to a pre-value-state tree.
static void reproit_sig_descriptor(const char* anchor, const ReproItSig_Node* root,
                                   ReproItSig_Buf* out) {
    reproit_sig_buf_init(out);
    reproit_sig_puts(out, "A:");
    if (anchor) reproit_sig_puts(out, anchor);
    reproit_sig_putc(out, '\n');
    if (root && !reproit_sig_is_transient(root)) {
        bool first = true;
        reproit_sig_serialize_node(root, 0, false, first, out);
        reproit_sig_value_section(root, out);
    }
}

// FNV-1a 32-bit over the descriptor's UTF-8 bytes -> 8-char lowercase hex.
static void reproit_sig_fnv1a32_hex(const char* bytes, size_t len, char out[9]) {
    uint32_t h = 0x811c9dc5u;
    for (size_t i = 0; i < len; i++) { h ^= (unsigned char)bytes[i]; h *= 0x01000193u; }
    snprintf(out, 9, "%08x", h);
}

// Public: compute the canonical 8-char hex signature for (anchor, tree).
static void ReproIt_Signature(const char* anchor, const ReproItSig_Node* root, char out[9]) {
    ReproItSig_Buf desc;
    reproit_sig_descriptor(anchor, root, &desc);
    reproit_sig_fnv1a32_hex(desc.buf, desc.len, out);
}

#endif  // REPROIT_SIGNATURE_CORE_H

// ===========================================================================
// PRODUCTION TELEMETRY CORE (optional; OFF by default)
//
// IDENTICAL to the telemetry core block in runners/reproit_clay.h (shared guard,
// so if both headers land in one TU the telemetry core appears once). This block
// is COMPLETELY SEPARATE from the signature core above and from the fuzz drivers
// below: it is compiled only when REPROIT_TELEMETRY is defined, and it never
// touches the parity-critical signature core (it only CALLS the public
// ReproIt_Signature). So fuzz behavior is byte-for-byte unchanged when telemetry
// is off, and the signature core stays parity-tested.
//
// What it does: a shipped immediate-mode app, built with -DREPROIT_TELEMETRY=1,
// reports REAL production sessions as the same usage graph the fuzzer produces.
// Each frame (or sampled) it computes the canonical signature via the existing
// core, tracks the current edge/action path, and buffers events. A user-supplied
// transport callback (or the built-in newline-delimited-JSON spool transport)
// ships batches to the cloud in the SAME contract the other SDKs POST:
//   { "appId": <str>, "sentAt": <ms>, "ctx": <raw-json-or-omitted>, "events": [...] }
// Event shapes mirror sdk/reproit-web.js:
//   edge:  {"kind":"edge","from":<sig|null>,"action":<str>,"to":<sig>,"t":<ms>}
//   state: {"kind":"state","sig":<sig>,"t":<ms>}
//   error: {"kind":"error","sig":<sig|null>,"path":[{"sig","action"}...],
//           "message":<str>,"t":<ms>}
//
// A crash hook (REPROIT_TELEMETRY_CRASH, on by default in telemetry mode)
// installs SIGSEGV/SIGABRT/SIGBUS/SIGFPE/SIGILL handlers that flush the LAST
// signature + edge path before exit. The handler is async-signal-safe: the crash
// payload is PRE-SERIALIZED into a fixed static buffer on every frame, so the
// handler does no malloc, no printf, no buffered-stdio: it only does a single
// async-signal-safe write(2) to a fixed FD (the spool), then re-raises.
// ===========================================================================
#if defined(REPROIT_TELEMETRY) && !defined(REPROIT_TELEMETRY_CORE_H)
#define REPROIT_TELEMETRY_CORE_H
#include <signal.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#ifndef REPROIT_TELE_MAX_EVENTS
#define REPROIT_TELE_MAX_EVENTS 64        // events buffered before an auto-flush
#endif
#ifndef REPROIT_TELE_EVENT_CAP
#define REPROIT_TELE_EVENT_CAP 512        // bytes per serialized event line
#endif
#ifndef REPROIT_TELE_PATH_CAP
#define REPROIT_TELE_PATH_CAP 60          // graph-trail entries kept for a repro
#endif
#ifndef REPROIT_TELE_CRASH_CAP
#define REPROIT_TELE_CRASH_CAP 4096       // pre-serialized crash payload buffer
#endif

// The transport callback contract. The telemetry layer hands you ONE fully
// serialized JSON batch (already in the {appId,sentAt,ctx?,events} envelope, a
// NUL-terminated UTF-8 string of `len` bytes) plus the `user` pointer you set at
// init. You ship it however you like (libcurl POST, a queue, a file). Return is
// ignored. The callback is invoked from reproit telemetry flush points (frame
// flush / explicit flush), NEVER from the crash signal handler (the crash path
// uses only the async-signal-safe spool write, see below), so your transport may
// allocate/use stdio freely. It must NOT call back into the reproit telemetry API.
typedef void (*ReproIt_TransportFn)(const char* json, size_t len, void* user);

// Telemetry init options. Zero-initialize then set fields; appId/endpoint are
// borrowed (kept by pointer, so use string literals or stable storage).
typedef struct {
    const char* appId;             // required; defaults to "app" if NULL
    ReproIt_TransportFn transport; // optional; NULL => built-in spool transport
    void* transportUser;           // opaque, passed to transport
    const char* ctxJson;           // optional raw JSON object/string for "ctx" (no validation); NULL => omit
    const char* spoolPath;         // built-in transport target; NULL => env REPROIT_TELEMETRY_SPOOL, else stderr fd
    bool installCrashHook;         // install SIGSEGV/SIGABRT/... handlers (default true via init helper)
    bool sampleEnabled;            // host decides sampling; false => telemetry no-ops
} ReproIt_TeleOptions;

// One graph-trail entry kept for crash repros: (signature, action that led here).
typedef struct {
    char sig[9];
    char action[64];
} ReproIt_TelePathEntry;

typedef struct {
    bool active;                                   // init() called and sampling on
    char appId[128];
    ReproIt_TransportFn transport;
    void* transportUser;
    char ctxJson[512];
    bool hasCtx;

    // serialized event lines, flushed as one batch
    char events[REPROIT_TELE_MAX_EVENTS][REPROIT_TELE_EVENT_CAP];
    int nEvents;

    // current state + graph trail (for edges and the crash repro path)
    char curSig[9];
    bool hasCur;
    ReproIt_TelePathEntry path[REPROIT_TELE_PATH_CAP];
    int nPath;

    // built-in spool transport target (also the crash-hook write target)
    int spoolFd;
    bool spoolOwned;                               // we opened it, so close on flush-exit

    // PRE-SERIALIZED crash payload, rebuilt on every state change so the signal
    // handler can write it without touching the heap or stdio.
    char crashBuf[REPROIT_TELE_CRASH_CAP];
    size_t crashLen;
    bool crashHookInstalled;
} ReproIt_TeleState;

// Single telemetry instance. `static` so each TU that compiles the telemetry core
// gets its own; the shared guard means only one TU compiles it when both headers
// are present.
static ReproIt_TeleState reproit_tele;

// ---- small async-signal-safe-friendly serializers -------------------------
// (Used both on the hot path and, for the crash buffer, pre-serialized off the
// signal handler. The handler itself only calls write(2).)

// Append src to dst[cap] at *len with NUL term, truncating safely. Returns false
// if truncated. No allocation.
static bool reproit_tele_append(char* dst, size_t cap, size_t* len, const char* src) {
    if (!src) return true;
    size_t i = 0;
    while (src[i] && *len + 1 < cap) { dst[(*len)++] = src[i++]; }
    dst[*len < cap ? *len : cap - 1] = 0;
    return src[i] == 0;
}

// Append a JSON string literal value WITH surrounding quotes, escaping the JSON
// control set (" \\ and the C0 controls). Deterministic, no locale, no alloc.
static void reproit_tele_append_jstr(char* dst, size_t cap, size_t* len, const char* s) {
    if (*len + 1 < cap) dst[(*len)++] = '"';
    for (const char* p = s ? s : ""; *p && *len + 2 < cap; p++) {
        unsigned char c = (unsigned char)*p;
        if (c == '"' || c == '\\') { dst[(*len)++] = '\\'; dst[(*len)++] = (char)c; }
        else if (c == '\n') { dst[(*len)++] = '\\'; dst[(*len)++] = 'n'; }
        else if (c == '\r') { dst[(*len)++] = '\\'; dst[(*len)++] = 'r'; }
        else if (c == '\t') { dst[(*len)++] = '\\'; dst[(*len)++] = 't'; }
        else if (c < 0x20) { /* drop other control bytes */ }
        else dst[(*len)++] = (char)c;
    }
    if (*len + 1 < cap) dst[(*len)++] = '"';
    dst[*len < cap ? *len : cap - 1] = 0;
}

// Append an unsigned 64-bit integer in decimal. No alloc.
static void reproit_tele_append_u64(char* dst, size_t cap, size_t* len, uint64_t v) {
    char tmp[24];
    int i = 0;
    if (v == 0) tmp[i++] = '0';
    while (v && i < (int)sizeof tmp) { tmp[i++] = (char)('0' + (int)(v % 10)); v /= 10; }
    while (i-- && *len + 1 < cap) dst[(*len)++] = tmp[i];
    dst[*len < cap ? *len : cap - 1] = 0;
}

// Wall-clock milliseconds for the "t"/"sentAt" fields. time() is async-signal-
// safe per POSIX; we only use second precision *1000 to stay portable.
static uint64_t reproit_tele_now_ms(void) {
    return (uint64_t)time(NULL) * 1000ull;
}

// Rebuild the PRE-SERIALIZED crash payload from the current state + path, so the
// signal handler can emit it with a single write(2). Called on the hot path only
// (never from the handler). The crash event is a complete batch envelope so the
// spooled line is a self-contained record even though the process is dying.
static void reproit_tele_build_crash(const char* signame) {
    char* b = reproit_tele.crashBuf;
    size_t cap = sizeof reproit_tele.crashBuf;
    size_t n = 0;
    reproit_tele_append(b, cap, &n, "{\"appId\":");
    reproit_tele_append_jstr(b, cap, &n, reproit_tele.appId);
    reproit_tele_append(b, cap, &n, ",\"sentAt\":");
    reproit_tele_append_u64(b, cap, &n, reproit_tele_now_ms());
    if (reproit_tele.hasCtx) {
        reproit_tele_append(b, cap, &n, ",\"ctx\":");
        reproit_tele_append(b, cap, &n, reproit_tele.ctxJson);
    }
    reproit_tele_append(b, cap, &n, ",\"events\":[{\"kind\":\"error\",\"sig\":");
    if (reproit_tele.hasCur) reproit_tele_append_jstr(b, cap, &n, reproit_tele.curSig);
    else reproit_tele_append(b, cap, &n, "null");
    reproit_tele_append(b, cap, &n, ",\"message\":");
    reproit_tele_append_jstr(b, cap, &n, signame ? signame : "crash");
    reproit_tele_append(b, cap, &n, ",\"path\":[");
    for (int i = 0; i < reproit_tele.nPath; i++) {
        if (i) reproit_tele_append(b, cap, &n, ",");
        reproit_tele_append(b, cap, &n, "{\"sig\":");
        reproit_tele_append_jstr(b, cap, &n, reproit_tele.path[i].sig);
        reproit_tele_append(b, cap, &n, ",\"action\":");
        reproit_tele_append_jstr(b, cap, &n, reproit_tele.path[i].action);
        reproit_tele_append(b, cap, &n, "}");
    }
    reproit_tele_append(b, cap, &n, "]}]}\n");
    reproit_tele.crashLen = n;
}

// Flush the buffered events as ONE batch through the transport (or the built-in
// spool transport when none was set). Safe to call any time on the hot path; a
// no-op when there is nothing buffered. Not called from the signal handler.
static void reproit_tele_flush(void) {
    if (reproit_tele.nEvents == 0) return;
    // Build the {appId,sentAt,ctx?,events:[...]} envelope into a heap batch buffer.
    size_t cap = (size_t)reproit_tele.nEvents * REPROIT_TELE_EVENT_CAP + 1024;
    char* batch = (char*)malloc(cap);
    if (!batch) { reproit_tele.nEvents = 0; return; }
    size_t n = 0;
    reproit_tele_append(batch, cap, &n, "{\"appId\":");
    reproit_tele_append_jstr(batch, cap, &n, reproit_tele.appId);
    reproit_tele_append(batch, cap, &n, ",\"sentAt\":");
    reproit_tele_append_u64(batch, cap, &n, reproit_tele_now_ms());
    if (reproit_tele.hasCtx) {
        reproit_tele_append(batch, cap, &n, ",\"ctx\":");
        reproit_tele_append(batch, cap, &n, reproit_tele.ctxJson);
    }
    reproit_tele_append(batch, cap, &n, ",\"events\":[");
    for (int i = 0; i < reproit_tele.nEvents; i++) {
        if (i) reproit_tele_append(batch, cap, &n, ",");
        reproit_tele_append(batch, cap, &n, reproit_tele.events[i]);
    }
    reproit_tele_append(batch, cap, &n, "]}");

    if (reproit_tele.transport) {
        reproit_tele.transport(batch, n, reproit_tele.transportUser);
    } else if (reproit_tele.spoolFd >= 0) {
        // Built-in transport: newline-delimited JSON to the spool fd.
        size_t off = 0;
        while (off < n) {
            long w = (long)write(reproit_tele.spoolFd, batch + off, n - off);
            if (w <= 0) break;
            off += (size_t)w;
        }
        if (write(reproit_tele.spoolFd, "\n", 1) < 0) { /* best-effort */ }
    }
    free(batch);
    reproit_tele.nEvents = 0;
}

// Push one already-serialized event line into the buffer, auto-flushing when full.
static void reproit_tele_push(const char* line) {
    if (reproit_tele.nEvents >= REPROIT_TELE_MAX_EVENTS) reproit_tele_flush();
    snprintf(reproit_tele.events[reproit_tele.nEvents], REPROIT_TELE_EVENT_CAP, "%s", line);
    reproit_tele.nEvents++;
}

// The crash signal handler. Async-signal-safe: it does NOT allocate, format, or
// touch buffered stdio. It writes the PRE-SERIALIZED crashBuf (rebuilt each frame)
// with a single write(2) to the spool fd, then restores the default disposition
// and re-raises so the OS still produces a core dump / the parent sees the signal.
static void reproit_tele_crash_handler(int sig) {
    if (reproit_tele.crashLen > 0 && reproit_tele.spoolFd >= 0) {
        size_t off = 0;
        while (off < reproit_tele.crashLen) {
            long w = (long)write(reproit_tele.spoolFd, reproit_tele.crashBuf + off,
                                 reproit_tele.crashLen - off);
            if (w <= 0) break;
            off += (size_t)w;
        }
    }
    signal(sig, SIG_DFL);
    raise(sig);
}

static void reproit_tele_install_crash_hook(void) {
    if (reproit_tele.crashHookInstalled) return;
    reproit_tele.crashHookInstalled = true;
    signal(SIGSEGV, reproit_tele_crash_handler);
    signal(SIGABRT, reproit_tele_crash_handler);
    signal(SIGFPE,  reproit_tele_crash_handler);
    signal(SIGILL,  reproit_tele_crash_handler);
#ifdef SIGBUS
    signal(SIGBUS,  reproit_tele_crash_handler);
#endif
}

// Initialize telemetry. After this, the per-frame hooks below feed states/edges.
// Returns true if telemetry is active (sampling on), false if it no-ops.
static bool ReproIt_Telemetry_Init(const ReproIt_TeleOptions* opt) {
    memset(&reproit_tele, 0, sizeof reproit_tele);
    reproit_tele.spoolFd = -1;
    if (!opt || !opt->sampleEnabled) return false;
    reproit_tele.active = true;
    snprintf(reproit_tele.appId, sizeof reproit_tele.appId, "%s",
             opt->appId ? opt->appId : "app");
    reproit_tele.transport = opt->transport;
    reproit_tele.transportUser = opt->transportUser;
    if (opt->ctxJson && opt->ctxJson[0]) {
        snprintf(reproit_tele.ctxJson, sizeof reproit_tele.ctxJson, "%s", opt->ctxJson);
        reproit_tele.hasCtx = true;
    }
    // Built-in spool transport: only opened when no custom transport is supplied.
    if (!reproit_tele.transport) {
        const char* path = opt->spoolPath;
        if (!path || !path[0]) path = getenv("REPROIT_TELEMETRY_SPOOL");
        if (path && path[0]) {
            FILE* f = fopen(path, "ab");
            if (f) { reproit_tele.spoolFd = fileno(f); reproit_tele.spoolOwned = true; }
        }
        if (reproit_tele.spoolFd < 0) reproit_tele.spoolFd = 2;  // fall back to stderr
    }
    if (opt->installCrashHook) reproit_tele_install_crash_hook();
    return true;
}

// Record the current frame's signature. Computes the canonical signature with the
// EXISTING core (ReproIt_Signature) over the caller-built tree, emits a state
// event the first time a signature is seen-as-current, and an edge event when the
// signature changes (carrying the action that caused the transition). The crash
// payload is rebuilt here so the signal handler always has the latest path. Pass
// the action that led to this frame (e.g. "tap:play"), or NULL for "auto".
static void ReproIt_Telemetry_Observe(const char* anchor, const ReproItSig_Node* root,
                                      const char* action) {
    if (!reproit_tele.active) return;
    char sig[9];
    ReproIt_Signature(anchor, root, sig);
    if (reproit_tele.hasCur && strcmp(sig, reproit_tele.curSig) == 0) return;  // no change

    char from[9];
    bool hadFrom = reproit_tele.hasCur;
    if (hadFrom) memcpy(from, reproit_tele.curSig, sizeof from);
    memcpy(reproit_tele.curSig, sig, sizeof reproit_tele.curSig);
    reproit_tele.hasCur = true;

    // Append to the graph trail (capped, oldest-dropped) for the crash repro path.
    if (reproit_tele.nPath >= REPROIT_TELE_PATH_CAP) {
        memmove(&reproit_tele.path[0], &reproit_tele.path[1],
                sizeof reproit_tele.path[0] * (REPROIT_TELE_PATH_CAP - 1));
        reproit_tele.nPath--;
    }
    snprintf(reproit_tele.path[reproit_tele.nPath].sig, 9, "%s", sig);
    snprintf(reproit_tele.path[reproit_tele.nPath].action, 64, "%s", action ? action : "auto");
    reproit_tele.nPath++;

    // state event
    {
        char line[REPROIT_TELE_EVENT_CAP];
        size_t n = 0;
        reproit_tele_append(line, sizeof line, &n, "{\"kind\":\"state\",\"sig\":");
        reproit_tele_append_jstr(line, sizeof line, &n, sig);
        reproit_tele_append(line, sizeof line, &n, ",\"t\":");
        reproit_tele_append_u64(line, sizeof line, &n, reproit_tele_now_ms());
        reproit_tele_append(line, sizeof line, &n, "}");
        reproit_tele_push(line);
    }
    // edge event (only after we have a prior state)
    if (hadFrom) {
        char line[REPROIT_TELE_EVENT_CAP];
        size_t n = 0;
        reproit_tele_append(line, sizeof line, &n, "{\"kind\":\"edge\",\"from\":");
        reproit_tele_append_jstr(line, sizeof line, &n, from);
        reproit_tele_append(line, sizeof line, &n, ",\"action\":");
        reproit_tele_append_jstr(line, sizeof line, &n, action ? action : "auto");
        reproit_tele_append(line, sizeof line, &n, ",\"to\":");
        reproit_tele_append_jstr(line, sizeof line, &n, sig);
        reproit_tele_append(line, sizeof line, &n, ",\"t\":");
        reproit_tele_append_u64(line, sizeof line, &n, reproit_tele_now_ms());
        reproit_tele_append(line, sizeof line, &n, "}");
        reproit_tele_push(line);
    }
    reproit_tele_build_crash("crash");
}

// Explicitly report an error/crash with the current state + path (for caught
// exceptions or an app-level error oracle). Buffers an error event and flushes.
static void ReproIt_Telemetry_Error(const char* message) {
    if (!reproit_tele.active) return;
    char line[REPROIT_TELE_EVENT_CAP];
    size_t n = 0;
    reproit_tele_append(line, sizeof line, &n, "{\"kind\":\"error\",\"sig\":");
    if (reproit_tele.hasCur) reproit_tele_append_jstr(line, sizeof line, &n, reproit_tele.curSig);
    else reproit_tele_append(line, sizeof line, &n, "null");
    reproit_tele_append(line, sizeof line, &n, ",\"message\":");
    reproit_tele_append_jstr(line, sizeof line, &n, message ? message : "error");
    reproit_tele_append(line, sizeof line, &n, ",\"t\":");
    reproit_tele_append_u64(line, sizeof line, &n, reproit_tele_now_ms());
    reproit_tele_append(line, sizeof line, &n, "}");
    reproit_tele_push(line);
    reproit_tele_flush();
}

// Flush + tear down (close an owned spool fd). Call at clean shutdown.
static void ReproIt_Telemetry_Shutdown(void) {
    if (!reproit_tele.active) return;
    reproit_tele_flush();
    if (reproit_tele.spoolOwned && reproit_tele.spoolFd >= 0) close(reproit_tele.spoolFd);
    reproit_tele.active = false;
}

#endif  // REPROIT_TELEMETRY && !REPROIT_TELEMETRY_CORE_H

// ===========================================================================
// SCREENSHOT-CAPTURE CORE (always available; capture itself is opt-in)
//
// IDENTICAL to the capture core block in runners/reproit_clay.h (shared guard,
// so if both headers land in one TU the capture core appears once). Self-
// contained plain C: it depends on nothing but libc, and like the signature
// core it never touches the parity-critical descriptor/hash logic, so the
// parity test (runners/test_signature.c) is unaffected.
//
// THE CONTRACT (orchestrator side, crates/reproit/src/backends/drive.rs): the
// orchestrator exports env REPROIT_SHOTS_DIR (an absolute directory). On a named
// shoot the in-app hook writes "$REPROIT_SHOTS_DIR/<name>.png" then prints
// "SHOOT:<name>\n" to stdout; the orchestrator confirms the file exists and logs
// it. <name> is restricted to [A-Za-z0-9_/-] (the orchestrator filters anything
// else out of the marker, so we sanitize to the same charset here to keep the
// emitted name and the written path in agreement).
//
// THE CAPTURE SEAM: this header cannot know the app's graphics backend
// (OpenGL/D3D/Metal/Vulkan/software), so the app supplies a callback that grabs
// the current framebuffer and writes a PNG to the given path:
//   typedef bool (*ReproIt_CaptureFn)(const char* png_path, void* user);
// Set it once with ReproIt_SetCaptureFn(fn, user). A DEFAULT OpenGL
// implementation is provided behind -DREPROIT_CAPTURE_GL (glReadPixels of the
// current viewport, written via the self-contained PNG encoder below); when that
// flag is set and no callback was registered, the GL default is used.
//
// THE PNG ENCODER: a minimal, dependency-free, self-contained writer using zlib
// "stored" (uncompressed) blocks, so it needs no libz. It emits a valid 8-bit
// RGBA PNG (with correct CRC32 + Adler32), which any PNG reader accepts. It is
// drop-in: no app encoder required. (If you prefer your own encoder, register a
// callback and ignore this one.)
// ===========================================================================
#ifndef REPROIT_CAPTURE_CORE_H
#define REPROIT_CAPTURE_CORE_H
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

// App-supplied capture callback: grab the current framebuffer and write a PNG to
// `png_path`. Return true on success. `user` is the opaque pointer registered
// alongside the callback. Invoked synchronously from ReproIt_Shoot().
typedef bool (*ReproIt_CaptureFn)(const char* png_path, void* user);

// Single registered capture hook (static per TU; the shared guard means only one
// TU compiles this block when both headers are present).
typedef struct {
    ReproIt_CaptureFn fn;
    void* user;
} ReproIt_CaptureState;
static ReproIt_CaptureState reproit_capture = {0, 0};

static void ReproIt_SetCaptureFn(ReproIt_CaptureFn fn, void* user) {
    reproit_capture.fn = fn;
    reproit_capture.user = user;
}

// ---- minimal self-contained PNG (zlib "stored" / uncompressed) -------------
// No external dependency: deflate is emitted as raw stored blocks, which any
// conformant zlib/PNG decoder reads. CRC32 + Adler32 are computed inline.

static uint32_t reproit_png_crc_table[256];
static bool reproit_png_crc_ready = false;
static void reproit_png_crc_init(void) {
    if (reproit_png_crc_ready) return;
    for (uint32_t n = 0; n < 256; n++) {
        uint32_t c = n;
        for (int k = 0; k < 8; k++) c = (c & 1) ? 0xedb88320u ^ (c >> 1) : (c >> 1);
        reproit_png_crc_table[n] = c;
    }
    reproit_png_crc_ready = true;
}
static uint32_t reproit_png_crc(uint32_t crc, const unsigned char* buf, size_t len) {
    reproit_png_crc_init();
    crc ^= 0xffffffffu;
    for (size_t i = 0; i < len; i++) crc = reproit_png_crc_table[(crc ^ buf[i]) & 0xff] ^ (crc >> 8);
    return crc ^ 0xffffffffu;
}

static void reproit_png_be32(unsigned char* p, uint32_t v) {
    p[0] = (unsigned char)(v >> 24); p[1] = (unsigned char)(v >> 16);
    p[2] = (unsigned char)(v >> 8);  p[3] = (unsigned char)v;
}

// Write one PNG chunk (length + type + data + CRC) to `f`.
static bool reproit_png_chunk(FILE* f, const char* type, const unsigned char* data, size_t len) {
    unsigned char hdr[8];
    reproit_png_be32(hdr, (uint32_t)len);
    memcpy(hdr + 4, type, 4);
    if (fwrite(hdr, 1, 8, f) != 8) return false;
    if (len && fwrite(data, 1, len, f) != len) return false;
    uint32_t crc = reproit_png_crc(0, (const unsigned char*)type, 4);
    if (len) crc = reproit_png_crc(crc, data, len);
    unsigned char crcb[4];
    reproit_png_be32(crcb, crc);
    return fwrite(crcb, 1, 4, f) == 4;
}

// Write an 8-bit RGBA PNG. `pixels` is `w*h*4` bytes, top row first (the caller
// orders rows; the GL default below flips bottom-up reads). Returns true on
// success. Uses an uncompressed zlib stream so no compressor is needed.
static bool ReproIt_WritePNG_RGBA(const char* path, const unsigned char* pixels,
                                  unsigned w, unsigned h) {
    if (!path || !pixels || w == 0 || h == 0) return false;
    FILE* f = fopen(path, "wb");
    if (!f) return false;

    static const unsigned char SIG[8] = {137, 80, 78, 71, 13, 10, 26, 10};
    bool ok = fwrite(SIG, 1, 8, f) == 8;

    // IHDR: width, height, bit depth 8, color type 6 (RGBA), no compression/
    // filter/interlace method bytes (all 0).
    unsigned char ihdr[13];
    reproit_png_be32(ihdr, w);
    reproit_png_be32(ihdr + 4, h);
    ihdr[8] = 8; ihdr[9] = 6; ihdr[10] = 0; ihdr[11] = 0; ihdr[12] = 0;
    ok = ok && reproit_png_chunk(f, "IHDR", ihdr, sizeof ihdr);

    // Build the raw (pre-zlib) image data: each scanline prefixed with filter
    // byte 0 (none). Then wrap in a zlib stream of stored deflate blocks.
    size_t row = (size_t)w * 4;
    size_t raw_len = (row + 1) * h;                 // +1 filter byte per row
    unsigned char* raw = (unsigned char*)malloc(raw_len);
    if (!raw) { fclose(f); return false; }
    for (unsigned y = 0; y < h; y++) {
        raw[(row + 1) * y] = 0;                      // filter: none
        memcpy(raw + (row + 1) * y + 1, pixels + row * y, row);
    }

    // zlib stream: 2-byte header (0x78 0x01) + stored deflate blocks + Adler32.
    // Each stored block carries up to 65535 bytes: 1 BFINAL/BTYPE byte, LEN,
    // ~LEN (little-endian), then the literal bytes.
    size_t max_blocks = raw_len / 65535 + 1;
    size_t zcap = 2 + raw_len + max_blocks * 5 + 4;
    unsigned char* z = (unsigned char*)malloc(zcap);
    if (!z) { free(raw); fclose(f); return false; }
    size_t zn = 0;
    z[zn++] = 0x78; z[zn++] = 0x01;                  // CMF, FLG
    size_t off = 0;
    while (off < raw_len) {
        size_t block = raw_len - off;
        if (block > 65535) block = 65535;
        int final = (off + block >= raw_len) ? 1 : 0;
        z[zn++] = (unsigned char)final;              // BFINAL bit, BTYPE=00 (stored)
        z[zn++] = (unsigned char)(block & 0xff);
        z[zn++] = (unsigned char)((block >> 8) & 0xff);
        z[zn++] = (unsigned char)(~block & 0xff);
        z[zn++] = (unsigned char)((~block >> 8) & 0xff);
        memcpy(z + zn, raw + off, block);
        zn += block;
        off += block;
    }
    // Adler32 over the raw (uncompressed) data.
    uint32_t a = 1, b = 0;
    for (size_t i = 0; i < raw_len; i++) {
        a = (a + raw[i]) % 65521u;
        b = (b + a) % 65521u;
    }
    uint32_t adler = (b << 16) | a;
    reproit_png_be32(z + zn, adler);
    zn += 4;
    free(raw);

    ok = ok && reproit_png_chunk(f, "IDAT", z, zn);
    free(z);
    ok = ok && reproit_png_chunk(f, "IEND", NULL, 0);
    fclose(f);
    if (!ok) remove(path);
    return ok;
}

#ifdef REPROIT_CAPTURE_GL
// Default OpenGL capture: glReadPixels of the current viewport, written as PNG.
// The app must include its GL headers BEFORE this header (or define the GL
// prototypes); we only call glGetIntegerv/glReadPixels/GL_RGBA/GL_UNSIGNED_BYTE,
// which are core in every desktop/ES profile. GL reads bottom-up, so we flip
// rows into top-down order for the PNG.
static bool reproit_capture_gl(const char* png_path, void* user) {
    (void)user;
    GLint vp[4] = {0, 0, 0, 0};
    glGetIntegerv(GL_VIEWPORT, vp);
    unsigned w = (unsigned)(vp[2] > 0 ? vp[2] : 0);
    unsigned h = (unsigned)(vp[3] > 0 ? vp[3] : 0);
    if (w == 0 || h == 0) return false;
    size_t row = (size_t)w * 4;
    unsigned char* buf = (unsigned char*)malloc(row * h);
    if (!buf) return false;
    glPixelStorei(GL_PACK_ALIGNMENT, 1);
    glReadPixels(vp[0], vp[1], (GLsizei)w, (GLsizei)h, GL_RGBA, GL_UNSIGNED_BYTE, buf);
    // Flip vertically: GL row 0 is the bottom; PNG row 0 is the top.
    unsigned char* flip = (unsigned char*)malloc(row * h);
    if (!flip) { free(buf); return false; }
    for (unsigned y = 0; y < h; y++) memcpy(flip + row * y, buf + row * (h - 1 - y), row);
    free(buf);
    bool ok = ReproIt_WritePNG_RGBA(png_path, flip, w, h);
    free(flip);
    return ok;
}
#endif  // REPROIT_CAPTURE_GL

// Sanitize a shoot name to the orchestrator's charset [A-Za-z0-9_/-], in place,
// into `out` (capacity `cap`). Anything else is dropped, so the emitted marker
// name and the written file path agree with drive.rs's filter.
static void reproit_shoot_sanitize(const char* name, char* out, size_t cap) {
    size_t j = 0;
    for (const char* p = name ? name : ""; *p && j + 1 < cap; p++) {
        char c = *p;
        if ((c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z') ||
            (c >= '0' && c <= '9') || c == '_' || c == '/' || c == '-') {
            out[j++] = c;
        }
    }
    out[j] = 0;
}

// Perform a named shoot: build "$REPROIT_SHOTS_DIR/<name>.png", invoke the
// registered capture callback (or the GL default when -DREPROIT_CAPTURE_GL and no
// callback is set), then print "SHOOT:<name>" so the orchestrator confirms + logs
// it. No-op (returns false) when REPROIT_SHOTS_DIR is unset, the name sanitizes
// empty, or no capture mechanism is available. The marker is printed ONLY when
// the PNG was actually written, so the orchestrator's path.exists() check holds.
static bool ReproIt_Shoot(const char* name) {
    const char* dir = getenv("REPROIT_SHOTS_DIR");
    if (!dir || !dir[0]) return false;
    char safe[256];
    reproit_shoot_sanitize(name, safe, sizeof safe);
    if (!safe[0]) return false;

    char path[1024];
    int np = snprintf(path, sizeof path, "%s/%s.png", dir, safe);
    if (np <= 0 || (size_t)np >= sizeof path) return false;

    bool ok = false;
    if (reproit_capture.fn) {
        ok = reproit_capture.fn(path, reproit_capture.user);
    }
#ifdef REPROIT_CAPTURE_GL
    else {
        ok = reproit_capture_gl(path, NULL);
    }
#endif
    if (!ok) return false;
    printf("SHOOT:%s\n", safe);
    fflush(stdout);
    return true;
}

#endif  // REPROIT_CAPTURE_CORE_H

// ===========================================================================
// ImGui-specific hook
// ===========================================================================
#ifndef REPROIT_IMGUI_H
#define REPROIT_IMGUI_H
#include "imgui.h"

namespace reproit {
void Frame();                         // call after ImGui::NewFrame()
void FrameEnd();                      // call before ImGui::Render()
bool Done();                          // true once the budget is spent

// Window / screen scope. Begin() opens a window: its id becomes the screen
// anchor and a group node opens for its contents; End() closes it. Use these in
// place of ImGui::Begin/ImGui::End for the window you want fuzzed.
bool Begin(const char* name);
void End();

// Generic container scope (a layout group / child region). Optional: use to
// nest structure (BeginGroup/EndGroup, BeginChild/EndChild equivalents).
void BeginScope(const char* role, const char* idLabel = nullptr);
void EndScope();

// Interactive + content wrappers. Each takes an ImGui-style label; the stable
// id is the substring after "##" (or the whole label if none), and the display
// text before "##" is excluded from the signature.
bool Button(const char* label);
bool MenuItem(const char* label, const char* shortcut = nullptr, bool selected = false, bool enabled = true);
bool Selectable(const char* label, bool selected = false);
bool Checkbox(const char* label, bool* v);
bool SliderFloat(const char* label, float* v, float vmin, float vmax);
bool InputText(const char* label, char* buf, size_t bufSize);
void Text(const char* label);                 // content text node
void Header(const char* label);               // a header/section title node

// ReproIt value-display hook: mark an app-computed output as a value node so its
// bounded value-class folds into the canonical signature (a calculator display, a
// counter, a stopwatch). The label's stable id keys the V: entry; `value` is the
// raw displayed string (bucketed locale-safely by value_class). This is the
// immediate-mode equivalent of a `value_nodes:` selector (Layer 3); the node's
// role is "output" (a value-role) so it is value-bearing by role.
void Value(const char* label, const char* value);

// ReproIt accessibility declaration (operability/accessibility graph). Immediate-
// mode GUIs emit NOTHING to any OS accessibility channel, so graph 2 is EMPTY by
// construction: every interactive widget the header tracks is reported each frame
// in an EXPLORE:GROUNDTRUTH marker as operable:true with a11y all-false, i.e. the
// whole interactive surface is honestly a gap (no role, no name, not in tab order,
// not keyboard-activatable). To SHRINK that gap, the app declares a widget's
// accessibility BEFORE FrameEnd: A11y("Play##play", "button", "Play", true) marks
// it as exposing a role + name to AT, so rolePresent/namePresent become true for
// that widget and the engine no longer counts it as a no_role/unlabeled gap.
// `idLabel` matches the widget's label (same "##" stable-id rule); `role`/`name`
// may be NULL; `exposed` defaults false (declared but not actually exposed). The
// gap is thus MEASURABLE and shrinkable, never silently hidden.
void A11y(const char* idLabel, const char* role = nullptr,
          const char* name = nullptr, bool exposed = false);

// --- Screenshot capture (the shoot contract; see the capture core above) ----
// Register the framebuffer-capture callback the app wires to its renderer (the
// header cannot know the graphics backend). With -DREPROIT_CAPTURE_GL and no
// callback set, the default glReadPixels path is used. Shoot("name") writes
// "$REPROIT_SHOTS_DIR/name.png" via the callback/GL default and prints
// "SHOOT:name"; it is a guarded no-op when REPROIT_SHOTS_DIR is unset, so
// capture-off builds (and runs without the env var) are unaffected.
inline void SetCaptureFn(ReproIt_CaptureFn fn, void* user) { ReproIt_SetCaptureFn(fn, user); }
inline bool Shoot(const char* name) { return ReproIt_Shoot(name); }

#ifdef REPROIT_TELEMETRY
// --- Production telemetry (optional; OFF unless REPROIT_TELEMETRY is defined) --
// In a SHIPPED build, the same reproit:: wrappers build the canonical tree every
// frame; telemetry then signs it with the EXISTING core and reports the real
// usage graph (states + edges) and crash signatures to the cloud, in the same
// {appId,sentAt,ctx?,events} contract the other SDKs POST (see the telemetry core
// above for the transport-callback contract and event shapes). This is fully
// separate from the fuzz driver: with telemetry on, the fuzzer does NOT pick or
// fire actions; the app runs normally and reproit observes.
//
// Usage (shipped app):
//   #define REPROIT_TELEMETRY 1
//   #define REPROIT_IMGUI_IMPLEMENTATION
//   #include "reproit_imgui.h"
//   ReproIt_TeleOptions o = {};
//   o.appId = "myapp"; o.sampleEnabled = true; o.installCrashHook = true;
//   o.transport = my_curl_post;   // or leave NULL for the spool-file transport
//   reproit::TelemetryInit(&o);
//   ... each frame: ImGui::NewFrame(); reproit::Frame(); ...wrappers...; reproit::FrameEnd();
//   ... at shutdown: reproit::TelemetryShutdown();
inline bool TelemetryInit(const ReproIt_TeleOptions* o) { return ReproIt_Telemetry_Init(o); }
inline void TelemetryError(const char* message)         { ReproIt_Telemetry_Error(message); }
inline void TelemetryShutdown(void)                      { ReproIt_Telemetry_Shutdown(); }
#endif
}  // namespace reproit

#endif  // REPROIT_IMGUI_H

#ifdef REPROIT_IMGUI_IMPLEMENTATION
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <map>
#include <set>
#include <string>
#include <vector>
#ifndef REPROIT_TELEMETRY
// Fuzz-build crash handler needs POSIX signal + write(2). Telemetry builds pull
// these in from their own block; the fuzz build includes them here. The soak
// (--soak) RSS sampler + the per-frame jank watchdog need a monotonic clock and
// the current resident-set size, pulled in for the fuzz build below.
#include <csignal>
#include <unistd.h>
#include <ctime>
#if defined(__APPLE__)
#include <mach/mach.h>
#endif
#endif

namespace reproit {
namespace {

#define REPROIT_IMGUI_MAX 512

#ifndef REPROIT_TELEMETRY
// ---- soak (MEMORY:SAMPLE) + jank (EXPLORE:JANK) shared primitives ----------
//
// Both run ONLY in the fuzz build (this header drives the app; in a telemetry
// build the app is reporting, not driven). They are deterministic by
// construction: the leak verdict is a slope over the whole walk (a true leak
// grows monotonically; a neutral cycle collapses back), and the jank verdict is
// a COARSE, well-separated bucket (a normal immediate-mode frame is < 16ms; the
// jank floor is 200ms, the hang floor 2000ms), so wall-clock jitter cannot flip
// either verdict and the finding id (from,action,bucket) is the same run to run.

// Monotonic milliseconds (CLOCK_MONOTONIC), for per-frame durations + soak time.
static uint64_t reproit_mono_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * 1000ull + (uint64_t)(ts.tv_nsec / 1000000ll);
}

// Current resident-set size in BYTES for THIS process (the app under test, which
// hosts the fuzz driver), or 0 on failure. This is the OS-process analogue of the
// web runner's v8 heap_used: soak.rs reads first-vs-last to compute the per-cycle
// slope. Linux reads `/proc/self/statm` (pages -> bytes); Apple reads the Mach
// task's resident_size. A pure read of the OS, so the same process state yields
// the same number.
static uint64_t reproit_rss_bytes(void) {
#if defined(__APPLE__)
    mach_task_basic_info_data_t info;
    mach_msg_type_number_t count = MACH_TASK_BASIC_INFO_COUNT;
    if (task_info(mach_task_self(), MACH_TASK_BASIC_INFO,
                  (task_info_t)&info, &count) == KERN_SUCCESS) {
        return (uint64_t)info.resident_size;
    }
    return 0;
#elif defined(__linux__)
    FILE* f = std::fopen("/proc/self/statm", "rb");
    if (!f) return 0;
    long pages = 0, resident = 0;
    int got = std::fscanf(f, "%ld %ld", &pages, &resident);
    std::fclose(f);
    if (got < 2 || resident <= 0) return 0;
    long pg = sysconf(_SC_PAGESIZE);
    if (pg <= 0) pg = 4096;
    return (uint64_t)resident * (uint64_t)pg;
#else
    return 0;
#endif
}

// Coarse, well-separated jank floors (ms), identical to the web runner so the two
// surfaces classify the same way. A frame at/over HANG_FLOOR is a freeze; at/over
// JANK_FLOOR is a dropped-frame stall; below it, nothing (a clean frame). The
// emitted marker carries the BUCKET (the floor), never the raw ms, so even the
// detail is reproducible.
#define REPROIT_JANK_FLOOR_MS 200
#define REPROIT_HANG_FLOOR_MS 2000
#endif  // !REPROIT_TELEMETRY

struct State {
    uint32_t rngState = 1;
    int budget = 36;
    int actions = 0;
    int settleFrames = 0;           // let the UI settle a few frames per step
    bool done = false;
    bool loaded = false;
    std::string fireId;             // stable id the wrapper should report this frame
    std::string anchor;             // screen anchor (the active window id)
    std::string prevSig;

    // Canonical structural tree, pool-allocated per frame. Index 0 is the
    // synthetic "screen" root. Strings backing role/id/type/icon/value are kept
    // in parallel storage so the Node pointers stay valid for the frame.
    ReproItSig_Node nodes[REPROIT_IMGUI_MAX];
    std::string store[REPROIT_IMGUI_MAX * 5];   // role,id,type,icon,value per node
    int nNodes = 0;
    std::vector<int> scope;         // stack of open container node indices

    // tappable elements this frame (stable ids) for action selection.
    std::vector<std::string> frameTappables;
    std::set<std::string> seen;

    // Operability/accessibility graph (EXPLORE:GROUNDTRUTH). Every interactive
    // widget collected this frame, in document order, with its gesture kind. This
    // is graph 1 (the ground-truth operable set). Graph 2 (OS accessibility) is
    // empty by construction for an immediate-mode GUI, so each entry defaults to
    // a11y all-false unless the app declares it exposed via reproit::A11y().
    struct Tappable { std::string id; std::string gesture; };
    std::vector<Tappable> frameWidgets;
    // App-declared accessibility for this frame, keyed by stable id: whether the
    // widget exposes a programmatic role / name to AT. Absent => all false.
    struct A11yDecl { bool role; bool name; };
    std::map<std::string, A11yDecl> a11y;

    // Value-class cap (Layer 2): per structural-node signature (V: stripped), the
    // distinct FULL signatures observed. Once a structural node accumulates more
    // than 8 distinct value-class combinations, the runner drops its V: section
    // (structural-only) so an adversarial value generator cannot explode the graph.
    std::map<std::string, std::set<std::string>> valueVariants;

#ifndef REPROIT_TELEMETRY
    // --soak: this run is a soak (the soak tier writes {"replay":[..]}). When set,
    // we sample this process's RSS per step into a MEMORY:SAMPLE series soak.rs
    // reads. Off => no samples (a plain fuzz walk is not a soak).
    bool soak = false;
    uint64_t soakStart = 0;             // monotonic ms at the first sample (t=0 base)

    // Per-frame jank watchdog. lastFrameMs is the monotonic timestamp at the start
    // of the previous frame; the gap to this frame's start is that frame's render
    // duration. While settling AFTER a fired action, we track the MAX frame
    // duration in the window so a transition whose frames blow the jank floor is
    // reported on the edge it belongs to (the action that caused the stall).
    uint64_t lastFrameMs = 0;
    bool haveLastFrame = false;
    uint64_t windowMaxFrameMs = 0;      // max frame duration since the last fire
    int windowJankFrames = 0;           // frames in the window over the jank floor
    std::string jankFrom;               // the from-sig the pending action left
    std::string jankAction;             // the action whose settle window we measure
#endif
};
State g;

// Minimal int extraction from the fuzz json (avoids a json dependency).
long jsonInt(const std::string& text, const char* key, long fallback) {
    std::string needle = std::string("\"") + key + "\"";
    auto pos = text.find(needle);
    if (pos == std::string::npos) return fallback;
    pos = text.find(':', pos);
    if (pos == std::string::npos) return fallback;
    return std::strtol(text.c_str() + pos + 1, nullptr, 10);
}

void loadConfig() {
    g.loaded = true;
    const char* p = std::getenv("REPROIT_FUZZ_CONFIG");
    if (!p) return;
    FILE* f = std::fopen(p, "rb");
    if (!f) return;
    std::string text;
    char buf[4096];
    size_t n;
    while ((n = std::fread(buf, 1, sizeof buf, f)) > 0) text.append(buf, n);
    std::fclose(f);
    long seed = jsonInt(text, "seed", 0);
    g.rngState = seed ? (uint32_t)seed : 1;
    g.budget = (int)jsonInt(text, "budget", 36);
#ifndef REPROIT_TELEMETRY
    // Soak mode is signalled by a "replay" key in the config (the soak tier writes
    // {"replay":[cycle x N]}); in that mode we sample RSS per step. A plain fuzz
    // config has no "replay", so the leak sampler stays off.
    g.soak = text.find("\"replay\"") != std::string::npos;
#endif
}

uint32_t rngNext(uint32_t n) {
    uint32_t s = g.rngState;
    s ^= s << 13; s ^= s >> 17; s ^= s << 5;
    g.rngState = s;
    // Multiply-shift reduction off the HIGH bits: xorshift's low bits are weak.
    return (uint32_t)(((uint64_t)s * n) >> 32);
}

// ImGui labels embed a stable id after "##" (and "###" replaces the whole id).
// The stable id is what enters the signature; the display text before it is
// excluded (rule 1). "Play##play" -> id "play"; "Play###k" -> id "k";
// "Play" (no marker) -> id "Play" (treated as the stable id absent a "##").
std::string stableId(const char* label) {
    if (!label) return std::string();
    std::string s(label);
    auto triple = s.find("###");
    if (triple != std::string::npos) return s.substr(triple + 3);
    auto dbl = s.find("##");
    if (dbl != std::string::npos) return s.substr(dbl + 2);
    return s;  // no marker: the whole label is the developer-chosen id
}

// Allocate a canonical node, append it under the current scope, return its idx.
// `value` (optional) carries the node's displayed value for Layer 2 value-state;
// `valueNode` opt-in marks a non-value-role node as value-bearing (Layer 3).
int alloc(const char* role, const std::string& id, const char* type, const char* icon,
          const char* value = nullptr, bool valueNode = false) {
    if (g.nNodes >= REPROIT_IMGUI_MAX) return g.nNodes - 1;
    int idx = g.nNodes++;
    ReproItSig_Node* n = &g.nodes[idx];
    n->role = nullptr; n->id = nullptr; n->type = nullptr; n->icon = nullptr;
    n->transient = false; n->value = nullptr; n->value_node = valueNode; n->n_children = 0;

    g.store[idx * 5 + 0] = role ? role : "node";
    n->role = g.store[idx * 5 + 0].c_str();
    if (!id.empty())   { g.store[idx * 5 + 1] = id;   n->id   = g.store[idx * 5 + 1].c_str(); }
    if (type && *type) { g.store[idx * 5 + 2] = type; n->type = g.store[idx * 5 + 2].c_str(); }
    if (icon && *icon) { g.store[idx * 5 + 3] = icon; n->icon = g.store[idx * 5 + 3].c_str(); }
    // A value of nullptr means "absent"; an empty string is a real (EMPTY) value.
    if (value)         { g.store[idx * 5 + 4] = value; n->value = g.store[idx * 5 + 4].c_str(); }

    if (!g.scope.empty()) {
        ReproItSig_Node* parent = &g.nodes[g.scope.back()];
        if (parent->n_children < REPROIT_SIG_MAX_CHILDREN) {
            parent->children[parent->n_children++] = n;
        }
    }
    return idx;
}

// Record a leaf widget node and track it as tappable if interactive. `value`
// (optional) is the node's displayed value for Layer 2 value-state; a value-role
// node (textfield) or a value_node-flagged node folds it into the V: section.
// Map a widget role to the GROUNDTRUTH gesture kind (the affordance a pointer
// user exercises). Text-entry widgets are "field"; everything interactive else
// is "button" (a discrete activation). Mirrors the docs' gestureKind vocabulary.
const char* gestureFor(const char* role) {
    if (role && std::strcmp(role, "textfield") == 0) return "field";
    return "button";
}

void emit(const char* role, const char* label, bool interactive,
          const char* type = nullptr, const char* icon = nullptr,
          const char* value = nullptr, bool valueNode = false) {
    std::string id = stableId(label);
    alloc(role, id, type, icon, value, valueNode);
    if (interactive && !id.empty()) {
        g.frameTappables.push_back(id);
        g.frameWidgets.push_back({id, gestureFor(role)});
    }
}

// True for exactly the stable id the fuzzer chose this step.
bool fires(const char* label) {
    return !g.fireId.empty() && g.fireId == stableId(label);
}

// Compute the canonical signature of the current frame's tree. When
// `dropValues` is true, value nodes are temporarily stripped of their `value`
// so the V: section disappears: this yields the STRUCTURAL-ONLY signature, used
// both as the value-cap fallback and as the per-node cap bucket key.
std::string sigImpl(bool dropValues) {
    char out[9];
    const char* anchor = g.anchor.empty() ? nullptr : g.anchor.c_str();
    if (g.nNodes == 0) {
        ReproItSig_Node empty;
        empty.role = "screen"; empty.id = nullptr; empty.type = nullptr;
        empty.icon = nullptr; empty.transient = false; empty.value = nullptr;
        empty.value_node = false; empty.n_children = 0;
        ReproIt_Signature(anchor, &empty, out);
        return out;
    }
    if (!dropValues) {
        ReproIt_Signature(anchor, &g.nodes[0], out);
        return out;
    }
    // Save+clear values, sign, restore: structural-only.
    const char* saved[REPROIT_IMGUI_MAX];
    for (int i = 0; i < g.nNodes; i++) { saved[i] = g.nodes[i].value; g.nodes[i].value = nullptr; }
    ReproIt_Signature(anchor, &g.nodes[0], out);
    for (int i = 0; i < g.nNodes; i++) g.nodes[i].value = saved[i];
    return out;
}

// Layer 1 effect detection + Layer 2 value cap. The emitted state signature is
// the FULL canonical signature (structure + V: value-classes), so a value change
// between frames yields a new/changed EXPLORE:STATE and the host reproit's state
// diff sees the action as effective. The runner enforces the hard cap: at most 8
// distinct value-class combinations per structural node; once exceeded, that
// structural node falls back to structural-only (drop the V: section) so an
// adversarial value generator cannot explode the graph.
std::string structuralSig() {
    std::string structural = sigImpl(/*dropValues=*/true);
    std::string full = sigImpl(/*dropValues=*/false);
    if (structural == full) return full;       // no value nodes: nothing to cap
    auto& variants = g.valueVariants[structural];
    if (variants.size() < 8 || variants.count(full)) {
        variants.insert(full);
        if (variants.size() <= 8) return full;
    }
    // Cap exceeded for this structural node: fall back to structural-only.
    return structural;
}

// Emit one EXPLORE:GROUNDTRUTH line for the current frame's interactive surface
// (single-line JSON). Every widget collected this frame is operable:true; its
// a11y dimensions are all-false BY DEFAULT (immediate-mode GUIs expose nothing to
// AT), upgraded to rolePresent/namePresent true only where the app declared the
// widget exposed via reproit::A11y(). inTabOrder + keyboardActivatable stay false:
// ImGui's interactive widgets are pointer-driven and not part of any reported
// keyboard/tab order, so this is the honest ground truth. Duplicate ids (a widget
// emitted twice in a frame) are reported once, keyed by stable id.
void emitGroundtruth(const std::string& sig) {
    std::printf("EXPLORE:GROUNDTRUTH {\"sig\":\"%s\",\"focusTrap\":false,\"elements\":[",
                sig.c_str());
    std::set<std::string> emitted;
    bool firstEl = true;
    for (const auto& w : g.frameWidgets) {
        if (!emitted.insert(w.id).second) continue;  // dedupe by stable id
        auto it = g.a11y.find(w.id);
        bool rolePresent = it != g.a11y.end() && it->second.role;
        bool namePresent = it != g.a11y.end() && it->second.name;
        std::printf(
            "%s{\"id\":\"%s\",\"operable\":true,\"gestureKind\":\"%s\","
            "\"a11y\":{\"rolePresent\":%s,\"namePresent\":%s,"
            "\"inTabOrder\":false,\"keyboardActivatable\":false}}",
            firstEl ? "" : ",", w.id.c_str(), w.gesture.c_str(),
            rolePresent ? "true" : "false", namePresent ? "true" : "false");
        firstEl = false;
    }
    std::printf("]}\n");
    std::fflush(stdout);
}

// ---- FUZZ-BUILD CRASH HANDLER (async-signal-safe) -------------------------
//
// In the FUZZ build (REPROIT_TELEMETRY undefined) a SIGSEGV/SIGABRT/SIGBUS/
// SIGFPE/SIGILL in the app under test would otherwise just kill the process
// silently: the orchestrator sees the runner die mid-walk with no JOURNEY DONE
// and no crash marker, so the crash is NOT attributed to a node. This handler
// closes that gap: on a fatal signal it writes an `EXCEPTION CAUGHT BY ...`
// block to stdout (the same marker every backend uses, parsed by drive.rs and
// the fuzz oracle), naming the current state signature and the action that led
// there, then re-raises so the OS still produces a core dump and the parent
// still sees the signal. The block is PRE-SERIALIZED on the hot path (FrameEnd)
// so the handler only does async-signal-safe write(2) calls, no malloc / printf
// / buffered stdio. The crash bucket key (kind + message) embeds the state sig +
// action, so the same crash reached the same way buckets to one finding.
//
// This is the FUZZ counterpart to the telemetry crash hook below; the two are
// mutually exclusive by the #ifndef guard, so a TU never installs both.
#ifndef REPROIT_TELEMETRY

// Pre-serialized crash payload (rebuilt each frame from the current sig+action),
// plus its length. Only the handler reads it; FrameEnd writes it. The block is
// split so the handler can splice the (in-handler-chosen) signal name between
// the constant prefix (header + "The following..." line) and the buffered tail
// (the message line carrying state+action, then the closing rule). The fuzz
// oracle reads the message from the line AFTER "The following ...", which is the
// "raised by signal <NAME> at state <sig> after action tap:<act>" line, so the
// crash buckets by signal + node + action.
char reproit_imgui_crash_pre[256];   // header + "╡ IMGUI APP ╞" + "The following ...:\n" + "raised by signal "
size_t reproit_imgui_crash_pre_len = 0;
char reproit_imgui_crash_post[256];  // " at state <sig> after action tap:<act>\n<rule>\n"
size_t reproit_imgui_crash_post_len = 0;
volatile sig_atomic_t reproit_imgui_crash_installed = 0;

// Map a signal number to a stable, async-signal-safe name (a string literal).
const char* reproit_imgui_signame(int sig) {
    switch (sig) {
        case SIGSEGV: return "SIGSEGV";
        case SIGABRT: return "SIGABRT";
        case SIGBUS:  return "SIGBUS";
        case SIGFPE:  return "SIGFPE";
        case SIGILL:  return "SIGILL";
        default:      return "SIGNAL";
    }
}

// Async-signal-safe write of a NUL-terminated literal to fd 1.
void reproit_imgui_safe_write(const char* s) {
    size_t n = 0;
    while (s[n]) n++;
    size_t off = 0;
    while (off < n) {
        long w = (long)write(1, s + off, n - off);
        if (w <= 0) break;
        off += (size_t)w;
    }
}
void reproit_imgui_safe_write_n(const char* s, size_t n) {
    size_t off = 0;
    while (off < n) {
        long w = (long)write(1, s + off, n - off);
        if (w <= 0) break;
        off += (size_t)w;
    }
}

void reproit_imgui_crash_handler(int sig) {
    // Fixed prefix + the signal name + the buffered sig/action tail. All three
    // pieces are constant or pre-serialized off the signal path, so this stays
    // async-signal-safe (only write(2)).
    reproit_imgui_safe_write_n(reproit_imgui_crash_pre, reproit_imgui_crash_pre_len);
    reproit_imgui_safe_write(reproit_imgui_signame(sig));
    reproit_imgui_safe_write_n(reproit_imgui_crash_post, reproit_imgui_crash_post_len);
    // Restore the default handler and re-raise so the OS still cores / the parent
    // still observes the original signal.
    signal(sig, SIG_DFL);
    raise(sig);
}

void reproit_imgui_install_crash_hook() {
    if (reproit_imgui_crash_installed) return;
    reproit_imgui_crash_installed = 1;
    // The constant prefix: the EXCEPTION header (the ╡ KIND ╞ markers the oracle
    // extracts the `kind` from), the "The following ...:" line (the oracle reads
    // the MESSAGE from the line after it), and the start of that message line up
    // to where the signal name is spliced in. "IMGUI APP" is the kind; the
    // message (signal + node + action) buckets distinct crash sites separately.
    const char* pre =
        "EXCEPTION CAUGHT BY IMGUI APP \xe2\x95\xa1 IMGUI APP \xe2\x95\x9e\n"
        "The following crash was raised by the app:\n"
        "raised by signal ";
    size_t n = 0; while (pre[n]) n++;
    if (n >= sizeof reproit_imgui_crash_pre) n = sizeof reproit_imgui_crash_pre - 1;
    std::memcpy(reproit_imgui_crash_pre, pre, n);
    reproit_imgui_crash_pre[n] = 0;
    reproit_imgui_crash_pre_len = n;
    for (int s : {SIGSEGV, SIGABRT, SIGBUS, SIGFPE, SIGILL}) signal(s, reproit_imgui_crash_handler);
}

// Rebuild the post buffer (the message tail + closing rule) from the current
// state. Called on the hot path (FrameEnd), never from the handler.
void reproit_imgui_build_crash_tail() {
    const char* sig = g.prevSig.empty() ? "?" : g.prevSig.c_str();
    const char* act = g.fireId.empty() ? "(launch)" : g.fireId.c_str();
    int w = std::snprintf(
        reproit_imgui_crash_post, sizeof reproit_imgui_crash_post,
        " at state %s after action tap:%s\n"
        "\xe2\x95\x90\xe2\x95\x90\xe2\x95\x90\xe2\x95\x90\xe2\x95\x90\xe2\x95\x90\xe2\x95\x90\xe2\x95\x90\n",
        sig, act);
    reproit_imgui_crash_post_len = (w > 0 && (size_t)w < sizeof reproit_imgui_crash_post)
        ? (size_t)w : 0;
}
#endif  // !REPROIT_TELEMETRY

}  // namespace

void Frame() {
    if (!g.loaded) loadConfig();
#ifndef REPROIT_TELEMETRY
    // Per-frame jank watchdog: the gap between the start of the previous frame and
    // the start of this one is the previous frame's render duration. Accumulate
    // the MAX duration since the last fired action so the settle window's worst
    // frame is attributed to the transition that caused it. Only meaningful while
    // a transition is in flight (we have a from-sig + action); idle frames before
    // the first action just prime lastFrameMs.
    if (!g.done) {
        uint64_t now = reproit_mono_ms();
        if (g.haveLastFrame) {
            uint64_t dur = now - g.lastFrameMs;
            if (!g.jankAction.empty()) {
                if (dur > g.windowMaxFrameMs) g.windowMaxFrameMs = dur;
                if (dur >= REPROIT_JANK_FLOOR_MS) g.windowJankFrames++;
            }
        }
        g.lastFrameMs = now;
        g.haveLastFrame = true;
    }
#endif
    g.nNodes = 0;
    g.scope.clear();
    g.frameTappables.clear();
    g.frameWidgets.clear();
    g.a11y.clear();
    g.anchor.clear();
    // Synthetic screen root (index 0); window contents nest under it.
    int root = alloc("screen", std::string(), nullptr, nullptr);
    g.scope.push_back(root);
}

bool Begin(const char* name) {
    // The window id is the screen anchor; its contents open a group scope.
    g.anchor = stableId(name);
    int idx = alloc("group", stableId(name), nullptr, nullptr);
    g.scope.push_back(idx);
    return ImGui::Begin(name);
}
void End() {
    if (g.scope.size() > 1) g.scope.pop_back();
    ImGui::End();
}

void BeginScope(const char* role, const char* idLabel) {
    int idx = alloc(role, stableId(idLabel), nullptr, nullptr);
    g.scope.push_back(idx);
}
void EndScope() {
    if (g.scope.size() > 1) g.scope.pop_back();
}

bool Button(const char* label) {
    emit("button", label, true);
    return ImGui::Button(label) || fires(label);
}
bool MenuItem(const char* label, const char* shortcut, bool selected, bool enabled) {
    emit("menuitem", label, true);
    return ImGui::MenuItem(label, shortcut, selected, enabled) || fires(label);
}
bool Selectable(const char* label, bool selected) {
    emit("listitem", label, true);
    return ImGui::Selectable(label, selected) || fires(label);
}
bool Checkbox(const char* label, bool* v) {
    emit("checkbox", label, true, "checkbox");
    bool real = ImGui::Checkbox(label, v);
    if (fires(label) && v) { *v = !*v; return true; }
    return real;
}
bool SliderFloat(const char* label, float* v, float vmin, float vmax) {
    // The slider's current value is value-bearing. Role "slider" is NOT in the
    // value-role set, so flag it value_node (Layer 3) to fold the value-class in.
    // A value change between frames thus moves the signature even though the
    // structure is unchanged.
    char vbuf[32];
    std::snprintf(vbuf, sizeof vbuf, "%g", v ? *v : 0.0f);
    emit("slider", label, true, "slider", nullptr, vbuf, /*valueNode=*/true);
    return ImGui::SliderFloat(label, v, vmin, vmax) || fires(label);
}
bool InputText(const char* label, char* buf, size_t bufSize) {
    // The textfield's contents are value-bearing (textfield is a value-role), so
    // typed text folds into the V: section as a bounded value-class.
    emit("textfield", label, true, "text", nullptr, buf ? buf : "");
    return ImGui::InputText(label, buf, bufSize) || fires(label);
}
void Text(const char* label) {
    emit("text", label, false);
    ImGui::TextUnformatted(label);
}
void Header(const char* label) {
    emit("header", label, false);
    ImGui::TextUnformatted(label);
}
void Value(const char* label, const char* value) {
    // Role "output" is a value-role, so the displayed value is value-bearing by
    // role (no value_node flag needed). Excluded from tappables (it is output).
    emit("output", label, false, nullptr, nullptr, value ? value : "");
    if (value) ImGui::TextUnformatted(value);
}

void A11y(const char* idLabel, const char* role, const char* name, bool exposed) {
    std::string id = stableId(idLabel);
    if (id.empty()) return;
    // A widget is role-present / name-present only when the app declares it
    // exposed AND supplies the corresponding string. `exposed` with no role/name
    // is a no-op upgrade (still a gap), keeping the result honest.
    bool rolePresent = exposed && role && *role;
    bool namePresent = exposed && name && *name;
    g.a11y[id] = State::A11yDecl{rolePresent, namePresent};
}

void FrameEnd() {
#ifdef REPROIT_TELEMETRY
    // Production telemetry path: when telemetry is active, observe the real
    // session (sign the current tree with the existing core, report state/edge)
    // and return WITHOUT running the fuzz driver. This keeps the two paths fully
    // separate: a shipped app reports, it is not driven by the seeded walk. The
    // action is the last fuzzer/app tap when present, else "auto".
    if (reproit_tele.active) {
        const char* anchor = g.anchor.empty() ? nullptr : g.anchor.c_str();
        const ReproItSig_Node* root = g.nNodes ? &g.nodes[0] : nullptr;
        const char* action = g.fireId.empty() ? nullptr : g.fireId.c_str();
        ReproIt_Telemetry_Observe(anchor, root, action);
        g.fireId.clear();
        return;
    }
#endif
    if (g.done) return;
#ifndef REPROIT_TELEMETRY
    // Arm the fuzz-build crash handler once, so a SIGSEGV/SIGABRT in the app
    // under test surfaces as an attributed EXCEPTION block instead of a silent
    // process death (see the handler above).
    reproit_imgui_install_crash_hook();
#endif
    // Wait for the UI to settle after a fire; emit nothing mid-transition, so
    // each state/edge is reported exactly once.
    if (g.settleFrames > 0) { g.settleFrames--; return; }

#ifndef REPROIT_TELEMETRY
    // JANK / HANG watchdog (EXPLORE:JANK / EXPLORE:HANG). The settle window for the
    // last fired action has now elapsed; if its WORST frame blew a floor, the
    // action stalled the UI. Classify by the coarse bucket (jitter cannot flip a
    // 200ms/2000ms-separated verdict) and key it by (from, action) like the web
    // runner, so the engine attributes the stall to this exact transition and
    // `check` re-confirms it. Cleared after evaluation so the next action starts a
    // fresh window. The finding id is (from, action, bucket): deterministic.
    if (!g.jankAction.empty()) {
        if (g.windowMaxFrameMs >= REPROIT_HANG_FLOOR_MS) {
            std::printf("EXPLORE:HANG {\"from\":\"%s\",\"action\":\"%s\",\"bucket\":%d,\"count\":%d}\n",
                        g.jankFrom.c_str(), g.jankAction.c_str(),
                        REPROIT_HANG_FLOOR_MS, g.windowJankFrames);
            std::fflush(stdout);
        } else if (g.windowMaxFrameMs >= REPROIT_JANK_FLOOR_MS) {
            std::printf("EXPLORE:JANK {\"from\":\"%s\",\"action\":\"%s\",\"bucket\":%d,\"count\":%d}\n",
                        g.jankFrom.c_str(), g.jankAction.c_str(),
                        REPROIT_JANK_FLOOR_MS, g.windowJankFrames);
            std::fflush(stdout);
        }
        g.jankAction.clear();
        g.jankFrom.clear();
        g.windowMaxFrameMs = 0;
        g.windowJankFrames = 0;
    }
#endif

    // Layer 1 effect detection (immediate-mode): these UIs emit per frame, so an
    // action is effective iff the emitted signature changed between frames. The
    // signature here is the FULL canonical signature (structure + value-classes),
    // so a pure value change (a keypress in a calculator, a counter increment)
    // produces a new/changed EXPLORE:STATE even though the structure is static.
    // The host reproit already diffs emitted states, so a value change surfaces
    // as a new edge/state with no extra protocol.
    std::string sig = structuralSig();
    if (g.seen.insert(sig).second) {
        // labels:[] is required by the engine's STATE parser (map.rs) for the
        // state to register; immediate-mode GUIs exclude localized text from the
        // descriptor by construction, so there are no labels to report.
        std::printf("EXPLORE:STATE {\"sig\":\"%s\",\"labels\":[]}\n", sig.c_str());
        std::fflush(stdout);
        // The operability/accessibility ground truth for this state. Graph 1 is
        // every interactive widget the header tracked this frame (operable:true).
        // Graph 2 (OS accessibility) is EMPTY by construction for immediate-mode
        // GUIs, so each widget's a11y defaults to all-false -> the engine reports
        // the whole interactive surface as a gap (no role/name/tab-order/keyboard
        // activation). reproit::A11y() upgrades rolePresent/namePresent per widget
        // to shrink that gap. focusTrap is false: ImGui has no modal focus capture
        // we can observe. Emitted once per newly seen state (same key as STATE).
        emitGroundtruth(sig);
    }
    if (!g.prevSig.empty() && sig != g.prevSig && !g.fireId.empty()) {
        std::printf("EXPLORE:EDGE {\"from\":\"%s\",\"action\":\"tap:%s\",\"to\":\"%s\"}\n",
                    g.prevSig.c_str(), g.fireId.c_str(), sig.c_str());
        std::fflush(stdout);
    }
    g.prevSig = sig;
    g.fireId.clear();

#ifndef REPROIT_TELEMETRY
    // LEAK sampler (--soak): emit one MEMORY:SAMPLE per step with this process's
    // current RSS, the SAME shape the desktop/web runners emit (heap_used carries
    // RSS bytes), so soak.rs reconstructs the RSS-vs-time series and reads the
    // slope. t_ms is monotonic from the first sample. No-op outside soak.
    if (g.soak) {
        uint64_t now = reproit_mono_ms();
        if (g.soakStart == 0) g.soakStart = now;
        uint64_t rss = reproit_rss_bytes();
        if (rss) {
            std::printf("MEMORY:SAMPLE {\"t_ms\":%llu,\"heap_used\":%llu}\n",
                        (unsigned long long)(now - g.soakStart),
                        (unsigned long long)rss);
            std::fflush(stdout);
        }
    }
#endif

    if (g.actions >= g.budget || g.frameTappables.empty()) {
        std::printf("JOURNEY[a] step: explored %zu states\nJOURNEY DONE\nAll tests passed\n", g.seen.size());
        std::fflush(stdout);
        g.done = true;
        return;
    }
    if (g.actions == 0) { std::printf("JOURNEY claimed role=a\n"); std::fflush(stdout); }

    std::set<std::string> uniqTap(g.frameTappables.begin(), g.frameTappables.end());
    std::vector<std::string> taps(uniqTap.begin(), uniqTap.end());
    g.fireId = taps[rngNext((uint32_t)taps.size())];
    std::printf("FUZZ:ACT tap:%s\n", g.fireId.c_str());
    std::fflush(stdout);
    g.actions++;
    g.settleFrames = 2;
#ifndef REPROIT_TELEMETRY
    // Arm the jank window for the action about to fire: its settle frames are
    // measured (in Frame()) and evaluated when the window elapses next FrameEnd.
    // from = the state we are in (prevSig); action = the edge label being fired.
    g.jankFrom = g.prevSig;
    g.jankAction = std::string("tap:") + g.fireId;
    g.windowMaxFrameMs = 0;
    g.windowJankFrames = 0;
#endif
#ifndef REPROIT_TELEMETRY
    // Refresh the pre-serialized crash payload to the state we are in (prevSig)
    // and the action about to fire (fireId), so if the app's own handler for this
    // action crashes, the handler attributes it to this exact (state, action).
    reproit_imgui_build_crash_tail();
#endif
}

bool Done() { return g.done; }

}  // namespace reproit
#endif  // REPROIT_IMGUI_IMPLEMENTATION
