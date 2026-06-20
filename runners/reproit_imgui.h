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
// Status: authored, NOT yet validated against a built ImGui app. The signature
// core is parity-tested against signature_vectors.json (runners/test_signature.c).

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
}  // namespace reproit

#endif  // REPROIT_IMGUI_H

#ifdef REPROIT_IMGUI_IMPLEMENTATION
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <map>
#include <set>
#include <string>
#include <vector>

namespace reproit {
namespace {

#define REPROIT_IMGUI_MAX 512

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

    // Value-class cap (Layer 2): per structural-node signature (V: stripped), the
    // distinct FULL signatures observed. Once a structural node accumulates more
    // than 8 distinct value-class combinations, the runner drops its V: section
    // (structural-only) so an adversarial value generator cannot explode the graph.
    std::map<std::string, std::set<std::string>> valueVariants;
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
void emit(const char* role, const char* label, bool interactive,
          const char* type = nullptr, const char* icon = nullptr,
          const char* value = nullptr, bool valueNode = false) {
    std::string id = stableId(label);
    alloc(role, id, type, icon, value, valueNode);
    if (interactive && !id.empty()) g.frameTappables.push_back(id);
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

}  // namespace

void Frame() {
    if (!g.loaded) loadConfig();
    g.nNodes = 0;
    g.scope.clear();
    g.frameTappables.clear();
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

void FrameEnd() {
    if (g.done) return;
    // Wait for the UI to settle after a fire; emit nothing mid-transition, so
    // each state/edge is reported exactly once.
    if (g.settleFrames > 0) { g.settleFrames--; return; }

    // Layer 1 effect detection (immediate-mode): these UIs emit per frame, so an
    // action is effective iff the emitted signature changed between frames. The
    // signature here is the FULL canonical signature (structure + value-classes),
    // so a pure value change (a keypress in a calculator, a counter increment)
    // produces a new/changed EXPLORE:STATE even though the structure is static.
    // The host reproit already diffs emitted states, so a value change surfaces
    // as a new edge/state with no extra protocol.
    std::string sig = structuralSig();
    if (g.seen.insert(sig).second) {
        std::printf("EXPLORE:STATE {\"sig\":\"%s\"}\n", sig.c_str());
        std::fflush(stdout);
    }
    if (!g.prevSig.empty() && sig != g.prevSig && !g.fireId.empty()) {
        std::printf("EXPLORE:EDGE {\"from\":\"%s\",\"action\":\"tap:%s\",\"to\":\"%s\"}\n",
                    g.prevSig.c_str(), g.fireId.c_str(), sig.c_str());
        std::fflush(stdout);
    }
    g.prevSig = sig;
    g.fireId.clear();

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
}

bool Done() { return g.done; }

}  // namespace reproit
#endif  // REPROIT_IMGUI_IMPLEMENTATION
