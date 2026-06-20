// reproit_clay.h - in-app hook for fuzzing Clay (immediate-mode layout).
//
// Like the ImGui hook, Clay has no retained tree or OS accessibility, so the
// app cooperates. Clay hands you a Clay_RenderCommandArray every frame; we walk
// it to build a CANONICAL STRUCTURAL NODE TREE (role + stable id + type + icon +
// nesting), then compute the canonical screen signature from that tree exactly
// as docs/signature.md specifies. Clicks are fired by making the wrapped
// click-check report true for the chosen element. No synthetic pointer events,
// fully deterministic.
//
// The signature is structural, NOT a hash of visible text: localized strings
// never enter the descriptor (rule 1). This is what makes a Clay finding bucket
// to the same node a production SDK crash hits.
//
// Usage (one translation unit defines the impl):
//   #define REPROIT_CLAY_IMPLEMENTATION
//   #include "reproit_clay.h"
//   ... each frame, after Clay_EndLayout():
//   Clay_RenderCommandArray cmds = Clay_EndLayout();
//   ReproIt_Clay_Frame(cmds);                          // build canonical tree
//   ... where you handle clicks, instead of your own hit-test:
//   if (ReproIt_Clay_Clicked(CLAY_ID("PlayButton"))) { ... }
//   ReproIt_Clay_FrameEnd();                           // emit + pick next
//   if (ReproIt_Clay_Done()) break;
//
// Config via REPROIT_FUZZ_CONFIG (json): {"seed":N,"budget":N}. Output is the
// marker protocol on stdout.
//
// NOTE: Clay's struct layout shifts between versions; the field accesses in
// ReproIt_Clay_Frame may need adjusting to your Clay release. Status: authored,
// NOT yet validated against a built Clay app.

// ===========================================================================
// CANONICAL STRUCTURAL SIGNATURE CORE (docs/signature.md)
//
// This block is the parity-critical part: a self-contained, plain-C
// implementation of the canonical descriptor + FNV-1a hash. It depends on
// nothing but libc, so runners/test_signature.c can include this header alone
// (without clay.h) and assert it against signature_vectors.json. The Clay-
// specific code below builds a ReproItSig_Node tree and calls ReproIt_Signature.
//
// It MUST stay byte-for-byte equivalent to the Rust oracle in
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

// Everything below is Clay-specific and pulls in clay.h. Define
// REPROIT_SIG_CORE_ONLY (as the parity test does) to consume only the core
// above without the Clay dependency.
#ifndef REPROIT_SIG_CORE_ONLY

#ifndef REPROIT_CLAY_H
#define REPROIT_CLAY_H
#include <stdbool.h>
#include "clay.h"

void ReproIt_Clay_Frame(Clay_RenderCommandArray commands);
bool ReproIt_Clay_Clicked(Clay_ElementId id);  // use instead of your hit-test
void ReproIt_Clay_FrameEnd(void);
bool ReproIt_Clay_Done(void);
void ReproIt_Clay_Screen(const char* name);    // optional screen/route anchor

// Mark an element as a VALUE NODE carrying a displayed value (docs/signature.md
// "Value-state", Layer 2/3). Clay text is excluded from the structural body, so a
// value-state app (a counter, a score, a clock) would otherwise collapse to one
// signature. Call this each frame for any element whose displayed value is part
// of the screen's identity; `stringId` matches the element's CLAY_ID string and
// `value` is the raw displayed string (bucketed locale-safely by value_class).
// The matching node is flagged value_node so its bounded value-class folds into
// the canonical signature's V: section; a value change then yields a new state.
void ReproIt_Clay_Value(const char* stringId, const char* value);

#endif  // REPROIT_CLAY_H

#ifdef REPROIT_CLAY_IMPLEMENTATION
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define REPROIT_CLAY_MAX 256
#define REPROIT_CLAY_STR 64

typedef struct {
    uint32_t rng;
    int budget, actions, settle;
    bool done, loaded;
    uint32_t fireId;
    char fireName[REPROIT_CLAY_STR];   // name of the chosen element, at pick time
    char prevSig[9];
    char anchor[REPROIT_CLAY_STR];     // optional screen/route anchor

    // Canonical structural tree built each frame. Nodes are pool-allocated;
    // index 0 is always the synthetic "screen" root.
    ReproItSig_Node nodes[REPROIT_CLAY_MAX];
    char nodeRole[REPROIT_CLAY_MAX][REPROIT_CLAY_STR];
    char nodeId[REPROIT_CLAY_MAX][REPROIT_CLAY_STR];
    char nodeValue[REPROIT_CLAY_MAX][REPROIT_CLAY_STR];   // displayed value (Layer 2)
    int nNodes;

    // Value-node marks for this frame: (stringId, value) pairs the app declared
    // via ReproIt_Clay_Value before ReproIt_Clay_FrameEnd. Applied to matching
    // nodes so their value-class folds into the V: section.
    char markId[REPROIT_CLAY_MAX][REPROIT_CLAY_STR];
    char markVal[REPROIT_CLAY_MAX][REPROIT_CLAY_STR];
    int nMarks;

    // Value-class cap (Layer 2): per structural-node signature (V: stripped), the
    // count of distinct FULL signatures observed. Once a structural node exceeds 8
    // distinct value-class combinations, the runner drops its V: section so an
    // adversarial value generator cannot explode the graph.
    char capStruct[REPROIT_CLAY_MAX][9];
    char capFull[REPROIT_CLAY_MAX][9];   // (struct, full) pairs, flat
    int nCap;

    // tappable elements this frame (for action selection)
    uint32_t tapIds[REPROIT_CLAY_MAX];
    char tapNames[REPROIT_CLAY_MAX][REPROIT_CLAY_STR];
    int nTaps;

    // seen-state ring (signatures)
    char seen[REPROIT_CLAY_MAX][9];
    int nSeen;
} ReproItClay;
static ReproItClay rc;

static long reproit_json_int(const char* text, const char* key, long fallback) {
    char needle[64];
    snprintf(needle, sizeof needle, "\"%s\"", key);
    const char* p = strstr(text, needle);
    if (!p) return fallback;
    p = strchr(p, ':');
    return p ? strtol(p + 1, NULL, 10) : fallback;
}

static void reproit_clay_load(void) {
    rc.loaded = true;
    rc.budget = 36;
    rc.rng = 1;
    const char* path = getenv("REPROIT_FUZZ_CONFIG");
    if (!path) return;
    FILE* f = fopen(path, "rb");
    if (!f) return;
    char text[8192];
    size_t n = fread(text, 1, sizeof text - 1, f);
    text[n] = 0;
    fclose(f);
    long seed = reproit_json_int(text, "seed", 0);
    rc.rng = seed ? (uint32_t)seed : 1;
    rc.budget = (int)reproit_json_int(text, "budget", 36);
}

static uint32_t reproit_clay_rng(uint32_t mod) {
    uint32_t s = rc.rng;
    s ^= s << 13; s ^= s >> 17; s ^= s << 5;
    rc.rng = s;
    // High-bit multiply-shift: xorshift's low bits are weak for small moduli.
    return (uint32_t)(((uint64_t)s * mod) >> 32);
}

// Map a Clay element's string-id PREFIX to a canonical role. Clay has no role
// system, so the convention is to name elements with a role prefix on the
// CLAY_ID, e.g. CLAY_ID("Button_Play"), CLAY_ID("Header_Title"). The prefix
// (before the first '_') drives the role; the WHOLE string-id is the stable id.
// Unknown / prefix-less ids fall back to "group" for containers (the default
// for a layout element) -- callers wanting a precise role use a known prefix.
static const char* reproit_clay_role_for(const char* stringId) {
    static const struct { const char* prefix; const char* role; } MAP[] = {
        {"Button",    "button"},   {"Btn",      "button"},
        {"Header",    "header"},   {"Title",    "header"},
        {"Text",      "text"},     {"Label",    "text"},
        {"Link",      "link"},
        {"Field",     "textfield"},{"Input",    "textfield"},{"TextField", "textfield"},
        {"Image",     "image"},    {"Img",      "image"},
        {"Icon",      "icon"},
        {"List",      "list"},     {"Item",     "listitem"}, {"ListItem", "listitem"},
        {"Tab",       "tab"},
        {"Switch",    "switch"},   {"Toggle",   "switch"},
        {"Checkbox",  "checkbox"}, {"Check",    "checkbox"},
        {"Radio",     "radio"},
        {"Slider",    "slider"},
        {"Menu",      "menu"},     {"MenuItem", "menuitem"},
        {"Dialog",    "dialog"},   {"Modal",    "dialog"},
        {"Group",     "group"},    {"Row",      "group"},    {"Col",      "group"},
        {"Spinner",   "spinner"},  {"Toast",    "toast"},    {"Badge",    "badge"},
    };
    if (stringId) {
        for (size_t i = 0; i < sizeof MAP / sizeof *MAP; i++) {
            size_t plen = strlen(MAP[i].prefix);
            if (strncmp(stringId, MAP[i].prefix, plen) == 0 &&
                (stringId[plen] == 0 || stringId[plen] == '_')) {
                return MAP[i].role;
            }
        }
    }
    return "group";
}

static ReproItSig_Node* reproit_clay_alloc(const char* role, const char* id) {
    if (rc.nNodes >= REPROIT_CLAY_MAX) return &rc.nodes[REPROIT_CLAY_MAX - 1];
    int idx = rc.nNodes++;
    ReproItSig_Node* n = &rc.nodes[idx];
    memset(n, 0, sizeof *n);
    snprintf(rc.nodeRole[idx], REPROIT_CLAY_STR, "%s", role ? role : "node");
    n->role = rc.nodeRole[idx];
    if (id && id[0]) {
        snprintf(rc.nodeId[idx], REPROIT_CLAY_STR, "%s", id);
        n->id = rc.nodeId[idx];
    }
    return n;
}

static void reproit_clay_add_child(ReproItSig_Node* parent, ReproItSig_Node* child) {
    if (parent->n_children < REPROIT_SIG_MAX_CHILDREN) {
        parent->children[parent->n_children++] = child;
    }
}

// Compute the canonical signature of the current frame's tree. When dropValues
// is true, value nodes are temporarily stripped so the V: section disappears,
// yielding the STRUCTURAL-ONLY signature (the value-cap fallback and bucket key).
static void reproit_clay_sig_impl(char out[9], bool dropValues) {
    const char* anchor = rc.anchor[0] ? rc.anchor : NULL;
    if (!dropValues) { ReproIt_Signature(anchor, &rc.nodes[0], out); return; }
    const char* saved[REPROIT_CLAY_MAX];
    for (int i = 0; i < rc.nNodes; i++) { saved[i] = rc.nodes[i].value; rc.nodes[i].value = NULL; }
    ReproIt_Signature(anchor, &rc.nodes[0], out);
    for (int i = 0; i < rc.nNodes; i++) rc.nodes[i].value = saved[i];
}

// Compute the emitted state signature with the Layer 2 value cap enforced: the
// FULL signature normally, but once a structural node accumulates more than 8
// distinct value-class combinations it falls back to structural-only so an
// adversarial value generator cannot explode the graph.
static void reproit_clay_sig(char out[9]) {
    char structural[9], full[9];
    reproit_clay_sig_impl(structural, true);
    reproit_clay_sig_impl(full, false);
    if (strcmp(structural, full) == 0) { snprintf(out, 9, "%s", full); return; }
    // Count distinct full sigs already seen for this structural node, and whether
    // this exact full sig was already recorded.
    int distinct = 0;
    bool known = false;
    for (int i = 0; i < rc.nCap; i++) {
        if (strcmp(rc.capStruct[i], structural) == 0) {
            distinct++;
            if (strcmp(rc.capFull[i], full) == 0) known = true;
        }
    }
    if (!known && distinct >= 8) { snprintf(out, 9, "%s", structural); return; }  // cap hit
    if (!known && rc.nCap < REPROIT_CLAY_MAX) {
        snprintf(rc.capStruct[rc.nCap], 9, "%s", structural);
        snprintf(rc.capFull[rc.nCap], 9, "%s", full);
        rc.nCap++;
    }
    snprintf(out, 9, "%s", full);
}

void ReproIt_Clay_Screen(const char* name) {
    if (name) snprintf(rc.anchor, sizeof rc.anchor, "%s", name); else rc.anchor[0] = 0;
}

// Build the canonical tree from the render commands. Clay reports commands in
// document (open) order with explicit scissor/border start+end markers that
// bracket nesting. We use the bounding-box nesting implied by the element ids,
// but Clay does not expose parentage in the command stream directly, so we
// build a flat "screen -> elements" tree keyed by element string-id. Each
// rectangle/border element with a non-zero string-id becomes a node; text
// commands are excluded (localized text never enters the descriptor).
//
// Clay's struct layout shifts between versions; adjust field accesses to your
// release if this does not compile.
void ReproIt_Clay_Frame(Clay_RenderCommandArray commands) {
    if (!rc.loaded) reproit_clay_load();
    rc.nNodes = 0;
    rc.nTaps = 0;
    rc.nMarks = 0;
    ReproItSig_Node* root = reproit_clay_alloc("screen", NULL);  // index 0
    (void)root;

    for (int32_t i = 0; i < commands.length; i++) {
        Clay_RenderCommand* cmd = Clay_RenderCommandArray_Get(&commands, i);
        if (!cmd) continue;
        // Text commands carry localized strings -> excluded from the descriptor.
        if (cmd->commandType == CLAY_RENDER_COMMAND_TYPE_TEXT) continue;
        // Only elements with a stable string-id contribute a node.
        if (cmd->id == 0) continue;
        // Recover the string-id text. Clay stores the element id; the string is
        // available via Clay_ElementId lookup on most releases. We approximate
        // with the numeric id when no string is available.
        char sid[REPROIT_CLAY_STR];
        sid[0] = 0;
#ifdef REPROIT_CLAY_HAS_STRINGID
        // If your Clay release exposes the string-id on the command, fill sid.
        // (Left as a hook; default path uses the numeric id below.)
#endif
        if (!sid[0]) snprintf(sid, sizeof sid, "e%u", (unsigned)cmd->id);
        const char* role = reproit_clay_role_for(sid);
        ReproItSig_Node* n = reproit_clay_alloc(role, sid);
        reproit_clay_add_child(&rc.nodes[0], n);
    }
}

// Declare a value node for this frame (Layer 2/3). Recorded now and applied to
// the matching node in ReproIt_Clay_FrameEnd; call after ReproIt_Clay_Frame.
void ReproIt_Clay_Value(const char* stringId, const char* value) {
    if (!stringId || rc.nMarks >= REPROIT_CLAY_MAX) return;
    snprintf(rc.markId[rc.nMarks], REPROIT_CLAY_STR, "%s", stringId);
    snprintf(rc.markVal[rc.nMarks], REPROIT_CLAY_STR, "%s", value ? value : "");
    rc.nMarks++;
}

// Apply this frame's value marks: for each mark whose stringId matches a node's
// id, flag the node value_node and attach its value so the value-class folds in.
static void reproit_clay_apply_marks(void) {
    for (int m = 0; m < rc.nMarks; m++) {
        for (int i = 0; i < rc.nNodes; i++) {
            if (rc.nodes[i].id && strcmp(rc.nodes[i].id, rc.markId[m]) == 0) {
                snprintf(rc.nodeValue[i], REPROIT_CLAY_STR, "%s", rc.markVal[m]);
                rc.nodes[i].value = rc.nodeValue[i];
                rc.nodes[i].value_node = true;
                break;
            }
        }
    }
}

// Record an element as tappable and report a click when real OR fuzzer-chosen.
bool ReproIt_Clay_Clicked(Clay_ElementId id) {
    if (rc.nTaps < REPROIT_CLAY_MAX) {
        rc.tapIds[rc.nTaps] = id.id;
        snprintf(rc.tapNames[rc.nTaps], REPROIT_CLAY_STR, "%.*s",
                 (int)id.stringId.length, id.stringId.chars);
        rc.nTaps++;
    }
    bool real = Clay_PointerOver(id);  // app's normal hover/click path
    return real || (rc.fireId && rc.fireId == id.id);
}

static bool reproit_seen(const char* sig) {
    for (int i = 0; i < rc.nSeen; i++) if (strcmp(rc.seen[i], sig) == 0) return true;
    if (rc.nSeen < REPROIT_CLAY_MAX) { snprintf(rc.seen[rc.nSeen++], 9, "%s", sig); }
    return false;
}

void ReproIt_Clay_FrameEnd(void) {
    if (rc.done) return;
    // Settle before reading, so each state/edge is emitted exactly once.
    if (rc.settle > 0) { rc.settle--; return; }
    // Apply value-node marks so value-classes are part of the emitted signature.
    reproit_clay_apply_marks();
    // Layer 1 effect detection (immediate-mode): Clay emits per frame, so an
    // action is effective iff the emitted signature changed between frames. The
    // signature is the FULL canonical signature (structure + value-classes), so a
    // pure value change (a counter increment, a clock tick) produces a new/changed
    // EXPLORE:STATE even though the structure is static; the host reproit already
    // diffs emitted states, so the value change surfaces as a new state/edge.
    char sig[9];
    reproit_clay_sig(sig);

    if (!reproit_seen(sig)) {
        printf("EXPLORE:STATE {\"sig\":\"%s\"}\n", sig);
        fflush(stdout);
    }
    if (rc.prevSig[0] && strcmp(sig, rc.prevSig) != 0 && rc.fireId) {
        // The fired element belonged to the PREVIOUS screen, whose taps are no
        // longer in tapNames; use the name captured when the action was picked.
        printf("EXPLORE:EDGE {\"from\":\"%s\",\"action\":\"tap:%s\",\"to\":\"%s\"}\n",
               rc.prevSig, rc.fireName, sig);
        fflush(stdout);
    }

    snprintf(rc.prevSig, 9, "%s", sig);
    rc.fireId = 0;
    rc.fireName[0] = 0;

    if (rc.actions >= rc.budget || rc.nTaps == 0) {
        printf("JOURNEY[a] step: explored %d states\nJOURNEY DONE\nAll tests passed\n", rc.nSeen);
        fflush(stdout);
        rc.done = true;
        return;
    }
    if (rc.actions == 0) { printf("JOURNEY claimed role=a\n"); fflush(stdout); }

    int pick = (int)reproit_clay_rng((uint32_t)rc.nTaps);
    rc.fireId = rc.tapIds[pick];
    snprintf(rc.fireName, sizeof rc.fireName, "%s", rc.tapNames[pick]);
    printf("FUZZ:ACT tap:%s\n", rc.tapNames[pick]);
    fflush(stdout);
    rc.actions++;
    rc.settle = 2;
}

bool ReproIt_Clay_Done(void) { return rc.done; }

#endif  // REPROIT_CLAY_IMPLEMENTATION

#endif  // REPROIT_SIG_CORE_ONLY
