// test_signature.c - parity gate for the canonical structural signature.
//
// Includes ONLY the self-contained signature core from reproit_clay.h (the core
// block guards itself with REPROIT_SIGNATURE_CORE_H and depends on nothing but
// libc, so we can pull it in without clay.h). The same core is embedded byte-
// for-byte in reproit_imgui.h, so passing here proves both runners.
//
// The golden vectors are hardcoded as C trees derived from signature_vectors.json
// (rather than parsing JSON in C). It asserts ReproIt_Signature(anchor, tree) ==
// expected_sig for ALL vectors, mirroring the Rust oracle's golden_vectors_match.
//
// Build:  clang -std=c11 -Wall -Wextra -o /tmp/test_signature runners/test_signature.c
// Run:    /tmp/test_signature

#include <stdio.h>
#include <string.h>

// Pull in just the canonical core (no clay.h needed).
#define REPROIT_SIG_CORE_ONLY
#include "reproit_clay.h"

// ---- tiny tree builder helpers -------------------------------------------
// Nodes live in a static pool so we can build literal trees without malloc.
#define POOL 256
static ReproItSig_Node POOLN[POOL];
static int POOLI = 0;

static ReproItSig_Node *N(const char *role) {
  ReproItSig_Node *n = &POOLN[POOLI++];
  memset(n, 0, sizeof *n);
  n->role = role;
  return n;
}
static ReproItSig_Node *with_id(ReproItSig_Node *n, const char *id) {
  n->id = id;
  return n;
}
static ReproItSig_Node *with_type(ReproItSig_Node *n, const char *t) {
  n->type = t;
  return n;
}
static ReproItSig_Node *with_icon(ReproItSig_Node *n, const char *ic) {
  n->icon = ic;
  return n;
}
static ReproItSig_Node *with_transient(ReproItSig_Node *n) {
  n->transient = true;
  return n;
}
// Layer 2 value-state: attach a displayed value, or opt a node in as value-bearing.
static ReproItSig_Node *with_value(ReproItSig_Node *n, const char *v) {
  n->value = v;
  return n;
}
static ReproItSig_Node *with_value_node(ReproItSig_Node *n) {
  n->value_node = true;
  return n;
}
static ReproItSig_Node *kid(ReproItSig_Node *parent, ReproItSig_Node *child) {
  parent->children[parent->n_children++] = child;
  return parent;
}

static void reset_pool(void) { POOLI = 0; }

// ---- a vector and the table ----------------------------------------------
typedef struct {
  const char *description;
  const char *anchor; // NULL for JSON null
  ReproItSig_Node *(*build)(void);
  const char *expected;
} Vector;

// Each builder resets the pool and returns the root.

static ReproItSig_Node *v_empty(void) {
  reset_pool();
  return N("screen");
}

// basic login + the locale/transient variants all expect cae5a9d5.
static ReproItSig_Node *login_base(void) {
  ReproItSig_Node *s = N("screen");
  kid(s, with_id(N("header"), "title"));
  kid(s, with_type(with_id(N("textfield"), "email"), "email"));
  kid(s, with_type(with_id(N("textfield"), "password"), "password"));
  kid(s, with_id(N("button"), "submit"));
  return s;
}
static ReproItSig_Node *v_login(void) {
  reset_pool();
  return login_base();
}
static ReproItSig_Node *v_locale(void) {
  reset_pool();
  return login_base();
}

static ReproItSig_Node *v_spinner(void) {
  reset_pool();
  ReproItSig_Node *s = login_base();
  kid(s, N("spinner"));
  return s;
}

static ReproItSig_Node *v_snackbar(void) {
  reset_pool();
  ReproItSig_Node *s = N("screen");
  kid(s, with_id(N("header"), "title"));
  kid(s, with_type(with_id(N("textfield"), "email"), "email"));
  ReproItSig_Node *banner = with_transient(N("group"));
  kid(banner, N("text"));
  kid(s, banner);
  kid(s, with_type(with_id(N("textfield"), "password"), "password"));
  kid(s, with_id(N("button"), "submit"));
  ReproItSig_Node *snack = N("snackbar");
  kid(snack, N("text"));
  kid(snack, N("button"));
  kid(s, snack);
  return s;
}

static ReproItSig_Node *listitem(void) {
  ReproItSig_Node *li = N("listitem");
  kid(li, N("image"));
  kid(li, N("text"));
  return li;
}
static ReproItSig_Node *v_feed3(void) {
  reset_pool();
  ReproItSig_Node *l = N("list");
  for (int i = 0; i < 3; i++)
    kid(l, listitem());
  return l;
}
static ReproItSig_Node *v_feed5(void) {
  reset_pool();
  ReproItSig_Node *l = N("list");
  for (int i = 0; i < 5; i++)
    kid(l, listitem());
  return l;
}

static ReproItSig_Node *v_collision_type(void) {
  reset_pool();
  ReproItSig_Node *s = N("screen");
  kid(s, with_id(N("header"), "title"));
  kid(s, with_type(with_id(N("textfield"), "email"), "email"));
  kid(s, with_type(with_id(N("textfield"), "password"), "text"));
  kid(s, with_id(N("button"), "submit"));
  return s;
}

static ReproItSig_Node *v_collision_icon(void) {
  reset_pool();
  ReproItSig_Node *s = N("screen");
  kid(s, with_id(N("header"), "title"));
  kid(s, with_type(with_id(N("textfield"), "email"), "email"));
  kid(s, with_type(with_id(N("textfield"), "password"), "password"));
  kid(s, with_icon(with_id(N("button"), "submit"), "e5cd"));
  return s;
}

static ReproItSig_Node *settings_tree(void) {
  ReproItSig_Node *s = N("screen");
  kid(s, with_id(N("header"), "title"));
  kid(s, with_id(N("switch"), "notifications"));
  return s;
}
static ReproItSig_Node *v_settings(void) {
  reset_pool();
  return settings_tree();
}
static ReproItSig_Node *v_profile(void) {
  reset_pool();
  return settings_tree();
}

static ReproItSig_Node *v_wizard2(void) {
  reset_pool();
  ReproItSig_Node *s = N("screen");
  kid(s, with_id(N("header"), "title"));
  kid(s, with_type(with_id(N("textfield"), "name"), "text"));
  kid(s, with_id(N("button"), "next"));
  return s;
}

static ReproItSig_Node *item_tree(void) {
  ReproItSig_Node *s = N("screen");
  kid(s, with_id(N("image"), "hero"));
  kid(s, with_id(N("header"), "name"));
  kid(s, with_id(N("button"), "buy"));
  return s;
}
static ReproItSig_Node *v_item42(void) {
  reset_pool();
  return item_tree();
}
static ReproItSig_Node *v_item99(void) {
  reset_pool();
  return item_tree();
}

static ReproItSig_Node *v_unknown_role(void) {
  reset_pool();
  ReproItSig_Node *g = N("group");
  kid(g, with_id(N("button"), "a"));
  kid(g, N("carousel"));
  kid(g, with_id(N("button"), "b"));
  return g;
}

// ---- Layer 2 value-state vectors (the 9 new golden vectors) ---------------
// All mirror signature_vectors.json: a value-role/value_node node with a value
// folds a bounded value-class into the canonical V: section.

// A /form screen with one textfield@amount carrying the given value. textfield is
// a value-role, so the value-class enters the V: section.
static ReproItSig_Node *form_amount(const char *value) {
  ReproItSig_Node *s = N("screen");
  kid(s, with_value(with_id(N("textfield"), "amount"), value));
  return s;
}
static ReproItSig_Node *v_val_empty(void) {
  reset_pool();
  return form_amount("");
}
static ReproItSig_Node *v_val_zero(void) {
  reset_pool();
  return form_amount("0");
}
static ReproItSig_Node *v_val_pos1(void) {
  reset_pool();
  return form_amount("3");
}
static ReproItSig_Node *v_val_grouped(void) {
  reset_pool();
  return form_amount("1,234");
}

// A /counter screen with one status@count carrying the given value. status is a
// value-role but NOT in the structural vocabulary, so the body token is `node`.
static ReproItSig_Node *counter(const char *value) {
  ReproItSig_Node *s = N("screen");
  kid(s, with_value(with_id(N("status"), "count"), value));
  return s;
}
static ReproItSig_Node *v_counter0(void) {
  reset_pool();
  return counter("0");
}
static ReproItSig_Node *v_counter5(void) {
  reset_pool();
  return counter("5");
}
static ReproItSig_Node *v_counter_p3(void) {
  reset_pool();
  return counter("3");
}
static ReproItSig_Node *v_counter_p7(void) {
  reset_pool();
  return counter("7");
}

// A /home screen with a header@title carrying a value. header is chrome, so it is
// NEVER value-bearing: NO V: section, byte-identical to the same tree with no
// value. To also exercise the opt-in value_node path (Layer 3) without changing
// the expected hash, a value_node-flagged node WITHOUT a value is not value-
// bearing either (no value), so adding one would not alter the signature; we keep
// the vector exactly as the oracle's golden vector specifies.
static ReproItSig_Node *v_chrome_value(void) {
  reset_pool();
  ReproItSig_Node *s = N("screen");
  kid(s, with_value(with_id(N("header"), "title"), "Welcome"));
  return s;
}

// UNICODE vector: a non-ASCII anchor (/café), a header whose id is non-ASCII
// (café), a button with an astral/emoji icon (❤), and two value-bearing status
// nodes whose ids are an astral emoji (🎉) and a high-BMP CJK-compat glyph (豈).
// C source string literals are already UTF-8 bytes, and the core hashes bytes +
// sorts the V: section by strcmp (byte order), so this matches the Rust oracle:
// key:豈 (lead byte 0xEF) sorts BEFORE key:🎉 (lead byte 0xF0) -> ZERO then POS1.
static ReproItSig_Node *v_unicode(void) {
  reset_pool();
  ReproItSig_Node *s = N("screen");
  kid(s, with_id(N("header"), "caf\xc3\xa9"));
  kid(s, with_icon(with_id(N("button"), "submit"), "\xe2\x9d\xa4"));
  kid(s, with_value(with_id(N("status"), "\xf0\x9f\x8e\x89"), "5"));
  kid(s, with_value(with_id(N("status"), "\xef\xa4\x80"), "0"));
  return s;
}

static Vector VECTORS[] = {
    {"empty screen", NULL, v_empty, "34fa65ea"},
    {"basic login", "/login", v_login, "cae5a9d5"},
    {"locale-invariance", "/login", v_locale, "cae5a9d5"},
    {"transient-drop (spinner)", "/login", v_spinner, "cae5a9d5"},
    {"transient-drop (snackbar)", "/login", v_snackbar, "cae5a9d5"},
    {"repeated-collapse (3)", "/feed", v_feed3, "8793941c"},
    {"repeated-collapse (5)", "/feed", v_feed5, "8793941c"},
    {"collision via input type", "/login", v_collision_type, "228f6b63"},
    {"collision via icon", "/login", v_collision_icon, "d3d9482f"},
    {"settings", "/settings", v_settings, "f62301bb"},
    {"profile", "/profile", v_profile, "36825249"},
    {"wizard step 2", "/settings", v_wizard2, "9ce65f1b"},
    {"item 42", "/item/:id", v_item42, "6c562c77"},
    {"item 99", "/item/:id", v_item99, "6c562c77"},
    {"unknown role + non-adj", NULL, v_unknown_role, "0a747964"},
    // Layer 2 value-state: the 9 value vectors.
    {"value EMPTY", "/form", v_val_empty, "295b0231"},
    {"value ZERO", "/form", v_val_zero, "1bdbac42"},
    {"value POS1", "/form", v_val_pos1, "093cd391"},
    {"value grouped NONEMPTY", "/form", v_val_grouped, "a605e0f0"},
    {"counter at 0 (ZERO)", "/counter", v_counter0, "1bb94c41"},
    {"counter at 5 (POS1)", "/counter", v_counter5, "2517a50a"},
    {"counter POS1 (3)", "/counter", v_counter_p3, "2517a50a"},
    {"counter POS1 (7)", "/counter", v_counter_p7, "2517a50a"},
    {"chrome value (no V:)", "/home", v_chrome_value, "c9416741"},
    // Unicode: non-ASCII descriptor + UTF-8 byte-order V: sort.
    {"unicode descriptor + V:", "/caf\xc3\xa9", v_unicode, "234cd190"},
};

// Extra Layer 2 checks that exercise paths the table cannot (the opt-in
// value_node flag and the keyless structural-index V: key), asserting the exact
// descriptor against the Rust oracle's unit tests in signature.rs.
static int extra_value_checks(void) {
  int fails = 0;
  // opt_in_value_node_flag: a `text` (chrome) node is not value-bearing even
  // with a value, UNLESS flagged value_node (Layer 3).
  reset_pool();
  ReproItSig_Node *t = with_value(with_id(N("text"), "display"), "42");
  ReproItSig_Buf d;
  reproit_sig_descriptor(NULL, t, &d);
  if (strcmp(d.buf, "A:\n0:text@display") != 0) {
    fails++;
    printf("FAIL  opt-in (unflagged) descriptor = %s\n", d.buf);
  }
  with_value_node(t);
  reproit_sig_descriptor(NULL, t, &d);
  if (strcmp(d.buf, "A:\n0:text@display\nV:key:display=POS2") != 0) {
    fails++;
    printf("FAIL  opt-in (flagged) descriptor = %s\n", d.buf);
  }
  // keyless_value_node_uses_structural_index: two keyless textfields collapse
  // structurally to one `*` token, but the V: section distinguishes them by
  // role:<role>#<idx>.
  reset_pool();
  ReproItSig_Node *s = N("screen");
  kid(s, with_value(N("textfield"), "3"));
  kid(s, with_value(N("textfield"), "99"));
  reproit_sig_descriptor(NULL, s, &d);
  if (strcmp(d.buf, "A:\n0:screen;1:textfield*\nV:role:textfield#0=POS1;role:textfield#1=POS2") !=
      0) {
    fails++;
    printf("FAIL  keyless structural-index descriptor = %s\n", d.buf);
  }
  if (!fails)
    printf("ok    extra value-state checks (opt-in flag, keyless index)\n");
  return fails;
}

int main(void) {
  int n = (int)(sizeof VECTORS / sizeof *VECTORS);
  int fails = 0;
  for (int i = 0; i < n; i++) {
    Vector *v = &VECTORS[i];
    ReproItSig_Node *root = v->build();
    char got[9];
    ReproIt_Signature(v->anchor, root, got);

    ReproItSig_Buf desc;
    reproit_sig_descriptor(v->anchor, root, &desc);

    if (strcmp(got, v->expected) != 0) {
      fails++;
      printf("FAIL  %-28s expected %s got %s\n", v->description, v->expected, got);
      printf("      descriptor = %s\n", desc.buf);
    } else {
      printf("ok    %-28s %s\n", v->description, got);
    }
  }
  fails += extra_value_checks();

  if (fails) {
    printf("\n%d failure(s) across %d vectors + extra checks\n", fails, n);
    return 1;
  }
  printf("\nall %d vectors pass (+ extra value-state checks)\n", n);
  return 0;
}
