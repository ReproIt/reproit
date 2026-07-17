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
// For network-dependent apps, route the JSON HTTP client through
// ReproIt_Causal_Json and call ReproIt_Causal_Enable once; it is inert outside a
// Reproit run and fail-closed when a capsule is present.
//
// PRODUCTION TELEMETRY (optional, OFF by default): define REPROIT_TELEMETRY and
// the SAME ReproIt_Clay_* calls double as a production SDK. Every frame the tree
// they build is signed with the existing canonical core and reported as a live
// usage graph (states + edges) plus crash signatures, in the same
// {appId,sentAt,ctx?,events} contract the other SDKs POST. You supply a transport
// callback (wire it to libcurl/your HTTP), or the built-in transport spools
// newline-delimited JSON to a file/FD. A crash hook flushes the last signature +
// edge path on SIGSEGV/SIGABRT (async-signal-safe). With telemetry on the fuzz
// driver does not run: the app reports its real sessions, it is not driven. See
// the "PRODUCTION TELEMETRY CORE" block below and ReproIt_Telemetry_Init().
//
// The telemetry layer never modifies the signature core; it only CALLS
// ReproIt_Signature, so parity (runners/test_signature.c) is preserved.
//
// NOTE: Clay's struct layout shifts between versions; the field accesses in
// ReproIt_Clay_Frame may need adjusting to your Clay release.

#ifdef REPROIT_CLAY_IMPLEMENTATION
#define REPROIT_CAUSAL_IMPLEMENTATION
#endif
#include "reproit_causal.h"

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
#ifndef REPROIT_INPUT_PURPOSE
#define REPROIT_INPUT_PURPOSE(purpose, id) "reproit-purpose-" purpose "--" id
#endif

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
  const char *role;  // required
  const char *id;    // optional, NULL = none
  const char *type;  // optional input refinement
  const char *icon;  // optional icon identity
  bool transient;    // explicit transient marker
  const char *value; // optional displayed value (Layer 2); NULL = none
  bool value_node;   // opt-in value-bearing flag (Layer 3)
  int n_children;
  struct ReproItSig_Node *children[REPROIT_SIG_MAX_CHILDREN];
} ReproItSig_Node;

// The fixed, language-independent role vocabulary. Anything outside it
// normalizes to "node".
static const char *const REPROIT_SIG_ROLES[] = {
    "screen", "header", "text",     "button", "link",   "textfield", "image",
    "icon",   "list",   "listitem", "tab",    "switch", "checkbox",  "radio",
    "slider", "menu",   "menuitem", "dialog", "group",  "node",
};
// Roles that flicker in and out and are dropped before hashing (rule 2).
// "progress" is the role name for spinner/progress.
static const char *const REPROIT_SIG_TRANSIENT[] = {
    "toast", "snackbar", "spinner", "progress", "tooltip", "badge",
};
// Value-role set (docs/signature.md "Value-state"). A node is value-bearing only
// if it has a `value` AND its RAW role is one of these (or it is value_node-
// flagged, Layer 3). Several of these (status, log, progressbar, meter, timer,
// output) are NOT in REPROIT_SIG_ROLES, so they normalize to "node" in the body;
// the value-role test therefore uses the RAW role, not the normalized one. Chrome
// roles (button/label/header/text/link) are NEVER value-bearing.
static const char *const REPROIT_SIG_VALUE_ROLES[] = {
    "textfield", "status", "log", "progressbar", "meter", "timer", "output",
};

static bool reproit_sig_str_empty(const char *s) { return !s || !s[0]; }

static const char *reproit_sig_normalize_role(const char *role) {
  if (!role)
    return "node";
  for (size_t i = 0; i < sizeof REPROIT_SIG_ROLES / sizeof *REPROIT_SIG_ROLES; i++) {
    if (strcmp(role, REPROIT_SIG_ROLES[i]) == 0)
      return REPROIT_SIG_ROLES[i];
  }
  return "node";
}

static bool reproit_sig_is_transient(const ReproItSig_Node *n) {
  if (n->transient)
    return true;
  if (!n->role)
    return false;
  for (size_t i = 0; i < sizeof REPROIT_SIG_TRANSIENT / sizeof *REPROIT_SIG_TRANSIENT; i++) {
    if (strcmp(n->role, REPROIT_SIG_TRANSIENT[i]) == 0)
      return true;
  }
  return false;
}

// A bounded append-only string buffer with truncation guard.
typedef struct {
  char buf[REPROIT_SIG_MAX_DESC];
  size_t len;
} ReproItSig_Buf;

static void reproit_sig_buf_init(ReproItSig_Buf *b) {
  b->len = 0;
  b->buf[0] = 0;
}
static void reproit_sig_putc(ReproItSig_Buf *b, char c) {
  if (b->len + 1 < sizeof b->buf) {
    b->buf[b->len++] = c;
    b->buf[b->len] = 0;
  }
}
static void reproit_sig_puts(ReproItSig_Buf *b, const char *s) {
  if (!s)
    return;
  for (; *s; s++)
    reproit_sig_putc(b, *s);
}
static void reproit_sig_put_uint(ReproItSig_Buf *b, unsigned v) {
  char tmp[16];
  int i = 0;
  if (v == 0) {
    reproit_sig_putc(b, '0');
    return;
  }
  while (v && i < (int)sizeof tmp) {
    tmp[i++] = (char)('0' + v % 10);
    v /= 10;
  }
  while (i--)
    reproit_sig_putc(b, tmp[i]);
}

// Emit one node's token body (everything after "<depth>:"), without the repeat
// marker: <role>[:<type>][#<icon>][@<id>]. Field order is fixed.
static void reproit_sig_token_body(const ReproItSig_Node *n, ReproItSig_Buf *b) {
  reproit_sig_puts(b, reproit_sig_normalize_role(n->role));
  if (!reproit_sig_str_empty(n->type)) {
    reproit_sig_putc(b, ':');
    reproit_sig_puts(b, n->type);
  }
  if (!reproit_sig_str_empty(n->icon)) {
    reproit_sig_putc(b, '#');
    reproit_sig_puts(b, n->icon);
  }
  if (!reproit_sig_str_empty(n->id)) {
    reproit_sig_putc(b, '@');
    reproit_sig_puts(b, n->id);
  }
}

// Emit one token "<depth>:<body>[*]". `first` is true for the very first token
// emitted into `b` (no leading ';'); otherwise a ';' separates from the prior
// token. Returns false so callers can keep threading the "first" flag.
static bool reproit_sig_emit_token(const ReproItSig_Node *n, unsigned depth, bool repeated,
                                   bool first, ReproItSig_Buf *b) {
  if (!first)
    reproit_sig_putc(b, ';');
  reproit_sig_put_uint(b, depth);
  reproit_sig_putc(b, ':');
  reproit_sig_token_body(n, b);
  if (repeated)
    reproit_sig_putc(b, '*');
  return false;
}

// Build the canonical subtree descriptor for collapse comparison (rule 3): the
// pre-order token list of this subtree with depths re-based to start at 0, so
// two sibling subtrees at the same level compare equal regardless of absolute
// depth. Transients are dropped. Mirrors walk_key in the oracle.
static void reproit_sig_subtree_key(const ReproItSig_Node *n, unsigned depth, bool *first,
                                    ReproItSig_Buf *b) {
  *first = reproit_sig_emit_token(n, depth, false, *first, b);
  for (int i = 0; i < n->n_children; i++) {
    if (reproit_sig_is_transient(n->children[i]))
      continue;
    reproit_sig_subtree_key(n->children[i], depth + 1, first, b);
  }
}

static void reproit_sig_subtree_key_str(const ReproItSig_Node *n, char *out) {
  ReproItSig_Buf b;
  reproit_sig_buf_init(&b);
  bool first = true;
  reproit_sig_subtree_key(n, 0, &first, &b);
  memcpy(out, b.buf, b.len + 1);
}

// Forward decl: the serializer recurses through children-with-collapse.
static bool reproit_sig_serialize_node(const ReproItSig_Node *n, unsigned depth, bool repeated,
                                       bool first, ReproItSig_Buf *b);

// Walk a run of retained siblings, collapsing maximal runs of >= 2 consecutive
// children whose subtree_key is identical into one emission with the `*` marker
// (count dropped). Threads the "first token" flag through.
static bool reproit_sig_serialize_children(struct ReproItSig_Node *const *children, int n,
                                           unsigned depth, bool first, ReproItSig_Buf *b) {
  // Filter out transient children up front so collapse runs see only retained
  // siblings (a transient between two identical nodes must not break the run).
  const ReproItSig_Node *kept[REPROIT_SIG_MAX_CHILDREN];
  int nk = 0;
  for (int i = 0; i < n && nk < REPROIT_SIG_MAX_CHILDREN; i++) {
    if (!reproit_sig_is_transient(children[i]))
      kept[nk++] = children[i];
  }
  int i = 0;
  while (i < nk) {
    char key[REPROIT_SIG_MAX_DESC];
    reproit_sig_subtree_key_str(kept[i], key);
    int j = i + 1;
    while (j < nk) {
      char k2[REPROIT_SIG_MAX_DESC];
      reproit_sig_subtree_key_str(kept[j], k2);
      if (strcmp(k2, key) != 0)
        break;
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
static bool reproit_sig_serialize_node(const ReproItSig_Node *n, unsigned depth, bool repeated,
                                       bool first, ReproItSig_Buf *b) {
  first = reproit_sig_emit_token(n, depth, repeated, first, b);
  first = reproit_sig_serialize_children(n->children, n->n_children, depth + 1, first, b);
  return first;
}

// --- Layer 2: value-state (docs/signature.md "Value-state") ----------------

// Strict ^[+-]?[0-9]+(\.[0-9]+)?$: optional sign, one or more ASCII digits,
// optionally a period followed by one or more ASCII digits. No grouping, no
// exponent, no leading/trailing dot. Locale-safe by construction.
static bool reproit_sig_is_strict_decimal(const char *s) {
  size_t i = 0, len = strlen(s);
  if (i < len && (s[i] == '+' || s[i] == '-'))
    i++;
  size_t int_start = i;
  while (i < len && s[i] >= '0' && s[i] <= '9')
    i++;
  if (i == int_start)
    return false; // need at least one integer digit
  if (i < len && s[i] == '.') {
    i++;
    size_t frac_start = i;
    while (i < len && s[i] >= '0' && s[i] <= '9')
      i++;
    if (i == frac_start)
      return false; // trailing dot with no fraction
  }
  return i == len;
}

// Map a value string to a bounded, deterministic, locale-safe value-class token.
static const char *reproit_sig_value_class(const char *s) {
  if (!s)
    return "EMPTY";
  // Trim leading/trailing ASCII whitespace into a local copy of the core span.
  const char *a = s;
  while (*a == ' ' || *a == '\t' || *a == '\n' || *a == '\r' || *a == '\f' || *a == '\v')
    a++;
  const char *e = a + strlen(a);
  while (e > a) {
    char c = e[-1];
    if (c == ' ' || c == '\t' || c == '\n' || c == '\r' || c == '\f' || c == '\v')
      e--;
    else
      break;
  }
  size_t n = (size_t)(e - a);
  if (n == 0)
    return "EMPTY";
  char tmp[64];
  if (n >= sizeof tmp)
    return "NONEMPTY"; // too long to be our short numeric grammar
  memcpy(tmp, a, n);
  tmp[n] = 0;
  if (!reproit_sig_is_strict_decimal(tmp))
    return "NONEMPTY";
  // Parse is safe: the grammar is a subset of strtod's accepted syntax.
  double v = strtod(tmp, NULL);
  double abs = v < 0 ? -v : v;
  if (v == 0.0)
    return "ZERO";
  if (v < 0.0)
    return "NEG";
  if (abs < 10.0)
    return "POS1";
  if (abs < 100.0)
    return "POS2";
  if (abs < 1000.0)
    return "POS3";
  return "POSL";
}

// True if this node carries a canonical value-class in the V: section: it has a
// non-NULL `value` AND it is value-bearing (RAW role in the value-role set OR
// value_node-flagged). The raw role is used deliberately (status/meter normalize
// to "node" but are still value-roles).
static bool reproit_sig_is_value_bearing(const ReproItSig_Node *n) {
  if (!n->value)
    return false;
  if (n->value_node)
    return true;
  if (!n->role)
    return false;
  for (size_t i = 0; i < sizeof REPROIT_SIG_VALUE_ROLES / sizeof *REPROIT_SIG_VALUE_ROLES; i++) {
    if (strcmp(n->role, REPROIT_SIG_VALUE_ROLES[i]) == 0)
      return true;
  }
  return false;
}

// Emit the V:-section key for a value-bearing node: "key:<id>" if it has an id,
// otherwise the structural fallback "role:<normalized-role>#<idx>".
static void reproit_sig_value_key(const ReproItSig_Node *n, unsigned idx, ReproItSig_Buf *b) {
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
  const char *cls;
} ReproItSig_VEntry;

// Collect (value_key, value_class) for every value-bearing node in pre-order,
// skipping transient subtrees (rule 2) so the V: section is consistent with the
// body. A keyless node's structural index is its position among same-(normalized-)
// role non-transient siblings under the same parent (the root gets index 0).
static void reproit_sig_collect_values(const ReproItSig_Node *n, unsigned idx,
                                       ReproItSig_VEntry *out, int *count, int cap) {
  if (reproit_sig_is_transient(n))
    return;
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
  const char *roles[REPROIT_SIG_MAX_CHILDREN];
  unsigned counts[REPROIT_SIG_MAX_CHILDREN];
  int nr = 0;
  for (int i = 0; i < n->n_children; i++) {
    const ReproItSig_Node *c = n->children[i];
    if (reproit_sig_is_transient(c))
      continue;
    const char *role = reproit_sig_normalize_role(c->role);
    unsigned cidx = 0;
    int found = -1;
    for (int r = 0; r < nr; r++)
      if (strcmp(roles[r], role) == 0) {
        found = r;
        break;
      }
    if (found >= 0) {
      cidx = counts[found];
      counts[found]++;
    } else if (nr < REPROIT_SIG_MAX_CHILDREN) {
      roles[nr] = role;
      counts[nr] = 1;
      nr++;
    }
    reproit_sig_collect_values(c, cidx, out, count, cap);
  }
}

// Build the V: section suffix. Returns nothing appended when there are NO value-
// bearing nodes, keeping the descriptor purely structural. Otherwise appends
// "\nV:" + key=class;... sorted by key.
static void reproit_sig_value_section(const ReproItSig_Node *root, ReproItSig_Buf *out) {
  static ReproItSig_VEntry entries[REPROIT_SIG_MAX_CHILDREN * 4];
  int cap = (int)(sizeof entries / sizeof *entries);
  int count = 0;
  reproit_sig_collect_values(root, 0, entries, &count, cap);
  if (count == 0)
    return;
  // Insertion sort by key (stable, small n).
  for (int i = 1; i < count; i++) {
    ReproItSig_VEntry tmp = entries[i];
    int j = i - 1;
    while (j >= 0 && strcmp(entries[j].key, tmp.key) > 0) {
      entries[j + 1] = entries[j];
      j--;
    }
    entries[j + 1] = tmp;
  }
  reproit_sig_puts(out, "\nV:");
  for (int i = 0; i < count; i++) {
    if (i)
      reproit_sig_putc(out, ';');
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
static void reproit_sig_descriptor(const char *anchor, const ReproItSig_Node *root,
                                   ReproItSig_Buf *out) {
  reproit_sig_buf_init(out);
  reproit_sig_puts(out, "A:");
  if (anchor)
    reproit_sig_puts(out, anchor);
  reproit_sig_putc(out, '\n');
  if (root && !reproit_sig_is_transient(root)) {
    bool first = true;
    reproit_sig_serialize_node(root, 0, false, first, out);
    reproit_sig_value_section(root, out);
  }
}

// FNV-1a 32-bit over the descriptor's UTF-8 bytes -> 8-char lowercase hex.
static void reproit_sig_fnv1a32_hex(const char *bytes, size_t len, char out[9]) {
  uint32_t h = 0x811c9dc5u;
  for (size_t i = 0; i < len; i++) {
    h ^= (unsigned char)bytes[i];
    h *= 0x01000193u;
  }
  snprintf(out, 9, "%08x", h);
}

// Public: compute the canonical 8-char hex signature for (anchor, tree).
static void ReproIt_Signature(const char *anchor, const ReproItSig_Node *root, char out[9]) {
  ReproItSig_Buf desc;
  reproit_sig_descriptor(anchor, root, &desc);
  reproit_sig_fnv1a32_hex(desc.buf, desc.len, out);
}

#endif // REPROIT_SIGNATURE_CORE_H

// ===========================================================================
// CONTENT-BUG CLASSIFIER CORE (always available; the scan itself is fuzz-only)
//
// IDENTICAL to the content-bug classifier block in runners/reproit_imgui.h
// (shared guard, so if both headers land in one TU the classifier appears once).
// Self-contained plain C: it depends on nothing but libc and never touches the
// parity-critical signature core, so the parity test (which defines
// REPROIT_SIG_CORE_ONLY) is unaffected.
//
// WHAT IT IS: the immediate-mode counterpart of the web runner's content-bug
// oracle (runners/web/runner.mjs detectContentBugs/reasonOf). Instrumented
// runners SEE the actual display strings the app draws (Clay text elements,
// ImGui labels), so we can scan that VISIBLE text for the SAME literal artifacts
// a stringify/template bug leaks to the screen, with the SAME classifier
// semantics + precedence, byte-for-byte:
//   - "[object Object]"          -> "object-object"      (object coerced to string)
//   - "{{ ... }}" or "${ ... }"  -> "unrendered-template" (binding never evaluated)
//   - whole-word "undefined"     -> "undefined"
//   - whole-word "null"          -> "null"
//   - whole-word "NaN"           -> "nan"
// The order is fixed and the FIRST match wins, so a label carries one reason.
// The match is on STRUCTURE (a literal artifact token), never natural language:
// `undefined`/`null`/`NaN` must stand alone as a whole word (the same boundary
// sets the web regex uses), so ordinary prose ("Cancellation", "Null Island")
// is never flagged. A clean app renders none of these, so the control stays
// silent (no marker, no finding). The finding is addressed by the element's
// STABLE id (locale-invariant), never by the text, so it is replayable.
// ===========================================================================
#ifndef REPROIT_CONTENTBUG_CORE_H
#define REPROIT_CONTENTBUG_CORE_H
#include <stdbool.h>
#include <stddef.h>
#include <string.h>

// Left/right whole-word boundary sets, IDENTICAL to the web runner's regex
// character classes around `undefined`/`null`/`NaN`:
//   left  (^|[\s:>(\[,])   right ($|[\s.,!?)\]<])
// A NUL on either side is start/end of string (the `^`/`$` anchors). The
// whitespace members mirror JS \s for the ASCII range (space/tab/newline/CR/
// form-feed/vertical-tab); non-ASCII bytes are never boundary chars, matching
// the web behaviour where a multibyte run keeps the token glued to its prose.
static bool reproit_cbug_is_left_boundary(unsigned char c) {
  if (c == 0)
    return true; // start of string
  switch (c) {
  case ' ':
  case '\t':
  case '\n':
  case '\r':
  case '\f':
  case '\v':
  case ':':
  case '>':
  case '(':
  case '[':
  case ',':
    return true;
  default:
    return false;
  }
}
static bool reproit_cbug_is_right_boundary(unsigned char c) {
  if (c == 0)
    return true; // end of string
  switch (c) {
  case ' ':
  case '\t':
  case '\n':
  case '\r':
  case '\f':
  case '\v':
  case '.':
  case ',':
  case '!':
  case '?':
  case ')':
  case ']':
  case '<':
    return true;
  default:
    return false;
  }
}

// True if `word` occurs in `text` as a whole word per the boundary sets above.
// Scans every occurrence (a later boundaried hit still counts), so the same
// guarding the web `\b`-anchored regex applies.
static bool reproit_cbug_has_word(const char *text, const char *word) {
  size_t wlen = strlen(word);
  if (wlen == 0)
    return false;
  for (const char *p = text; (p = strstr(p, word)) != NULL; p++) {
    unsigned char before = (p == text) ? 0 : (unsigned char)p[-1];
    unsigned char after = (unsigned char)p[wlen];
    if (reproit_cbug_is_left_boundary(before) && reproit_cbug_is_right_boundary(after))
      return true;
  }
  return false;
}

// True if `text` contains an unrendered template placeholder: a "{{ ... }}" or
// "${ ... }" where the "..." has no closing brace inside it. Mirrors the web
// regexes /\{\{[^}]*\}\}/ and /\$\{[^}]*\}/.
static bool reproit_cbug_has_template(const char *text) {
  // {{ [^}]* }}
  for (const char *p = strstr(text, "{{"); p; p = strstr(p + 1, "{{")) {
    const char *q = p + 2;
    while (*q && *q != '}')
      q++;
    if (q[0] == '}' && q[1] == '}')
      return true;
  }
  // ${ [^}]* }
  for (const char *p = strstr(text, "${"); p; p = strstr(p + 1, "${")) {
    const char *q = p + 2;
    while (*q && *q != '}')
      q++;
    if (*q == '}')
      return true;
  }
  return false;
}

// Classify a display string into a stable content-bug reason tag, or NULL when
// clean. Order is FIXED and the first match wins, IDENTICAL to the web
// reasonOf precedence: object-object, then unrendered-template, then whole-word
// undefined, null, nan. Returned tags are string literals (stable storage).
static inline const char *reproit_cbug_reason(const char *text) {
  if (!text || !text[0])
    return NULL;
  if (strstr(text, "[object Object]"))
    return "object-object";
  if (reproit_cbug_has_template(text))
    return "unrendered-template";
  if (reproit_cbug_has_word(text, "undefined"))
    return "undefined";
  if (reproit_cbug_has_word(text, "null"))
    return "null";
  if (reproit_cbug_has_word(text, "NaN"))
    return "nan";
  return NULL;
}

#endif // REPROIT_CONTENTBUG_CORE_H

// ===========================================================================
// PRODUCTION TELEMETRY CORE (optional; OFF by default)
//
// IDENTICAL to the telemetry core block in runners/reproit_imgui.h (shared guard,
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
#define REPROIT_TELE_MAX_EVENTS 64 // events buffered before an auto-flush
#endif
#ifndef REPROIT_TELE_EVENT_CAP
#define REPROIT_TELE_EVENT_CAP 512 // bytes per serialized event line
#endif
#ifndef REPROIT_TELE_PATH_CAP
#define REPROIT_TELE_PATH_CAP 60 // graph-trail entries kept for a repro
#endif
#ifndef REPROIT_TELE_CRASH_CAP
#define REPROIT_TELE_CRASH_CAP 4096 // pre-serialized crash payload buffer
#endif

// The transport callback contract. The telemetry layer hands you ONE fully
// serialized JSON batch (already in the {appId,sentAt,ctx?,events} envelope, a
// NUL-terminated UTF-8 string of `len` bytes) plus the `user` pointer you set at
// init. You ship it however you like (libcurl POST, a queue, a file). Return is
// ignored. The callback is invoked from reproit telemetry flush points (frame
// flush / explicit flush), NEVER from the crash signal handler (the crash path
// uses only the async-signal-safe spool write, see below), so your transport may
// allocate/use stdio freely. It must NOT call back into the reproit telemetry API.
typedef void (*ReproIt_TransportFn)(const char *json, size_t len, void *user);

// Telemetry init options. Zero-initialize then set fields; appId/endpoint are
// borrowed (kept by pointer, so use string literals or stable storage).
typedef struct {
  const char *appId;             // required; defaults to "app" if NULL
  ReproIt_TransportFn transport; // optional; NULL => built-in spool transport
  void *transportUser;           // opaque, passed to transport
  const char *ctxJson; // optional raw JSON object/string for "ctx" (no validation); NULL => omit
  const char
      *spoolPath; // built-in transport target; NULL => env REPROIT_TELEMETRY_SPOOL, else stderr fd
  bool installCrashHook; // install SIGSEGV/SIGABRT/... handlers (default true via init helper)
  bool sampleEnabled;    // host decides sampling; false => telemetry no-ops
} ReproIt_TeleOptions;

// One graph-trail entry kept for crash repros: (signature, action that led here).
typedef struct {
  char sig[9];
  char action[64];
} ReproIt_TelePathEntry;

typedef struct {
  bool active; // init() called and sampling on
  char appId[128];
  ReproIt_TransportFn transport;
  void *transportUser;
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
  bool spoolOwned; // we opened it, so close on flush-exit

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
static bool reproit_tele_append(char *dst, size_t cap, size_t *len, const char *src) {
  if (!src)
    return true;
  size_t i = 0;
  while (src[i] && *len + 1 < cap) {
    dst[(*len)++] = src[i++];
  }
  dst[*len < cap ? *len : cap - 1] = 0;
  return src[i] == 0;
}

// Append a JSON string literal value WITH surrounding quotes, escaping the JSON
// control set (" \\ and the C0 controls). Deterministic, no locale, no alloc.
static void reproit_tele_append_jstr(char *dst, size_t cap, size_t *len, const char *s) {
  if (*len + 1 < cap)
    dst[(*len)++] = '"';
  for (const char *p = s ? s : ""; *p && *len + 2 < cap; p++) {
    unsigned char c = (unsigned char)*p;
    if (c == '"' || c == '\\') {
      dst[(*len)++] = '\\';
      dst[(*len)++] = (char)c;
    } else if (c == '\n') {
      dst[(*len)++] = '\\';
      dst[(*len)++] = 'n';
    } else if (c == '\r') {
      dst[(*len)++] = '\\';
      dst[(*len)++] = 'r';
    } else if (c == '\t') {
      dst[(*len)++] = '\\';
      dst[(*len)++] = 't';
    } else if (c < 0x20) { /* drop other control bytes */
    } else
      dst[(*len)++] = (char)c;
  }
  if (*len + 1 < cap)
    dst[(*len)++] = '"';
  dst[*len < cap ? *len : cap - 1] = 0;
}

// Append an unsigned 64-bit integer in decimal. No alloc.
static void reproit_tele_append_u64(char *dst, size_t cap, size_t *len, uint64_t v) {
  char tmp[24];
  int i = 0;
  if (v == 0)
    tmp[i++] = '0';
  while (v && i < (int)sizeof tmp) {
    tmp[i++] = (char)('0' + (int)(v % 10));
    v /= 10;
  }
  while (i-- && *len + 1 < cap)
    dst[(*len)++] = tmp[i];
  dst[*len < cap ? *len : cap - 1] = 0;
}

// Wall-clock milliseconds for the "t"/"sentAt" fields. time() is async-signal-
// safe per POSIX; we only use second precision *1000 to stay portable.
static uint64_t reproit_tele_now_ms(void) { return (uint64_t)time(NULL) * 1000ull; }

// Rebuild the PRE-SERIALIZED crash payload from the current state + path, so the
// signal handler can emit it with a single write(2). Called on the hot path only
// (never from the handler). The crash event is a complete batch envelope so the
// spooled line is a self-contained record even though the process is dying.
static void reproit_tele_build_crash(const char *signame) {
  char *b = reproit_tele.crashBuf;
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
  if (reproit_tele.hasCur)
    reproit_tele_append_jstr(b, cap, &n, reproit_tele.curSig);
  else
    reproit_tele_append(b, cap, &n, "null");
  reproit_tele_append(b, cap, &n, ",\"message\":");
  reproit_tele_append_jstr(b, cap, &n, signame ? signame : "crash");
  reproit_tele_append(b, cap, &n, ",\"path\":[");
  for (int i = 0; i < reproit_tele.nPath; i++) {
    if (i)
      reproit_tele_append(b, cap, &n, ",");
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
  if (reproit_tele.nEvents == 0)
    return;
  // Build the {appId,sentAt,ctx?,events:[...]} envelope into a heap batch buffer.
  size_t cap = (size_t)reproit_tele.nEvents * REPROIT_TELE_EVENT_CAP + 1024;
  char *batch = (char *)malloc(cap);
  if (!batch) {
    reproit_tele.nEvents = 0;
    return;
  }
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
    if (i)
      reproit_tele_append(batch, cap, &n, ",");
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
      if (w <= 0)
        break;
      off += (size_t)w;
    }
    if (write(reproit_tele.spoolFd, "\n", 1) < 0) { /* best-effort */
    }
  }
  free(batch);
  reproit_tele.nEvents = 0;
}

// Push one already-serialized event line into the buffer, auto-flushing when full.
static void reproit_tele_push(const char *line) {
  if (reproit_tele.nEvents >= REPROIT_TELE_MAX_EVENTS)
    reproit_tele_flush();
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
      if (w <= 0)
        break;
      off += (size_t)w;
    }
  }
  signal(sig, SIG_DFL);
  raise(sig);
}

static void reproit_tele_install_crash_hook(void) {
  if (reproit_tele.crashHookInstalled)
    return;
  reproit_tele.crashHookInstalled = true;
  signal(SIGSEGV, reproit_tele_crash_handler);
  signal(SIGABRT, reproit_tele_crash_handler);
  signal(SIGFPE, reproit_tele_crash_handler);
  signal(SIGILL, reproit_tele_crash_handler);
#ifdef SIGBUS
  signal(SIGBUS, reproit_tele_crash_handler);
#endif
}

// Initialize telemetry. After this, the per-frame hooks below feed states/edges.
// Returns true if telemetry is active (sampling on), false if it no-ops.
static bool ReproIt_Telemetry_Init(const ReproIt_TeleOptions *opt) {
  memset(&reproit_tele, 0, sizeof reproit_tele);
  reproit_tele.spoolFd = -1;
  if (!opt || !opt->sampleEnabled)
    return false;
  reproit_tele.active = true;
  snprintf(reproit_tele.appId, sizeof reproit_tele.appId, "%s", opt->appId ? opt->appId : "app");
  reproit_tele.transport = opt->transport;
  reproit_tele.transportUser = opt->transportUser;
  if (opt->ctxJson && opt->ctxJson[0]) {
    snprintf(reproit_tele.ctxJson, sizeof reproit_tele.ctxJson, "%s", opt->ctxJson);
    reproit_tele.hasCtx = true;
  }
  // Built-in spool transport: only opened when no custom transport is supplied.
  if (!reproit_tele.transport) {
    const char *path = opt->spoolPath;
    if (!path || !path[0])
      path = getenv("REPROIT_TELEMETRY_SPOOL");
    if (path && path[0]) {
      FILE *f = fopen(path, "ab");
      if (f) {
        reproit_tele.spoolFd = fileno(f);
        reproit_tele.spoolOwned = true;
      }
    }
    if (reproit_tele.spoolFd < 0)
      reproit_tele.spoolFd = 2; // fall back to stderr
  }
  if (opt->installCrashHook)
    reproit_tele_install_crash_hook();
  return true;
}

// Record the current frame's signature. Computes the canonical signature with the
// EXISTING core (ReproIt_Signature) over the caller-built tree, emits a state
// event the first time a signature is seen-as-current, and an edge event when the
// signature changes (carrying the action that caused the transition). The crash
// payload is rebuilt here so the signal handler always has the latest path. Pass
// the action that led to this frame (e.g. "tap:play"), or NULL for "auto".
static void ReproIt_Telemetry_Observe(const char *anchor, const ReproItSig_Node *root,
                                      const char *action) {
  if (!reproit_tele.active)
    return;
  char sig[9];
  ReproIt_Signature(anchor, root, sig);
  if (reproit_tele.hasCur && strcmp(sig, reproit_tele.curSig) == 0)
    return; // no change

  char from[9];
  bool hadFrom = reproit_tele.hasCur;
  if (hadFrom)
    memcpy(from, reproit_tele.curSig, sizeof from);
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
static void ReproIt_Telemetry_Error(const char *message) {
  if (!reproit_tele.active)
    return;
  char line[REPROIT_TELE_EVENT_CAP];
  size_t n = 0;
  reproit_tele_append(line, sizeof line, &n, "{\"kind\":\"error\",\"sig\":");
  if (reproit_tele.hasCur)
    reproit_tele_append_jstr(line, sizeof line, &n, reproit_tele.curSig);
  else
    reproit_tele_append(line, sizeof line, &n, "null");
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
  if (!reproit_tele.active)
    return;
  reproit_tele_flush();
  if (reproit_tele.spoolOwned && reproit_tele.spoolFd >= 0)
    close(reproit_tele.spoolFd);
  reproit_tele.active = false;
}

#endif // REPROIT_TELEMETRY && !REPROIT_TELEMETRY_CORE_H

// Everything below is Clay-specific and pulls in clay.h. Define
// REPROIT_SIG_CORE_ONLY (as the parity test does) to consume only the core
// above without the Clay dependency.
#ifndef REPROIT_SIG_CORE_ONLY

// ===========================================================================
// SCREENSHOT-CAPTURE CORE (always available; capture itself is opt-in)
//
// IDENTICAL to the capture core block in runners/reproit_imgui.h (shared guard,
// so if both headers land in one TU the capture core appears once). Self-
// contained plain C: it depends on nothing but libc, and like the signature
// core it never touches the parity-critical descriptor/hash logic, so the
// parity test (runners/test_signature.c, which defines REPROIT_SIG_CORE_ONLY)
// never even compiles this block.
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
typedef bool (*ReproIt_CaptureFn)(const char *png_path, void *user);

// Single registered capture hook (static per TU; the shared guard means only one
// TU compiles this block when both headers are present).
typedef struct {
  ReproIt_CaptureFn fn;
  void *user;
} ReproIt_CaptureState;
static ReproIt_CaptureState reproit_capture = {0, 0};

static void ReproIt_SetCaptureFn(ReproIt_CaptureFn fn, void *user) {
  reproit_capture.fn = fn;
  reproit_capture.user = user;
}

// ---- minimal self-contained PNG (zlib "stored" / uncompressed) -------------
// No external dependency: deflate is emitted as raw stored blocks, which any
// conformant zlib/PNG decoder reads. CRC32 + Adler32 are computed inline.

static uint32_t reproit_png_crc_table[256];
static bool reproit_png_crc_ready = false;
static void reproit_png_crc_init(void) {
  if (reproit_png_crc_ready)
    return;
  for (uint32_t n = 0; n < 256; n++) {
    uint32_t c = n;
    for (int k = 0; k < 8; k++)
      c = (c & 1) ? 0xedb88320u ^ (c >> 1) : (c >> 1);
    reproit_png_crc_table[n] = c;
  }
  reproit_png_crc_ready = true;
}
static uint32_t reproit_png_crc(uint32_t crc, const unsigned char *buf, size_t len) {
  reproit_png_crc_init();
  crc ^= 0xffffffffu;
  for (size_t i = 0; i < len; i++)
    crc = reproit_png_crc_table[(crc ^ buf[i]) & 0xff] ^ (crc >> 8);
  return crc ^ 0xffffffffu;
}

static void reproit_png_be32(unsigned char *p, uint32_t v) {
  p[0] = (unsigned char)(v >> 24);
  p[1] = (unsigned char)(v >> 16);
  p[2] = (unsigned char)(v >> 8);
  p[3] = (unsigned char)v;
}

// Write one PNG chunk (length + type + data + CRC) to `f`.
static bool reproit_png_chunk(FILE *f, const char *type, const unsigned char *data, size_t len) {
  unsigned char hdr[8];
  reproit_png_be32(hdr, (uint32_t)len);
  memcpy(hdr + 4, type, 4);
  if (fwrite(hdr, 1, 8, f) != 8)
    return false;
  if (len && fwrite(data, 1, len, f) != len)
    return false;
  uint32_t crc = reproit_png_crc(0, (const unsigned char *)type, 4);
  if (len)
    crc = reproit_png_crc(crc, data, len);
  unsigned char crcb[4];
  reproit_png_be32(crcb, crc);
  return fwrite(crcb, 1, 4, f) == 4;
}

// Write an 8-bit RGBA PNG. `pixels` is `w*h*4` bytes, top row first (the caller
// orders rows; the GL default below flips bottom-up reads). Returns true on
// success. Uses an uncompressed zlib stream so no compressor is needed.
static bool ReproIt_WritePNG_RGBA(const char *path, const unsigned char *pixels, unsigned w,
                                  unsigned h) {
  if (!path || !pixels || w == 0 || h == 0)
    return false;
  FILE *f = fopen(path, "wb");
  if (!f)
    return false;

  static const unsigned char SIG[8] = {137, 80, 78, 71, 13, 10, 26, 10};
  bool ok = fwrite(SIG, 1, 8, f) == 8;

  // IHDR: width, height, bit depth 8, color type 6 (RGBA), no compression/
  // filter/interlace method bytes (all 0).
  unsigned char ihdr[13];
  reproit_png_be32(ihdr, w);
  reproit_png_be32(ihdr + 4, h);
  ihdr[8] = 8;
  ihdr[9] = 6;
  ihdr[10] = 0;
  ihdr[11] = 0;
  ihdr[12] = 0;
  ok = ok && reproit_png_chunk(f, "IHDR", ihdr, sizeof ihdr);

  // Build the raw (pre-zlib) image data: each scanline prefixed with filter
  // byte 0 (none). Then wrap in a zlib stream of stored deflate blocks.
  size_t row = (size_t)w * 4;
  size_t raw_len = (row + 1) * h; // +1 filter byte per row
  unsigned char *raw = (unsigned char *)malloc(raw_len);
  if (!raw) {
    fclose(f);
    return false;
  }
  for (unsigned y = 0; y < h; y++) {
    raw[(row + 1) * y] = 0; // filter: none
    memcpy(raw + (row + 1) * y + 1, pixels + row * y, row);
  }

  // zlib stream: 2-byte header (0x78 0x01) + stored deflate blocks + Adler32.
  // Each stored block carries up to 65535 bytes: 1 BFINAL/BTYPE byte, LEN,
  // ~LEN (little-endian), then the literal bytes.
  size_t max_blocks = raw_len / 65535 + 1;
  size_t zcap = 2 + raw_len + max_blocks * 5 + 4;
  unsigned char *z = (unsigned char *)malloc(zcap);
  if (!z) {
    free(raw);
    fclose(f);
    return false;
  }
  size_t zn = 0;
  z[zn++] = 0x78;
  z[zn++] = 0x01; // CMF, FLG
  size_t off = 0;
  while (off < raw_len) {
    size_t block = raw_len - off;
    if (block > 65535)
      block = 65535;
    int final = (off + block >= raw_len) ? 1 : 0;
    z[zn++] = (unsigned char) final; // BFINAL bit, BTYPE=00 (stored)
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
  if (!ok)
    remove(path);
  return ok;
}

#ifdef REPROIT_CAPTURE_GL
// Default OpenGL capture: glReadPixels of the current viewport, written as PNG.
// The app must include its GL headers BEFORE this header (or define the GL
// prototypes); we only call glGetIntegerv/glReadPixels/GL_RGBA/GL_UNSIGNED_BYTE,
// which are core in every desktop/ES profile. GL reads bottom-up, so we flip
// rows into top-down order for the PNG.
static bool reproit_capture_gl(const char *png_path, void *user) {
  (void)user;
  GLint vp[4] = {0, 0, 0, 0};
  glGetIntegerv(GL_VIEWPORT, vp);
  unsigned w = (unsigned)(vp[2] > 0 ? vp[2] : 0);
  unsigned h = (unsigned)(vp[3] > 0 ? vp[3] : 0);
  if (w == 0 || h == 0)
    return false;
  size_t row = (size_t)w * 4;
  unsigned char *buf = (unsigned char *)malloc(row * h);
  if (!buf)
    return false;
  glPixelStorei(GL_PACK_ALIGNMENT, 1);
  glReadPixels(vp[0], vp[1], (GLsizei)w, (GLsizei)h, GL_RGBA, GL_UNSIGNED_BYTE, buf);
  // Flip vertically: GL row 0 is the bottom; PNG row 0 is the top.
  unsigned char *flip = (unsigned char *)malloc(row * h);
  if (!flip) {
    free(buf);
    return false;
  }
  for (unsigned y = 0; y < h; y++)
    memcpy(flip + row * y, buf + row * (h - 1 - y), row);
  free(buf);
  bool ok = ReproIt_WritePNG_RGBA(png_path, flip, w, h);
  free(flip);
  return ok;
}
#endif // REPROIT_CAPTURE_GL

// Sanitize a shoot name to the orchestrator's charset [A-Za-z0-9_/-], in place,
// into `out` (capacity `cap`). Anything else is dropped, so the emitted marker
// name and the written file path agree with drive.rs's filter.
static void reproit_shoot_sanitize(const char *name, char *out, size_t cap) {
  size_t j = 0;
  for (const char *p = name ? name : ""; *p && j + 1 < cap; p++) {
    char c = *p;
    if ((c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z') || (c >= '0' && c <= '9') || c == '_' ||
        c == '/' || c == '-') {
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
static bool ReproIt_Shoot(const char *name) {
  const char *dir = getenv("REPROIT_SHOTS_DIR");
  if (!dir || !dir[0])
    return false;
  char safe[256];
  reproit_shoot_sanitize(name, safe, sizeof safe);
  if (!safe[0])
    return false;

  char path[1024];
  int np = snprintf(path, sizeof path, "%s/%s.png", dir, safe);
  if (np <= 0 || (size_t)np >= sizeof path)
    return false;

  bool ok = false;
  if (reproit_capture.fn) {
    ok = reproit_capture.fn(path, reproit_capture.user);
  }
#ifdef REPROIT_CAPTURE_GL
  else {
    ok = reproit_capture_gl(path, NULL);
  }
#endif
  if (!ok)
    return false;
  printf("SHOOT:%s\n", safe);
  fflush(stdout);
  return true;
}

#endif // REPROIT_CAPTURE_CORE_H

// ===========================================================================
// MULTI-ACTOR SCENARIO CORE (shared, self-contained: reproit_imgui.h carries
// an identical block under the same guard)
//
// The conductor client for authored multi-user scenarios (modes/barrier.rs):
//   GET  /claim               -> role letter (`a`, `b`, ...) | `ERR full`
//   GET  /next?device=<role>  -> `WAIT` | `ACT\t<action>` | `DONE`
//   POST /done?device=<role>  -> `OK`
// The hook must stay dependency-free, so this is a minimal BLOCKING HTTP/1.1
// exchange over plain sockets (POSIX, or Winsock behind _WIN32); the conductor
// always answers a numeric localhost address with Connection: close, so
// read-to-end after the blank line is exactly the body. Used only when
// REPROIT_SCENARIO_BARRIER names a conductor; a normal fuzz/telemetry build
// never opens a socket.
// ===========================================================================
#ifndef REPROIT_SCENARIO_CORE_H
#define REPROIT_SCENARIO_CORE_H

#ifdef _WIN32
#include <winsock2.h>
#include <ws2tcpip.h>
#ifdef _MSC_VER
#pragma comment(lib, "ws2_32.lib") /* MinGW: link -lws2_32 */
#endif
typedef SOCKET reproit_scn_sock_t;
#define REPROIT_SCN_BAD_SOCK INVALID_SOCKET
#else
#include <arpa/inet.h>
#include <netinet/in.h>
#include <sys/socket.h>
#include <time.h>
#include <unistd.h>
typedef int reproit_scn_sock_t;
#define REPROIT_SCN_BAD_SOCK (-1)
#endif

// Sleep the poll interval INSIDE the frame, so a waiting actor's render loop
// cannot spin a frame-count guard away while its peer holds the turn.
static void reproit_scn_sleep_ms(int ms) {
#ifdef _WIN32
  Sleep((DWORD)ms);
#else
  struct timespec ts;
  ts.tv_sec = ms / 1000;
  ts.tv_nsec = (long)(ms % 1000) * 1000000L;
  nanosleep(&ts, NULL);
#endif
}

// One blocking HTTP exchange with the conductor: send the request, read the
// whole response, copy the trimmed body into `out`. Returns false on any
// socket/parse failure (the caller just retries on the next frame).
static bool reproit_scn_http(const char *base, const char *method, const char *path, char *out,
                             size_t cap) {
  if (out && cap)
    out[0] = 0;
  if (!base || !base[0])
    return false;
  const char *p = strstr(base, "://");
  p = p ? p + 3 : base;
  char host[64];
  size_t hl = 0;
  while (p[hl] && p[hl] != ':' && p[hl] != '/' && hl < sizeof host - 1) {
    host[hl] = p[hl];
    hl++;
  }
  host[hl] = 0;
  int port = (p[hl] == ':') ? atoi(p + hl + 1) : 80;
#ifdef _WIN32
  static bool reproit_scn_wsa_ready = false;
  if (!reproit_scn_wsa_ready) {
    WSADATA w;
    if (WSAStartup(MAKEWORD(2, 2), &w) != 0)
      return false;
    reproit_scn_wsa_ready = true;
  }
#endif
  reproit_scn_sock_t s = socket(AF_INET, SOCK_STREAM, 0);
  if (s == REPROIT_SCN_BAD_SOCK)
    return false;
  struct sockaddr_in addr;
  memset(&addr, 0, sizeof addr);
  addr.sin_family = AF_INET;
  addr.sin_port = htons((unsigned short)port);
  // The conductor URL is always a numeric localhost address (the
  // orchestrator binds 127.0.0.1), so no resolver is needed.
  addr.sin_addr.s_addr = inet_addr(host);
  bool ok = false;
  if (addr.sin_addr.s_addr != INADDR_NONE &&
      connect(s, (struct sockaddr *)&addr, sizeof addr) == 0) {
    char req[512];
    int n = snprintf(req, sizeof req, "%s %s HTTP/1.1\r\nHost: %s\r\nConnection: close\r\n\r\n",
                     method, path, host);
    if (n > 0 && (size_t)n < sizeof req) {
      size_t off = 0;
      while (off < (size_t)n) {
        long w = (long)send(s, req + off, (int)((size_t)n - off), 0);
        if (w <= 0)
          break;
        off += (size_t)w;
      }
      if (off == (size_t)n) {
        char resp[2048];
        size_t got = 0;
        for (;;) {
          long r = (long)recv(s, resp + got, (int)(sizeof resp - 1 - got), 0);
          if (r <= 0)
            break;
          got += (size_t)r;
          if (got >= sizeof resp - 1)
            break;
        }
        resp[got] = 0;
        const char *body = strstr(resp, "\r\n\r\n");
        if (body) {
          body += 4;
          size_t len = strlen(body);
          while (len && (body[0] == ' ' || body[0] == '\r' || body[0] == '\n' || body[0] == '\t')) {
            body++;
            len--;
          }
          while (len && (body[len - 1] == ' ' || body[len - 1] == '\r' || body[len - 1] == '\n' ||
                         body[len - 1] == '\t')) {
            len--;
          }
          if (out && cap) {
            if (len >= cap)
              len = cap - 1;
            memcpy(out, body, len);
            out[len] = 0;
          }
          ok = true;
        }
      }
    }
  }
#ifdef _WIN32
  closesocket(s);
#else
  close(s);
#endif
  return ok;
}

#endif // REPROIT_SCENARIO_CORE_H

#ifndef REPROIT_CLAY_H
#define REPROIT_CLAY_H
#include "clay.h"
#include <stdbool.h>

void ReproIt_Clay_Frame(Clay_RenderCommandArray commands);
bool ReproIt_Clay_Clicked(Clay_ElementId id); // use instead of your hit-test
void ReproIt_Clay_FrameEnd(void);
bool ReproIt_Clay_Done(void);
void ReproIt_Clay_Screen(const char *name); // optional screen/route anchor

// Mark an element as a VALUE NODE carrying a displayed value (docs/signature.md
// "Value-state", Layer 2/3). Clay text is excluded from the structural body, so a
// value-state app (a counter, a score, a clock) would otherwise collapse to one
// signature. Call this each frame for any element whose displayed value is part
// of the screen's identity; `stringId` matches the element's CLAY_ID string and
// `value` is the raw displayed string (bucketed locale-safely by value_class).
// The matching node is flagged value_node so its bounded value-class folds into
// the canonical signature's V: section; a value change then yields a new state.
void ReproIt_Clay_Value(const char *stringId, const char *value);

// Declare an element's accessibility (operability/accessibility graph). Clay, like
// Dear ImGui, emits NOTHING to any OS accessibility channel, so graph 2 is EMPTY
// by construction: every clickable element the header tracks is reported each
// frame in an EXPLORE:GROUNDTRUTH marker as operable:true with a11y all-false, so
// the whole interactive surface is honestly a gap (no role, no name, not in tab
// order, not keyboard-activatable). To SHRINK that gap, the app declares an
// element exposed BEFORE FrameEnd: ReproIt_Clay_A11y("Button_Play", "button",
// "Play", true) marks it as exposing a role + name to AT, so rolePresent/
// namePresent become true for that element. `stringId` matches the element's CLAY_ID string;
// `role`/`name` may be NULL; `exposed` defaults false (declared but not exposed).
void ReproIt_Clay_A11y(const char *stringId, const char *role, const char *name, bool exposed);

// --- Screenshot capture (the shoot contract; see the capture core above) ----
// Register the framebuffer-capture callback the app wires to its renderer (the
// header cannot know the graphics backend); with -DREPROIT_CAPTURE_GL and no
// callback set, the default glReadPixels path is used. ReproIt_Clay_Shoot("name")
// writes "$REPROIT_SHOTS_DIR/name.png" via the callback/GL default and prints
// "SHOOT:name". Both are guarded no-ops when REPROIT_SHOTS_DIR is unset, so
// capture-off builds (and runs without the env var) are unaffected. These simply
// forward to ReproIt_SetCaptureFn / ReproIt_Shoot in the capture core.
void ReproIt_Clay_SetCaptureFn(ReproIt_CaptureFn fn, void *user);
bool ReproIt_Clay_Shoot(const char *name);

// --- Production telemetry (optional; OFF unless REPROIT_TELEMETRY is defined) --
// In a SHIPPED build, the same ReproIt_Clay_* calls build the canonical tree each
// frame; telemetry then signs it with the EXISTING core and reports the real
// usage graph (states + edges) and crash signatures to the cloud, in the same
// {appId,sentAt,ctx?,events} contract the other SDKs POST (see the telemetry core
// above for the transport-callback contract and event shapes). This is fully
// separate from the fuzz driver: with telemetry on, the fuzzer does NOT pick or
// fire clicks; the app runs normally and reproit observes.
//
// Usage (shipped app):
//   #define REPROIT_TELEMETRY 1
//   #define REPROIT_CLAY_IMPLEMENTATION
//   #include "reproit_clay.h"
//   ReproIt_TeleOptions o = {0};
//   o.appId = "myapp"; o.sampleEnabled = true; o.installCrashHook = true;
//   o.transport = my_curl_post;   // or leave NULL for the spool-file transport
//   ReproIt_Telemetry_Init(&o);
//   ... each frame: ReproIt_Clay_Frame(cmds); ...clicks...; ReproIt_Clay_FrameEnd();
//   ... at shutdown: ReproIt_Telemetry_Shutdown();
// ReproIt_Telemetry_Init / _Observe / _Error / _Shutdown are declared in the
// telemetry core above; ReproIt_Clay_FrameEnd calls _Observe for you each frame.

#endif // REPROIT_CLAY_H

#ifdef REPROIT_CLAY_IMPLEMENTATION
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#ifndef REPROIT_TELEMETRY
// Fuzz-build crash handler needs POSIX signal + write(2). Telemetry builds pull
// these in from their own block; the fuzz build includes them here. The soak
// (--soak) RSS sampler + the per-frame jank watchdog need a monotonic clock and
// the current resident-set size, pulled in for the fuzz build below.
#include <signal.h>
#include <time.h>
#include <unistd.h>
#if defined(__APPLE__)
#include <mach/mach.h>
#endif
#endif

#define REPROIT_CLAY_MAX 256
#define REPROIT_CLAY_STR 64

#ifndef REPROIT_TELEMETRY
// ---- soak (MEMORY:SAMPLE) + jank (EXPLORE:JANK) shared primitives ----------
//
// Both run ONLY in the fuzz build (this header drives the app; a telemetry build
// reports, it is not driven). They are deterministic by construction: the leak
// verdict is a slope over the whole walk (a true leak grows monotonically; a
// neutral cycle collapses back), and the jank verdict is a COARSE, well-separated
// bucket (a normal Clay frame is < 16ms; the jank floor is 200ms, the hang floor
// 2000ms), so wall-clock jitter cannot flip either verdict and the finding id
// (from,action,bucket) is the same run to run.

// Monotonic milliseconds (CLOCK_MONOTONIC), for per-frame durations + soak time.
static uint64_t reproit_clay_mono_ms(void) {
  struct timespec ts;
  clock_gettime(CLOCK_MONOTONIC, &ts);
  return (uint64_t)ts.tv_sec * 1000ull + (uint64_t)(ts.tv_nsec / 1000000ll);
}

// Current resident-set size in BYTES for THIS process (the app under test, which
// hosts the fuzz driver), or 0 on failure. The OS-process analogue of the web
// runner's v8 heap_used: soak.rs reads first-vs-last for the per-cycle slope.
// Linux reads `/proc/self/statm` (pages -> bytes); Apple reads the Mach task's
// resident_size. A pure read of the OS, so the same process state yields the same
// number.
static uint64_t reproit_clay_rss_bytes(void) {
#if defined(__APPLE__)
  mach_task_basic_info_data_t info;
  mach_msg_type_number_t count = MACH_TASK_BASIC_INFO_COUNT;
  if (task_info(mach_task_self(), MACH_TASK_BASIC_INFO, (task_info_t)&info, &count) ==
      KERN_SUCCESS) {
    return (uint64_t)info.resident_size;
  }
  return 0;
#elif defined(__linux__)
  FILE *f = fopen("/proc/self/statm", "rb");
  if (!f)
    return 0;
  long pages = 0, resident = 0;
  int got = fscanf(f, "%ld %ld", &pages, &resident);
  fclose(f);
  if (got < 2 || resident <= 0)
    return 0;
  long pg = sysconf(_SC_PAGESIZE);
  if (pg <= 0)
    pg = 4096;
  return (uint64_t)resident * (uint64_t)pg;
#else
  return 0;
#endif
}

// Coarse, well-separated jank floors (ms), identical to the web runner so the two
// surfaces classify the same way. A frame at/over HANG_FLOOR is a freeze; at/over
// JANK_FLOOR is a dropped-frame stall; below it, nothing. The emitted marker
// carries the BUCKET (the floor), never the raw ms, so the detail is reproducible.
#define REPROIT_CLAY_JANK_FLOOR_MS 200
#define REPROIT_CLAY_HANG_FLOOR_MS 2000
#endif // !REPROIT_TELEMETRY

typedef struct {
  uint32_t rng;
  int budget, actions, settle;
  bool done, loaded;
  uint32_t fireId;
  char fireName[REPROIT_CLAY_STR]; // name of the chosen element, at pick time
  char prevSig[9];
  char anchor[REPROIT_CLAY_STR]; // optional screen/route anchor

  // Canonical structural tree built each frame. Nodes are pool-allocated;
  // index 0 is always the synthetic "screen" root.
  ReproItSig_Node nodes[REPROIT_CLAY_MAX];
  char nodeRole[REPROIT_CLAY_MAX][REPROIT_CLAY_STR];
  char nodeId[REPROIT_CLAY_MAX][REPROIT_CLAY_STR];
  char nodeValue[REPROIT_CLAY_MAX][REPROIT_CLAY_STR]; // displayed value (Layer 2)
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
  char capFull[REPROIT_CLAY_MAX][9]; // (struct, full) pairs, flat
  int nCap;

  // tappable elements this frame (for action selection). tapGesture[i] is the
  // GROUNDTRUTH gesture kind for tapIds[i]/tapNames[i] (graph 1).
  uint32_t tapIds[REPROIT_CLAY_MAX];
  char tapNames[REPROIT_CLAY_MAX][REPROIT_CLAY_STR];
  char tapGesture[REPROIT_CLAY_MAX][REPROIT_CLAY_STR];
  int nTaps;

  // App-declared accessibility marks for this frame (operability/accessibility
  // graph). For each declared element: its stringId and whether it exposes a
  // programmatic role / name to AT. Absent => all false (the honest default).
  char a11yId[REPROIT_CLAY_MAX][REPROIT_CLAY_STR];
  bool a11yRole[REPROIT_CLAY_MAX];
  bool a11yName[REPROIT_CLAY_MAX];
  int nA11y;

  // seen-state ring (signatures)
  char seen[REPROIT_CLAY_MAX][9];
  int nSeen;

#ifndef REPROIT_TELEMETRY
  // Content-bug scan input (EXPLORE:CONTENTBUG): for every TEXT render command
  // drawn this frame, the owning element's stable key + the drawn DISPLAY text
  // (what the user sees), scanned for broken-content artifacts. Captured in
  // command order; classified + deduped + sorted at FrameEnd. Fuzz-only.
  char contentKey[REPROIT_CLAY_MAX][REPROIT_CLAY_STR];
  char contentText[REPROIT_CLAY_MAX][REPROIT_CLAY_STR];
  int nContent;
#endif

#ifndef REPROIT_TELEMETRY
  // --soak: this run is a soak (the soak tier writes {"replay":[..]}). When set,
  // we sample this process's RSS per step into a MEMORY:SAMPLE series soak.rs
  // reads. Off => no samples (a plain fuzz walk is not a soak).
  bool soak;
  uint64_t soakStart; // monotonic ms at the first sample (t=0 base)

  // Per-frame jank watchdog. lastFrameMs is the monotonic timestamp at the start
  // of the previous frame; the gap to this frame's start is that frame's render
  // duration. While settling AFTER a fired action we track the MAX frame
  // duration so a transition whose frames blow the jank floor is reported on the
  // edge it belongs to (the action that caused the stall).
  uint64_t lastFrameMs;
  bool haveLastFrame;
  uint64_t windowMaxFrameMs;             // max frame duration since the last fire
  int windowJankFrames;                  // frames in the window over the jank floor
  char jankFrom[9];                      // the from-sig the pending action left
  char jankAction[REPROIT_CLAY_STR + 4]; // "tap:" + name of the action measured

  // Multi-actor scenario (the conductor protocol): active when
  // REPROIT_SCENARIO_BARRIER names a conductor. FrameEnd then plays ONE
  // actor, pulling each action from the conductor instead of fuzzing.
  bool scn;
  char scnBase[128];  // conductor base URL
  char scnRole[8];    // this actor's role letter
  bool scnJoined;     // role claimed/announced
  bool scnAckPending; // a dispatched ACT awaits its /done ack
#endif
} ReproItClay;
static ReproItClay rc;

static long reproit_json_int(const char *text, const char *key, long fallback) {
  char needle[64];
  snprintf(needle, sizeof needle, "\"%s\"", key);
  const char *p = strstr(text, needle);
  if (!p)
    return fallback;
  p = strchr(p, ':');
  return p ? strtol(p + 1, NULL, 10) : fallback;
}

static void reproit_clay_load(void) {
  rc.loaded = true;
  rc.budget = 36;
  rc.rng = 1;
#ifndef REPROIT_TELEMETRY
  // Multi-actor scenario: the orchestrator passes the conductor URL as env
  // (defines arrive as env on every non-flutter backend, this app included).
  const char *scn = getenv("REPROIT_SCENARIO_BARRIER");
  if (scn && scn[0]) {
    rc.scn = true;
    snprintf(rc.scnBase, sizeof rc.scnBase, "%s", scn);
  }
#endif
  const char *path = getenv("REPROIT_FUZZ_CONFIG");
  if (!path)
    return;
  FILE *f = fopen(path, "rb");
  if (!f)
    return;
  char text[8192];
  size_t n = fread(text, 1, sizeof text - 1, f);
  text[n] = 0;
  fclose(f);
  long seed = reproit_json_int(text, "seed", 0);
  rc.rng = seed ? (uint32_t)seed : 1;
  rc.budget = (int)reproit_json_int(text, "budget", 36);
#ifndef REPROIT_TELEMETRY
  // Soak mode is signalled by a "replay" key in the config (the soak tier writes
  // {"replay":[cycle x N]}); in that mode we sample RSS per step. A plain fuzz
  // config has no "replay", so the leak sampler stays off.
  rc.soak = strstr(text, "\"replay\"") != NULL;
#endif
}

static uint32_t reproit_clay_rng(uint32_t mod) {
  uint32_t s = rc.rng;
  s ^= s << 13;
  s ^= s >> 17;
  s ^= s << 5;
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
static const char *reproit_clay_role_for(const char *stringId) {
  static const struct {
    const char *prefix;
    const char *role;
  } MAP[] = {
      {"Button", "button"},       {"Btn", "button"},        {"Header", "header"},
      {"Title", "header"},        {"Text", "text"},         {"Label", "text"},
      {"Link", "link"},           {"Field", "textfield"},   {"Input", "textfield"},
      {"TextField", "textfield"}, {"Image", "image"},       {"Img", "image"},
      {"Icon", "icon"},           {"List", "list"},         {"Item", "listitem"},
      {"ListItem", "listitem"},   {"Tab", "tab"},           {"Switch", "switch"},
      {"Toggle", "switch"},       {"Checkbox", "checkbox"}, {"Check", "checkbox"},
      {"Radio", "radio"},         {"Slider", "slider"},     {"Menu", "menu"},
      {"MenuItem", "menuitem"},   {"Dialog", "dialog"},     {"Modal", "dialog"},
      {"Group", "group"},         {"Row", "group"},         {"Col", "group"},
      {"Spinner", "spinner"},     {"Toast", "toast"},       {"Badge", "badge"},
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

static ReproItSig_Node *reproit_clay_alloc(const char *role, const char *id) {
  if (rc.nNodes >= REPROIT_CLAY_MAX)
    return &rc.nodes[REPROIT_CLAY_MAX - 1];
  int idx = rc.nNodes++;
  ReproItSig_Node *n = &rc.nodes[idx];
  memset(n, 0, sizeof *n);
  snprintf(rc.nodeRole[idx], REPROIT_CLAY_STR, "%s", role ? role : "node");
  n->role = rc.nodeRole[idx];
  if (id && id[0]) {
    snprintf(rc.nodeId[idx], REPROIT_CLAY_STR, "%s", id);
    n->id = rc.nodeId[idx];
  }
  return n;
}

static void reproit_clay_add_child(ReproItSig_Node *parent, ReproItSig_Node *child) {
  if (parent->n_children < REPROIT_SIG_MAX_CHILDREN) {
    parent->children[parent->n_children++] = child;
  }
}

// Compute the canonical signature of the current frame's tree. When dropValues
// is true, value nodes are temporarily stripped so the V: section disappears,
// yielding the STRUCTURAL-ONLY signature (the value-cap fallback and bucket key).
static void reproit_clay_sig_impl(char out[9], bool dropValues) {
  const char *anchor = rc.anchor[0] ? rc.anchor : NULL;
  if (!dropValues) {
    ReproIt_Signature(anchor, &rc.nodes[0], out);
    return;
  }
  const char *saved[REPROIT_CLAY_MAX];
  for (int i = 0; i < rc.nNodes; i++) {
    saved[i] = rc.nodes[i].value;
    rc.nodes[i].value = NULL;
  }
  ReproIt_Signature(anchor, &rc.nodes[0], out);
  for (int i = 0; i < rc.nNodes; i++)
    rc.nodes[i].value = saved[i];
}

// Compute the emitted state signature with the Layer 2 value cap enforced: the
// FULL signature normally, but once a structural node accumulates more than 8
// distinct value-class combinations it falls back to structural-only so an
// adversarial value generator cannot explode the graph.
static void reproit_clay_sig(char out[9]) {
  char structural[9], full[9];
  reproit_clay_sig_impl(structural, true);
  reproit_clay_sig_impl(full, false);
  if (strcmp(structural, full) == 0) {
    snprintf(out, 9, "%s", full);
    return;
  }
  // Count distinct full sigs already seen for this structural node, and whether
  // this exact full sig was already recorded.
  int distinct = 0;
  bool known = false;
  for (int i = 0; i < rc.nCap; i++) {
    if (strcmp(rc.capStruct[i], structural) == 0) {
      distinct++;
      if (strcmp(rc.capFull[i], full) == 0)
        known = true;
    }
  }
  if (!known && distinct >= 8) {
    snprintf(out, 9, "%s", structural);
    return;
  } // cap hit
  if (!known && rc.nCap < REPROIT_CLAY_MAX) {
    snprintf(rc.capStruct[rc.nCap], 9, "%s", structural);
    snprintf(rc.capFull[rc.nCap], 9, "%s", full);
    rc.nCap++;
  }
  snprintf(out, 9, "%s", full);
}

void ReproIt_Clay_Screen(const char *name) {
  if (name)
    snprintf(rc.anchor, sizeof rc.anchor, "%s", name);
  else
    rc.anchor[0] = 0;
}

// Capture API: thin forwarders to the shared capture core.
void ReproIt_Clay_SetCaptureFn(ReproIt_CaptureFn fn, void *user) { ReproIt_SetCaptureFn(fn, user); }
bool ReproIt_Clay_Shoot(const char *name) { return ReproIt_Shoot(name); }

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
  if (!rc.loaded)
    reproit_clay_load();
#ifndef REPROIT_TELEMETRY
  // Per-frame jank watchdog: the gap between the start of the previous frame and
  // the start of this one is the previous frame's render duration. Accumulate
  // the MAX duration since the last fired action so the settle window's worst
  // frame is attributed to the transition that caused it. Idle frames before the
  // first action just prime lastFrameMs (jankAction empty => not accumulated).
  if (!rc.done) {
    uint64_t now = reproit_clay_mono_ms();
    if (rc.haveLastFrame && rc.jankAction[0]) {
      uint64_t dur = now - rc.lastFrameMs;
      if (dur > rc.windowMaxFrameMs)
        rc.windowMaxFrameMs = dur;
      if (dur >= REPROIT_CLAY_JANK_FLOOR_MS)
        rc.windowJankFrames++;
    }
    rc.lastFrameMs = now;
    rc.haveLastFrame = true;
  }
#endif
  rc.nNodes = 0;
  rc.nTaps = 0;
  rc.nMarks = 0;
  rc.nA11y = 0;
#ifndef REPROIT_TELEMETRY
  rc.nContent = 0;
#endif
  ReproItSig_Node *root = reproit_clay_alloc("screen", NULL); // index 0
  (void)root;

  for (int32_t i = 0; i < commands.length; i++) {
    Clay_RenderCommand *cmd = Clay_RenderCommandArray_Get(&commands, i);
    if (!cmd)
      continue;
    // Text commands carry localized strings -> excluded from the structural
    // descriptor (rule 1). They are still captured for the content-bug oracle,
    // which scans the VISIBLE text (not the structure) for broken-content
    // artifacts and keys the finding by the stable element id, not the text.
    if (cmd->commandType == CLAY_RENDER_COMMAND_TYPE_TEXT) {
#if !defined(REPROIT_TELEMETRY) && !defined(REPROIT_CLAY_NO_CONTENTBUG)
      // The drawn text slice lives in renderData.text.stringContents on Clay
      // releases that expose the standard render-data union (the same union a
      // Clay renderer reads to draw text). If your Clay differs, define
      // REPROIT_CLAY_NO_CONTENTBUG to skip this capture (the scan then stays
      // a documented gap rather than reading a wrong field). The finding key
      // is the text command's own stable element id (e<id>), deterministic
      // across runs for a fixed layout, matching the node keying above.
      if (cmd->id != 0 && rc.nContent < REPROIT_CLAY_MAX) {
        Clay_StringSlice s = cmd->renderData.text.stringContents;
        int len = (int)s.length;
        if (len < 0)
          len = 0;
        if (len > REPROIT_CLAY_STR - 1)
          len = REPROIT_CLAY_STR - 1;
        if (s.chars && len > 0) {
          snprintf(rc.contentKey[rc.nContent], REPROIT_CLAY_STR, "e%u", (unsigned)cmd->id);
          memcpy(rc.contentText[rc.nContent], s.chars, (size_t)len);
          rc.contentText[rc.nContent][len] = 0;
          rc.nContent++;
        }
      }
#endif
      continue;
    }
    // Only elements with a stable string-id contribute a node.
    if (cmd->id == 0)
      continue;
    // Recover the string-id text. Clay stores the element id; the string is
    // available via Clay_ElementId lookup on most releases. We approximate
    // with the numeric id when no string is available.
    char sid[REPROIT_CLAY_STR];
    sid[0] = 0;
#ifdef REPROIT_CLAY_HAS_STRINGID
    // If your Clay release exposes the string-id on the command, fill sid.
    // (Left as a hook; default path uses the numeric id below.)
#endif
    if (!sid[0])
      snprintf(sid, sizeof sid, "e%u", (unsigned)cmd->id);
    const char *role = reproit_clay_role_for(sid);
    ReproItSig_Node *n = reproit_clay_alloc(role, sid);
    reproit_clay_add_child(&rc.nodes[0], n);
  }
}

// Declare a value node for this frame (Layer 2/3). Recorded now and applied to
// the matching node in ReproIt_Clay_FrameEnd; call after ReproIt_Clay_Frame.
void ReproIt_Clay_Value(const char *stringId, const char *value) {
  if (!stringId || rc.nMarks >= REPROIT_CLAY_MAX)
    return;
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

// Map a clickable element's role to the GROUNDTRUTH gesture kind: text-entry
// roles are "field"; everything else interactive is a discrete "button".
static const char *reproit_clay_gesture_for(const char *role) {
  if (role && strcmp(role, "textfield") == 0)
    return "field";
  return "button";
}

// Record an element as tappable and report a click when real OR fuzzer-chosen.
bool ReproIt_Clay_Clicked(Clay_ElementId id) {
  char stableName[REPROIT_CLAY_STR];
  snprintf(stableName, sizeof stableName, "%.*s", (int)id.stringId.length, id.stringId.chars);
  if (rc.nTaps < REPROIT_CLAY_MAX) {
    rc.tapIds[rc.nTaps] = id.id;
    snprintf(rc.tapNames[rc.nTaps], REPROIT_CLAY_STR, "%s", stableName);
    snprintf(rc.tapGesture[rc.nTaps], REPROIT_CLAY_STR, "%s",
             reproit_clay_gesture_for(reproit_clay_role_for(rc.tapNames[rc.nTaps])));
    rc.nTaps++;
  }
  // Layout-only Clay elements do not necessarily emit a render command (a
  // button with no background/border is a common example). Clicked() is the
  // authoritative declaration of the interactive graph, so ensure every
  // declared tappable is also present in the canonical structural tree. This
  // makes screen changes reproducible even when every control is visually
  // transparent and the render-command stream contains text only.
  bool present = false;
  for (int i = 1; i < rc.nNodes; i++) {
    if (rc.nodes[i].id && strcmp(rc.nodes[i].id, stableName) == 0) {
      present = true;
      break;
    }
  }
  if (!present && stableName[0] && rc.nNodes < REPROIT_CLAY_MAX) {
    ReproItSig_Node *n = reproit_clay_alloc(reproit_clay_role_for(stableName), stableName);
    if (n)
      reproit_clay_add_child(&rc.nodes[0], n);
  }
  bool real = Clay_PointerOver(id); // app's normal hover/click path
  return real || (rc.fireId && rc.fireId == id.id);
}

// Declare an element's accessibility for this frame (operability/accessibility
// graph). Recorded now and folded into the EXPLORE:GROUNDTRUTH marker in
// ReproIt_Clay_FrameEnd; call after ReproIt_Clay_Frame, like ReproIt_Clay_Value.
void ReproIt_Clay_A11y(const char *stringId, const char *role, const char *name, bool exposed) {
  if (!stringId || rc.nA11y >= REPROIT_CLAY_MAX)
    return;
  snprintf(rc.a11yId[rc.nA11y], REPROIT_CLAY_STR, "%s", stringId);
  // Exposed-with-a-string is the only thing that flips a dimension true; an
  // exposed declaration with no role/name is a no-op (still a gap, honestly).
  rc.a11yRole[rc.nA11y] = exposed && role && role[0];
  rc.a11yName[rc.nA11y] = exposed && name && name[0];
  rc.nA11y++;
}

static bool reproit_seen(const char *sig) {
  for (int i = 0; i < rc.nSeen; i++)
    if (strcmp(rc.seen[i], sig) == 0)
      return true;
  if (rc.nSeen < REPROIT_CLAY_MAX) {
    snprintf(rc.seen[rc.nSeen++], 9, "%s", sig);
  }
  return false;
}

// Emit one EXPLORE:GROUNDTRUTH line for the current frame's interactive surface
// (single-line JSON). Every clickable element collected this frame is
// operable:true; its a11y dimensions are all-false BY DEFAULT (Clay exposes
// nothing to AT), upgraded to rolePresent/namePresent true only where the app
// declared the element exposed via ReproIt_Clay_A11y(). inTabOrder +
// keyboardActivatable stay false: Clay's elements are pointer-driven and not part
// of any reported keyboard/tab order, so this is the honest ground truth.
// Duplicate ids in a frame are reported once.
static void reproit_clay_emit_groundtruth(const char *sig) {
  printf("EXPLORE:GROUNDTRUTH {\"sig\":\"%s\",\"focusTrap\":false,\"elements\":[", sig);
  int emitted = 0;
  for (int i = 0; i < rc.nTaps; i++) {
    // Dedupe by stable id (an element click-checked twice in one frame).
    int dup = 0;
    for (int j = 0; j < i; j++)
      if (rc.tapIds[j] == rc.tapIds[i]) {
        dup = 1;
        break;
      }
    if (dup)
      continue;
    int rolePresent = 0, namePresent = 0;
    for (int a = 0; a < rc.nA11y; a++) {
      if (strcmp(rc.a11yId[a], rc.tapNames[i]) == 0) {
        rolePresent = rc.a11yRole[a];
        namePresent = rc.a11yName[a];
        break;
      }
    }
    printf("%s{\"id\":\"%s\",\"operable\":true,\"gestureKind\":\"%s\","
           "\"a11y\":{\"rolePresent\":%s,\"namePresent\":%s,"
           "\"inTabOrder\":false,\"keyboardActivatable\":false}}",
           emitted ? "," : "", rc.tapNames[i], rc.tapGesture[i], rolePresent ? "true" : "false",
           namePresent ? "true" : "false");
    emitted++;
  }
  printf("]}\n");
  fflush(stdout);
}

#ifndef REPROIT_TELEMETRY
// Print `s` to stdout as a JSON string body (no surrounding quotes), escaping the
// JSON control set (" \\ and the C0 controls), so the CONTENTBUG marker is valid
// JSON the Rust parser (map.rs) reads. Deterministic, no locale.
static void reproit_clay_print_json_escaped(const char *s) {
  for (const unsigned char *p = (const unsigned char *)s; *p; p++) {
    unsigned char c = *p;
    if (c == '"' || c == '\\') {
      putchar('\\');
      putchar((char)c);
    } else if (c == '\n') {
      fputs("\\n", stdout);
    } else if (c == '\r') {
      fputs("\\r", stdout);
    } else if (c == '\t') {
      fputs("\\t", stdout);
    } else if (c < 0x20) { /* drop other control bytes */
    } else
      putchar((char)c);
  }
}

// Emit one EXPLORE:CONTENTBUG line for the current frame's drawn text, keyed by
// the SAME state signature as STATE/GROUNDTRUTH (single-line JSON). Scans each
// captured text command's drawn text with the shared classifier (web-exact
// semantics + precedence), dedupes by (key,reason), sorts by key then reason, and
// prints items[]. Emitted ONLY when at least one artifact is found, so a clean app
// stays silent (no marker, no finding) and the finding id is byte-identical run to
// run / on replay. The text is clipped (already bounded by REPROIT_CLAY_STR).
static void reproit_clay_emit_contentbugs(const char *sig) {
  // Classify each captured text into (key, reason), deduped by key|reason. Both
  // arrays are flat, parallel; small frames so the O(n^2) dedup is fine.
  static int order[REPROIT_CLAY_MAX];
  int n = 0;
  for (int i = 0; i < rc.nContent; i++) {
    const char *reason = reproit_cbug_reason(rc.contentText[i]);
    if (!reason)
      continue;
    // Dedupe by (key, reason): skip if an earlier kept entry has both equal.
    int dup = 0;
    for (int k = 0; k < n; k++) {
      int j = order[k];
      if (strcmp(rc.contentKey[j], rc.contentKey[i]) == 0 &&
          strcmp(reproit_cbug_reason(rc.contentText[j]), reason) == 0) {
        dup = 1;
        break;
      }
    }
    if (!dup)
      order[n++] = i;
  }
  if (n == 0)
    return; // clean frame: stay silent
  // Insertion-sort the kept indices by key then reason (stable, small n) so the
  // marker is byte-identical run to run.
  for (int a = 1; a < n; a++) {
    int v = order[a];
    int b = a - 1;
    while (b >= 0) {
      int kc = strcmp(rc.contentKey[order[b]], rc.contentKey[v]);
      int cmp = kc != 0 ? kc
                        : strcmp(reproit_cbug_reason(rc.contentText[order[b]]),
                                 reproit_cbug_reason(rc.contentText[v]));
      if (cmp <= 0)
        break;
      order[b + 1] = order[b];
      b--;
    }
    order[b + 1] = v;
  }
  printf("EXPLORE:CONTENTBUG {\"sig\":\"%s\",\"items\":[", sig);
  for (int k = 0; k < n; k++) {
    int i = order[k];
    const char *reason = reproit_cbug_reason(rc.contentText[i]);
    if (k)
      putchar(',');
    fputs("{\"key\":\"", stdout);
    reproit_clay_print_json_escaped(rc.contentKey[i]);
    fputs("\",\"reason\":\"", stdout);
    reproit_clay_print_json_escaped(reason);
    fputs("\",\"text\":\"", stdout);
    reproit_clay_print_json_escaped(rc.contentText[i]);
    fputs("\"}", stdout);
  }
  printf("]}\n");
  fflush(stdout);
}
#endif // !REPROIT_TELEMETRY

// ---- FUZZ-BUILD CRASH HANDLER (async-signal-safe) -------------------------
//
// In the FUZZ build (REPROIT_TELEMETRY undefined) a fatal signal in the app
// under test would otherwise kill the process silently: the orchestrator sees
// the runner die mid-walk with no JOURNEY DONE and no crash marker, so the
// crash is not attributed to a node. This handler closes that gap exactly like
// the imgui header's: on SIGSEGV/SIGABRT/SIGBUS/SIGFPE/SIGILL it writes an
// `EXCEPTION CAUGHT BY ...` block to stdout (the marker drive.rs + the fuzz
// oracle parse) naming the current state signature and the action that led
// there, then re-raises so the OS still cores. The block is PRE-SERIALIZED on
// the hot path (FrameEnd) so the handler only does async-signal-safe write(2).
// The bucket key (kind + message) embeds signal + node + action, so the same
// crash reached the same way buckets to one finding. Mutually exclusive with
// the telemetry crash hook via the #ifndef guard.
#ifndef REPROIT_TELEMETRY
static char reproit_clay_crash_pre[256];
static size_t reproit_clay_crash_pre_len = 0;
static char reproit_clay_crash_post[256];
static size_t reproit_clay_crash_post_len = 0;
static volatile sig_atomic_t reproit_clay_crash_installed = 0;

static const char *reproit_clay_signame(int sig) {
  switch (sig) {
  case SIGSEGV:
    return "SIGSEGV";
  case SIGABRT:
    return "SIGABRT";
  case SIGBUS:
    return "SIGBUS";
  case SIGFPE:
    return "SIGFPE";
  case SIGILL:
    return "SIGILL";
  default:
    return "SIGNAL";
  }
}

static void reproit_clay_safe_write(const char *s) {
  size_t n = 0;
  while (s[n])
    n++;
  size_t off = 0;
  while (off < n) {
    long w = (long)write(1, s + off, n - off);
    if (w <= 0)
      break;
    off += (size_t)w;
  }
}
static void reproit_clay_safe_write_n(const char *s, size_t n) {
  size_t off = 0;
  while (off < n) {
    long w = (long)write(1, s + off, n - off);
    if (w <= 0)
      break;
    off += (size_t)w;
  }
}

static void reproit_clay_crash_handler(int sig) {
  reproit_clay_safe_write_n(reproit_clay_crash_pre, reproit_clay_crash_pre_len);
  reproit_clay_safe_write(reproit_clay_signame(sig));
  reproit_clay_safe_write_n(reproit_clay_crash_post, reproit_clay_crash_post_len);
  signal(sig, SIG_DFL);
  raise(sig);
}

static void reproit_clay_install_crash_hook(void) {
  if (reproit_clay_crash_installed)
    return;
  reproit_clay_crash_installed = 1;
  const char *pre = "EXCEPTION CAUGHT BY CLAY APP \xe2\x95\xa1 CLAY APP \xe2\x95\x9e\n"
                    "The following crash was raised by the app:\n"
                    "raised by signal ";
  size_t n = 0;
  while (pre[n])
    n++;
  if (n >= sizeof reproit_clay_crash_pre)
    n = sizeof reproit_clay_crash_pre - 1;
  memcpy(reproit_clay_crash_pre, pre, n);
  reproit_clay_crash_pre[n] = 0;
  reproit_clay_crash_pre_len = n;
  int sigs[] = {SIGSEGV, SIGABRT, SIGBUS, SIGFPE, SIGILL};
  for (unsigned i = 0; i < sizeof sigs / sizeof sigs[0]; i++)
    signal(sigs[i], reproit_clay_crash_handler);
}

// Rebuild the message tail + closing rule from the current state. Hot path only.
static void reproit_clay_build_crash_tail(void) {
  const char *sig = rc.prevSig[0] ? rc.prevSig : "?";
  const char *act = rc.fireName[0] ? rc.fireName : "(launch)";
  int w = snprintf(reproit_clay_crash_post, sizeof reproit_clay_crash_post,
                   " at state %s after action tap:%s\n"
                   "\xe2\x95\x90\xe2\x95\x90\xe2\x95\x90\xe2\x95\x90\xe2\x95\x90\xe2\x95\x90\xe2"
                   "\x95\x90\xe2\x95\x90\n",
                   sig, act);
  reproit_clay_crash_post_len =
      (w > 0 && (size_t)w < sizeof reproit_clay_crash_post) ? (size_t)w : 0;
}

// ---- multi-actor scenario driver (fuzz build only) -------------------------
// One FrameEnd's worth of the conductor client: called INSTEAD of the fuzz
// picker when REPROIT_SCENARIO_BARRIER is set, after the frame's STATE/EDGE
// were emitted (so a dispatched action's effect is already reported when its
// /done ack advances the conductor). The in-app action vocabulary:
//   tap:<stringId>        fire the click-checked element with that string-id
//   shoot:<name>          screenshot point (capture callback / GL default)
//   assert:text=<t>       this frame's drawn text contains <t>
//   assert:count:<f>=<n>  this frame holds exactly <n> nodes with string-id <f>
//   auth:<acct>           unsupported (no session store); loud no-op
//   back / type: / key:   Clay's hook drives clicks only (no text channel, no
//                         back affordance): FUZZ:MISS, so a cross-surface
//                         journey fails loudly instead of silently passing
// The host resolves ${REPROIT_SECRET_*} placeholders before the conductor
// serves an action, so values arrive literal. Crash attribution rides the
// existing pre-serialized handler; a crashed process never acks its step, so
// the conductor's diagnose() names this actor and action.
static void reproit_clay_scenario_step(void) {
  char resp[64];
  char path[64];
  if (!rc.scnJoined) {
    // Role identity: the per-process env label wins; a runner without one
    // claims a distinct role atomically from the conductor.
    const char *dev = getenv("REPROIT_DEVICE");
    if (dev && dev[0]) {
      snprintf(rc.scnRole, sizeof rc.scnRole, "%s", dev);
    } else {
      char role[32];
      if (reproit_scn_http(rc.scnBase, "GET", "/claim", role, sizeof role) && role[0] &&
          strncmp(role, "ERR", 3) != 0) {
        snprintf(rc.scnRole, sizeof rc.scnRole, "%s", role);
      } else {
        snprintf(rc.scnRole, sizeof rc.scnRole, "a");
      }
    }
    printf("JOURNEY claimed role=%s\n", rc.scnRole);
    fflush(stdout);
    rc.scnJoined = true;
  }
  if (rc.scnAckPending) {
    // The previously dispatched action has settled and its STATE/EDGE were
    // just emitted above; ack it so the conductor advances.
    snprintf(path, sizeof path, "/done?device=%s", rc.scnRole);
    reproit_scn_http(rc.scnBase, "POST", path, resp, sizeof resp);
    rc.scnAckPending = false;
  }
  char body[2048];
  snprintf(path, sizeof path, "/next?device=%s", rc.scnRole);
  if (!reproit_scn_http(rc.scnBase, "GET", path, body, sizeof body)) {
    reproit_scn_sleep_ms(100); // conductor unreachable: retry next frame
    return;
  }
  if (strcmp(body, "DONE") == 0) {
    printf("JOURNEY DONE\nAll tests passed\n");
    fflush(stdout);
    rc.done = true;
    return;
  }
  if (strcmp(body, "WAIT") == 0) {
    reproit_scn_sleep_ms(40); // not our turn / join barrier: poll cadence
    return;
  }
  const char *act = body;
  if (strncmp(act, "ACT\t", 4) == 0)
    act += 4;
  printf("FUZZ:ACT %s %s\n", rc.scnRole, act);
  fflush(stdout);

  bool handled_now = true; // verbs that complete within this FrameEnd
  if (strncmp(act, "shoot:", 6) == 0) {
    ReproIt_Shoot(act + 6);
  } else if (strncmp(act, "assert:", 7) == 0) {
    const char *a = act + 7;
    if (strncmp(a, "text=", 5) == 0) {
      const char *want = a + 5;
      bool found = false;
      for (int i = 0; i < rc.nContent && !found; i++) {
        if (strstr(rc.contentText[i], want))
          found = true;
      }
      printf("FUZZ:ASSERT %s text=\"", found ? "pass" : "fail");
      reproit_clay_print_json_escaped(want);
      printf("\" actor=%s\n", rc.scnRole);
    } else if (strncmp(a, "count:", 6) == 0) {
      const char *rest = a + 6;
      const char *eq = strrchr(rest, '=');
      char finder[REPROIT_CLAY_STR];
      long want = 0;
      if (eq) {
        size_t fl = (size_t)(eq - rest);
        if (fl >= sizeof finder)
          fl = sizeof finder - 1;
        memcpy(finder, rest, fl);
        finder[fl] = 0;
        want = strtol(eq + 1, NULL, 10);
      } else {
        snprintf(finder, sizeof finder, "%s", rest);
      }
      // Structural nodes carry the NUMERIC command id (e<id>) unless the
      // Clay release exposes string-ids, so a string finder resolves via
      // the click-check registry (the same names tap: fires); the e<id>
      // form still counts against the node tree.
      long got = 0;
      for (int i = 0; i < rc.nTaps; i++) {
        if (strcmp(rc.tapNames[i], finder) == 0)
          got++;
      }
      if (got == 0) {
        for (int i = 0; i < rc.nNodes; i++) {
          if (rc.nodes[i].id && strcmp(rc.nodes[i].id, finder) == 0)
            got++;
        }
      }
      printf("FUZZ:ASSERT %s count %s want=%ld got=%ld actor=%s\n", got == want ? "pass" : "fail",
             finder, want, got, rc.scnRole);
    } else {
      printf("FUZZ:ASSERT fail unsupported %s actor=%s\n", a, rc.scnRole);
    }
    fflush(stdout);
  } else if (strncmp(act, "auth:", 5) == 0) {
    printf("JOURNEY[a] step: auth-restore unsupported on the in-app hook; "
           "drive the login UI explicitly for %s\n",
           act);
    fflush(stdout);
  } else if (strncmp(act, "tap:", 4) == 0) {
    const char *name = act + 4;
    if (strncmp(name, "key:", 4) == 0)
      name += 4;
    int hit = -1;
    for (int i = 0; i < rc.nTaps; i++) {
      if (strcmp(rc.tapNames[i], name) == 0) {
        hit = i;
        break;
      }
    }
    if (hit >= 0) {
      rc.fireId = rc.tapIds[hit];
      snprintf(rc.fireName, sizeof rc.fireName, "%s", rc.tapNames[hit]);
      rc.actions++;
      rc.settle = 2;
      rc.scnAckPending = true; // ack once the fire settles
      // Arm the jank window + crash attribution like the fuzz picker.
      snprintf(rc.jankFrom, sizeof rc.jankFrom, "%s", rc.prevSig);
      snprintf(rc.jankAction, sizeof rc.jankAction, "tap:%s", rc.fireName);
      rc.windowMaxFrameMs = 0;
      rc.windowJankFrames = 0;
      reproit_clay_build_crash_tail();
      handled_now = false;
    } else {
      printf("FUZZ:MISS %s %s\n", rc.scnRole, act);
      fflush(stdout);
    }
  } else {
    // back / type: / key:<Name>: no affordance in this hook; fail loudly.
    printf("FUZZ:MISS %s %s\n", rc.scnRole, act);
    fflush(stdout);
  }
  if (handled_now) {
    snprintf(path, sizeof path, "/done?device=%s", rc.scnRole);
    reproit_scn_http(rc.scnBase, "POST", path, resp, sizeof resp);
  }
}
#endif // !REPROIT_TELEMETRY

void ReproIt_Clay_FrameEnd(void) {
#ifdef REPROIT_TELEMETRY
  // Production telemetry path: when telemetry is active, observe the real
  // session (sign the current tree with the existing core, report state/edge)
  // and return WITHOUT running the fuzz driver. This keeps the two paths fully
  // separate: a shipped app reports, it is not driven by the seeded walk. The
  // action is the last fuzzer/app click when present, else "auto".
  if (reproit_tele.active) {
    reproit_clay_apply_marks();
    const char *anchor = rc.anchor[0] ? rc.anchor : NULL;
    const char *action = rc.fireName[0] ? rc.fireName : NULL;
    ReproIt_Telemetry_Observe(anchor, &rc.nodes[0], action);
    rc.fireId = 0;
    rc.fireName[0] = 0;
    return;
  }
#endif
  if (rc.done)
    return;
#ifndef REPROIT_TELEMETRY
  // Arm the fuzz-build crash handler once, so a fatal signal in the app under
  // test surfaces as an attributed EXCEPTION block, not a silent death.
  reproit_clay_install_crash_hook();
#endif
  // Settle before reading, so each state/edge is emitted exactly once.
  if (rc.settle > 0) {
    rc.settle--;
    return;
  }
#ifndef REPROIT_TELEMETRY
  // JANK / HANG watchdog (EXPLORE:JANK / EXPLORE:HANG). The settle window for the
  // last fired action has now elapsed; if its WORST frame blew a floor, the
  // action stalled the UI. Classify by the coarse bucket (jitter cannot flip a
  // 200ms/2000ms-separated verdict) and key by (from, action) like the web
  // runner, so the engine attributes the stall to this exact transition and
  // `check` re-confirms it. Cleared after evaluation. The finding id is
  // (from, action, bucket): deterministic.
  if (rc.jankAction[0]) {
    if (rc.windowMaxFrameMs >= REPROIT_CLAY_HANG_FLOOR_MS) {
      printf("EXPLORE:HANG {\"from\":\"%s\",\"action\":\"%s\",\"bucket\":%d,\"count\":%d}\n",
             rc.jankFrom, rc.jankAction, REPROIT_CLAY_HANG_FLOOR_MS, rc.windowJankFrames);
      fflush(stdout);
    } else if (rc.windowMaxFrameMs >= REPROIT_CLAY_JANK_FLOOR_MS) {
      printf("EXPLORE:JANK {\"from\":\"%s\",\"action\":\"%s\",\"bucket\":%d,\"count\":%d}\n",
             rc.jankFrom, rc.jankAction, REPROIT_CLAY_JANK_FLOOR_MS, rc.windowJankFrames);
      fflush(stdout);
    }
    rc.jankAction[0] = 0;
    rc.jankFrom[0] = 0;
    rc.windowMaxFrameMs = 0;
    rc.windowJankFrames = 0;
  }
#endif
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
    // labels:[] is required by the engine's STATE parser (map.rs) for the
    // state to register; immediate-mode GUIs exclude localized text from the
    // descriptor by construction, so there are no labels to report.
    printf("EXPLORE:STATE {\"sig\":\"%s\",\"labels\":[],\"elements\":[", sig);
    bool first_auth_element = true;
    const char *purpose_prefix = "reproit-purpose-";
    for (int i = 0; i < rc.nNodes; i++) {
      ReproItSig_Node *n = &rc.nodes[i];
      if (!n->role || strcmp(n->role, "textfield") != 0 || !n->id || !n->id[0])
        continue;
      const char *purpose = NULL;
      size_t purpose_len = 0;
      if (strncmp(n->id, purpose_prefix, strlen(purpose_prefix)) == 0) {
        purpose = n->id + strlen(purpose_prefix);
        const char *end = strstr(purpose, "--");
        if (end)
          purpose_len = (size_t)(end - purpose);
      } else if (n->type && strcmp(n->type, "password") == 0) {
        purpose = "password";
        purpose_len = 8;
      }
      if (!purpose || !purpose_len)
        continue;
      char safe_id[REPROIT_CLAY_STR];
      size_t out = 0;
      for (const char *p = n->id; *p && out + 1 < sizeof safe_id; p++) {
        if ((*p >= 'a' && *p <= 'z') || (*p >= 'A' && *p <= 'Z') || (*p >= '0' && *p <= '9') ||
            *p == '-' || *p == '_' || *p == '.')
          safe_id[out++] = *p;
      }
      safe_id[out] = 0;
      if (!first_auth_element)
        printf(",");
      first_auth_element = false;
      printf("{\"sel\":\"key:%s\",\"role\":\"textfield\",\"label\":\"\",\"inputPurpose\":\"%.*s\"}",
             safe_id, (int)purpose_len, purpose);
    }
    printf("]}\n");
    fflush(stdout);
    // The operability/accessibility ground truth for this state: graph 1 is
    // every clickable element this frame (operable:true); graph 2 (OS a11y) is
    // empty by construction, so a11y defaults all-false and the whole surface
    // is a gap unless the app declared elements exposed via ReproIt_Clay_A11y.
    // Emitted once per newly seen state (same key as STATE).
    reproit_clay_emit_groundtruth(sig);
#ifndef REPROIT_TELEMETRY
    // Content-bug scan for this newly-seen state, keyed by the SAME sig. A pure
    // scan of the drawn text (no pixels, no timing), so it reproduces on replay.
    // Only emitted when a broken-content artifact was actually drawn, so a clean
    // app stays silent (no marker, no finding).
    reproit_clay_emit_contentbugs(sig);
#endif
    // Capture a screenshot of each newly discovered state (the shoot
    // contract). Guarded inside ReproIt_Shoot on REPROIT_SHOTS_DIR + a
    // registered capture mechanism, so this is a no-op otherwise. The name is
    // the state signature, which is in [A-Za-z0-9].
    ReproIt_Shoot(sig);
  }
  if (rc.prevSig[0] && strcmp(sig, rc.prevSig) != 0 && rc.fireId) {
    // The fired element belonged to the PREVIOUS screen, whose taps are no
    // longer in tapNames; use the name captured when the action was picked.
    printf("EXPLORE:EDGE {\"from\":\"%s\",\"action\":\"tap:%s\",\"to\":\"%s\"}\n", rc.prevSig,
           rc.fireName, sig);
    fflush(stdout);
  }

  snprintf(rc.prevSig, 9, "%s", sig);
  rc.fireId = 0;
  rc.fireName[0] = 0;

#ifndef REPROIT_TELEMETRY
  // LEAK sampler (--soak): emit one MEMORY:SAMPLE per step with this process's
  // current RSS, the SAME shape the desktop/web runners emit (heap_used carries
  // RSS bytes), so soak.rs reconstructs the RSS-vs-time series and reads the
  // slope. t_ms is monotonic from the first sample. No-op outside soak.
  if (rc.soak) {
    uint64_t now = reproit_clay_mono_ms();
    if (rc.soakStart == 0)
      rc.soakStart = now;
    uint64_t rss = reproit_clay_rss_bytes();
    if (rss) {
      printf("MEMORY:SAMPLE {\"t_ms\":%llu,\"heap_used\":%llu}\n",
             (unsigned long long)(now - rc.soakStart), (unsigned long long)rss);
      fflush(stdout);
    }
  }
#endif

#ifndef REPROIT_TELEMETRY
  // Multi-actor scenario: this process plays ONE actor, pulling each action
  // from the host conductor instead of fuzzing. The budget and the
  // no-tappables early finish do not apply (the conductor owns termination).
  if (rc.scn) {
    reproit_clay_scenario_step();
    return;
  }
#endif

  if (rc.actions >= rc.budget || rc.nTaps == 0) {
    printf("JOURNEY[a] step: explored %d states\nJOURNEY DONE\nAll tests passed\n", rc.nSeen);
    fflush(stdout);
    rc.done = true;
    return;
  }
  if (rc.actions == 0) {
    printf("JOURNEY claimed role=a\n");
    fflush(stdout);
  }

  int pick = (int)reproit_clay_rng((uint32_t)rc.nTaps);
  rc.fireId = rc.tapIds[pick];
  snprintf(rc.fireName, sizeof rc.fireName, "%s", rc.tapNames[pick]);
  printf("FUZZ:ACT tap:%s\n", rc.tapNames[pick]);
  fflush(stdout);
  rc.actions++;
  rc.settle = 2;
#ifndef REPROIT_TELEMETRY
  // Arm the jank window for the action about to fire: its settle frames are
  // measured (in ReproIt_Clay_Frame) and evaluated when the window elapses next
  // FrameEnd. from = the state we are in (prevSig); action = the edge label.
  snprintf(rc.jankFrom, sizeof rc.jankFrom, "%s", rc.prevSig);
  snprintf(rc.jankAction, sizeof rc.jankAction, "tap:%s", rc.fireName);
  rc.windowMaxFrameMs = 0;
  rc.windowJankFrames = 0;
#endif
#ifndef REPROIT_TELEMETRY
  // Refresh the pre-serialized crash payload to the state we are in (prevSig)
  // and the action about to fire (fireName), so a crash in the app's handler
  // for this action is attributed to this exact (state, action).
  reproit_clay_build_crash_tail();
#endif
}

bool ReproIt_Clay_Done(void) { return rc.done; }

#endif // REPROIT_CLAY_IMPLEMENTATION

#endif // REPROIT_SIG_CORE_ONLY
