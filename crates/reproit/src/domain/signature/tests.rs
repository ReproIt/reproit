use super::*;

/// One golden vector from signature_vectors.json.
#[derive(Deserialize)]
struct Vector {
    description: String,
    anchor: Option<String>,
    tree: Node,
    expected_sig: String,
}

fn load_vectors() -> Vec<Vector> {
    // signature_vectors.json lives at the repo root. CARGO_MANIFEST_DIR for
    // this crate is <repo>/crates/reproit, so go up two levels.
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("signature_vectors.json");
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {}", path.display(), e));
    serde_json::from_str(&text).expect("parse signature_vectors.json")
}

/// THE parity gate. Other languages mirror these three lines.
#[test]
fn golden_vectors_match() {
    let vectors = load_vectors();
    assert!(
        vectors.len() >= 24,
        "need >= 24 vectors, got {}",
        vectors.len()
    );
    for v in &vectors {
        let got = signature(v.anchor.as_deref(), &v.tree);
        assert_eq!(
            got,
            v.expected_sig,
            "vector '{}' mismatch.\n  descriptor = {:?}\n  expected {} got {}",
            v.description,
            descriptor(v.anchor.as_deref(), &v.tree),
            v.expected_sig,
            got
        );
    }
}

/// Assert the cross-vector relationships the spec promises, by description
/// keyword, so a future edit to a vector that breaks a guarantee is caught.
#[test]
fn vector_relationships_hold() {
    let vectors = load_vectors();
    let by = |needle: &str| -> String {
        vectors
            .iter()
            .find(|v| v.description.contains(needle))
            .map(|v| v.expected_sig.clone())
            .unwrap_or_else(|| panic!("no vector matching {:?}", needle))
    };
    let login = by("basic login");
    // text-exclusion + transient-drop all collapse to the basic login.
    assert_eq!(login, by("locale-invariance"));
    assert_eq!(login, by("transient-drop (spinner)"));
    assert_eq!(login, by("transient-drop (snackbar"));
    // collapse drops the count.
    assert_eq!(
        by("repeated-collapse (3 items)"),
        by("repeated-collapse (5 items")
    );
    // discriminators split.
    assert_ne!(login, by("collision-fix via input type"));
    assert_ne!(login, by("collision-fix via icon"));
    assert_ne!(
        by("collision-fix via input type"),
        by("collision-fix via icon")
    );
    // anchor semantics.
    let settings = by("same route + same structure");
    assert_ne!(settings, by("different route + same structure"));
    assert_ne!(settings, by("same route + different structure"));
    assert_eq!(
        by("parameterized route (item 42)"),
        by("parameterized route (item 99)")
    );

    // value-state (Layer 2): EMPTY / ZERO / POS1 are three distinct states.
    let v_empty = by("empty value-class");
    let v_zero = by("zero value-class");
    let v_pos1 = by("POS1 value-class");
    assert_ne!(v_empty, v_zero);
    assert_ne!(v_empty, v_pos1);
    assert_ne!(v_zero, v_pos1);
    // numeric counter 0 vs 5 -> ZERO vs POS1 distinct.
    assert_ne!(by("counter at 0"), by("counter at 5"));
    // A chrome label with text (no value) stays structural: identical to the
    // same structure with no value field. The empty-anchor structural form is
    // exercised here by comparing to a hand-built structural sig.
    {
        let mut s = Node::new("screen");
        let mut h = Node::new("header");
        h.id = Some("title".into());
        s.children.push(h);
        assert_eq!(signature(Some("/home"), &s), by("chrome label with text"));
    }
    // grouped/locale number is locale-safe (NONEMPTY), distinct from numerics.
    let v_grouped = by("grouped/locale number");
    assert_ne!(v_grouped, v_pos1);
    assert_ne!(v_grouped, v_zero);
    // two different POS1 values (3 vs 7) bucket the same.
    assert_eq!(
        by("two different POS1 values bucket the same (3)"),
        by("two different POS1 values bucket the same (7)")
    );
}

#[test]
fn fnv1a_known_value() {
    // "" -> the FNV-1a 32-bit offset basis itself.
    assert_eq!(fnv1a32_hex(b""), "811c9dc5");
    // Cross-check a known FNV-1a 32 value for "a" = 0xe40c292c.
    assert_eq!(fnv1a32_hex(b"a"), "e40c292c");
}

#[test]
fn unknown_role_maps_to_node() {
    let n = Node::new("carousel");
    assert_eq!(descriptor(None, &n), "A:\n0:node");
}

#[test]
fn empty_anchor_still_has_prefix_line() {
    let n = Node::new("screen");
    assert_eq!(descriptor(None, &n), "A:\n0:screen");
    assert_eq!(descriptor(Some(""), &n), "A:\n0:screen");
}

#[test]
fn transient_subtree_dropped() {
    let mut with = Node::new("screen");
    with.children.push(Node::new("text"));
    let mut spinner = Node::new("spinner");
    spinner.children.push(Node::new("text")); // subtree goes too
    with.children.push(spinner);

    let mut without = Node::new("screen");
    without.children.push(Node::new("text"));

    assert_eq!(descriptor(None, &with), descriptor(None, &without));
}

#[test]
fn transient_flag_dropped() {
    let mut banner = Node::new("group");
    banner.transient = true;
    let mut with = Node::new("screen");
    with.children.push(banner);
    let without = Node::new("screen");
    assert_eq!(descriptor(None, &with), descriptor(None, &without));
}

#[test]
fn repeated_siblings_collapse_regardless_of_count() {
    let item = || {
        let mut li = Node::new("listitem");
        li.children.push(Node::new("text"));
        li
    };
    let mk = |n: usize| {
        let mut list = Node::new("list");
        for _ in 0..n {
            list.children.push(item());
        }
        list
    };
    assert_eq!(descriptor(None, &mk(3)), descriptor(None, &mk(5)));
    // The collapsed token carries the marker exactly once.
    assert_eq!(descriptor(None, &mk(3)), "A:\n0:list;1:listitem*;2:text");
}

#[test]
fn single_child_not_marked() {
    let mut list = Node::new("list");
    let mut li = Node::new("listitem");
    li.children.push(Node::new("text"));
    list.children.push(li);
    assert_eq!(descriptor(None, &list), "A:\n0:list;1:listitem;2:text");
}

#[test]
fn non_consecutive_identical_not_collapsed() {
    // a, b, a -> the two `a`s are not consecutive, so no collapse.
    let mut g = Node::new("group");
    g.children.push(Node::new("button"));
    g.children.push(Node::new("link"));
    g.children.push(Node::new("button"));
    assert_eq!(descriptor(None, &g), "A:\n0:group;1:button;1:link;1:button");
}

#[test]
fn token_field_order() {
    let mut n = Node::new("textfield");
    n.type_ = Some("password".into());
    n.icon = Some("lock".into());
    n.id = Some("pwd".into());
    assert_eq!(descriptor(None, &n), "A:\n0:textfield:password#lock@pwd");
}

#[test]
fn selector_prefers_id() {
    let mut n = Node::new("button");
    n.id = Some("submit".into());
    let s = selector(&n, 3);
    assert_eq!(s.selector, "key:submit");
    assert!(!s.nokey);

    let m = Node::new("button");
    let s2 = selector(&m, 2);
    assert_eq!(s2.selector, "role:button#2");
    assert!(s2.nokey);
}

// --- Layer 2: value-state ------------------------------------------------

#[test]
fn value_class_all_buckets() {
    assert_eq!(value_class(""), "EMPTY");
    assert_eq!(value_class("   "), "EMPTY");
    assert_eq!(value_class("0"), "ZERO");
    assert_eq!(value_class("0.0"), "ZERO");
    assert_eq!(value_class("-0"), "ZERO");
    assert_eq!(value_class("-3"), "NEG");
    assert_eq!(value_class("-0.5"), "NEG");
    assert_eq!(value_class("3"), "POS1");
    assert_eq!(value_class("9.99"), "POS1");
    assert_eq!(value_class("+7"), "POS1");
    assert_eq!(value_class("10"), "POS2");
    assert_eq!(value_class("99"), "POS2");
    assert_eq!(value_class("100"), "POS3");
    assert_eq!(value_class("999.99"), "POS3");
    assert_eq!(value_class("1000"), "POSL");
    assert_eq!(value_class("123456"), "POSL");
    // Trimming is applied before classification.
    assert_eq!(value_class("  42  "), "POS2");
}

#[test]
fn value_class_locale_safe_fallback() {
    // Grouped / locale numbers are ambiguous; we do NOT guess -> NONEMPTY.
    assert_eq!(value_class("1,234"), "NONEMPTY");
    assert_eq!(value_class("1.234.567"), "NONEMPTY"); // de-DE grouping
    assert_eq!(value_class("1 234"), "NONEMPTY"); // thin-space grouping
    assert_eq!(value_class("$5"), "NONEMPTY");
    assert_eq!(value_class("5%"), "NONEMPTY");
    assert_eq!(value_class("1e3"), "NONEMPTY"); // exponent not in grammar
    assert_eq!(value_class("0x10"), "NONEMPTY");
    assert_eq!(value_class("."), "NONEMPTY");
    assert_eq!(value_class("3."), "NONEMPTY"); // trailing dot rejected
    assert_eq!(value_class(".5"), "NONEMPTY"); // leading dot rejected
    assert_eq!(value_class("--5"), "NONEMPTY");
    assert_eq!(value_class("hello"), "NONEMPTY");
    assert_eq!(value_class("١٢٣"), "NONEMPTY"); // non-ASCII digits
}

#[test]
fn zero_value_tree_byte_identical_to_structural() {
    // A textfield WITHOUT a value produces no V: section: byte-identical to
    // the pre-value-state descriptor.
    let mut tf = Node::new("textfield");
    tf.id = Some("email".into());
    assert_eq!(descriptor(None, &tf), "A:\n0:textfield@email");

    // A chrome node WITH a value is still not value-bearing: no V: section.
    let mut header = Node::new("header");
    header.id = Some("title".into());
    header.value = Some("Welcome".into());
    assert_eq!(descriptor(None, &header), "A:\n0:header@title");
}

#[test]
fn value_bearing_adds_v_section() {
    let mut tf = Node::new("textfield");
    tf.id = Some("email".into());
    tf.value = Some("a@b.com".into());
    assert_eq!(
        descriptor(None, &tf),
        "A:\n0:textfield@email\nV:key:email=NONEMPTY"
    );

    let mut counter = Node::new("status");
    counter.id = Some("count".into());
    counter.value = Some("5".into());
    // status is a value-role but not in ROLES, so the body is `node`.
    assert_eq!(
        descriptor(None, &counter),
        "A:\n0:node@count\nV:key:count=POS1"
    );
}

#[test]
fn v_section_sorted_by_key() {
    let mut screen = Node::new("screen");
    let mut z = Node::new("textfield");
    z.id = Some("zeta".into());
    z.value = Some("0".into());
    let mut a = Node::new("textfield");
    a.id = Some("alpha".into());
    a.value = Some("12".into());
    screen.children.push(z);
    screen.children.push(a);
    // Structural body keeps document order (zeta then alpha; distinct ids do
    // not collapse). The V: section is independently sorted by key.
    assert_eq!(
        descriptor(None, &screen),
        "A:\n0:screen;1:textfield@zeta;1:textfield@alpha\nV:key:alpha=POS2;key:zeta=ZERO"
    );
}

#[test]
fn keyless_value_node_uses_structural_index() {
    let mut screen = Node::new("screen");
    let mut a = Node::new("textfield");
    a.value = Some("3".into());
    let mut b = Node::new("textfield");
    b.value = Some("99".into());
    screen.children.push(a);
    screen.children.push(b);
    // The two keyless textfields are structurally identical, so the body
    // COLLAPSES them to one `*`-marked token (value is not structural). The
    // V: section still distinguishes them by structural index, so the two
    // value-classes survive in the descriptor.
    assert_eq!(
        descriptor(None, &screen),
        "A:\n0:screen;1:textfield*\nV:role:textfield#0=POS1;role:textfield#1=POS2"
    );
}

#[test]
fn opt_in_value_node_flag() {
    // A `text` role is chrome, so even with a value it is not value-bearing...
    let mut t = Node::new("text");
    t.id = Some("display".into());
    t.value = Some("42".into());
    assert_eq!(descriptor(None, &t), "A:\n0:text@display");
    // ...unless explicitly flagged via value_node (Layer 3 opt-in).
    t.value_node = true;
    assert_eq!(
        descriptor(None, &t),
        "A:\n0:text@display\nV:key:display=POS2"
    );
}

#[test]
fn two_pos1_values_same_signature() {
    let mk = |v: &str| {
        let mut n = Node::new("status");
        n.id = Some("count".into());
        n.value = Some(v.into());
        n
    };
    assert_eq!(signature(None, &mk("3")), signature(None, &mk("7")));
    // ...but ZERO and POS1 differ.
    assert_ne!(signature(None, &mk("0")), signature(None, &mk("3")));
}

#[test]
fn transient_value_node_excluded_from_v_section() {
    let mut screen = Node::new("screen");
    let mut spinner_box = Node::new("group");
    spinner_box.transient = true;
    let mut inner = Node::new("status");
    inner.id = Some("loading".into());
    inner.value = Some("50".into());
    spinner_box.children.push(inner);
    screen.children.push(spinner_box);
    // The transient subtree is dropped from both the body and the V: section,
    // so this is byte-identical to a bare screen.
    assert_eq!(descriptor(None, &screen), "A:\n0:screen");
}

// --- Value-state plumbing shared by the desktop runners (Layers 1/2/3) ---
// These pin the same behavior the deleted runners/test_signature.py locked in
// for the Python runners, now that the in-process Rust UIA/AT-SPI runners
// reuse these functions directly.

fn status(id: &str, value: &str) -> Node {
    let mut n = Node::new("status");
    n.id = Some(id.into());
    n.value = Some(value.into());
    n
}

#[test]
fn content_fingerprint_distinguishes_same_bucket_values() {
    // 5 and 6 are both POS1 -> identical canonical signature, but the Layer-1
    // content fingerprint (raw value) differs, so a counter tick is effective.
    let a = status("count", "5");
    let b = status("count", "6");
    assert_eq!(signature(None, &a), signature(None, &b));
    assert_ne!(content_fingerprint(None, &a), content_fingerprint(None, &b));
    // The fingerprint carries the raw value after the \x1f separator.
    assert_eq!(
        content_fingerprint(None, &a),
        format!("{}\u{1f}key:count=5", signature(None, &a))
    );
}

#[test]
fn apply_value_nodes_flags_selected_chrome_node() {
    // A chrome `text` carrying a value is not value-bearing until a Layer-3
    // selector opts it in; then it emits a V: section, exactly like the flag.
    let mut screen = Node::new("screen");
    let mut t = Node::new("text");
    t.id = Some("score".into());
    t.value = Some("7".into());
    screen.children.push(t);
    assert_eq!(descriptor(None, &screen), "A:\n0:screen;1:text@score");
    apply_value_nodes(&mut screen, &["key:score".to_string()]);
    assert_eq!(
        descriptor(None, &screen),
        "A:\n0:screen;1:text@score\nV:key:score=POS1"
    );
}

#[test]
fn value_cap_falls_back_to_structural_after_eight_variants() {
    // Two value fields; > 8 distinct (a,b) bucket combinations blow the cap
    // and the effective signature sticks to structural-only afterwards.
    let two = |va: &str, vb: &str| {
        let mut s = Node::new("screen");
        let mut a = Node::new("textfield");
        a.id = Some("a".into());
        a.value = Some(va.into());
        let mut b = Node::new("textfield");
        b.id = Some("b".into());
        b.value = Some(vb.into());
        s.children.push(a);
        s.children.push(b);
        s
    };
    let struct_only = {
        let mut s = Node::new("screen");
        let mut a = Node::new("textfield");
        a.id = Some("a".into());
        let mut b = Node::new("textfield");
        b.id = Some("b".into());
        s.children.push(a);
        s.children.push(b);
        signature(None, &s)
    };
    let buckets = ["", "0", "-3", "3", "10", "100", "1000", "abc"];
    let mut combos: Vec<(&str, &str)> = (0..8).map(|k| (buckets[k], buckets[0])).collect();
    combos.push((buckets[0], buckets[1])); // the 9th distinct combination
    let mut cap = ValueCap::new();
    let sigs: Vec<String> = combos
        .iter()
        .map(|(a, b)| cap.effective_signature(None, &two(a, b)))
        .collect();
    assert_ne!(sigs[0], struct_only, "under the cap keeps the value sig");
    assert_eq!(sigs[8], struct_only, "the 9th combo blows the cap");
    // Sticky: even an already-seen combo now returns structural-only.
    assert_eq!(
        cap.effective_signature(None, &two(buckets[0], buckets[0])),
        struct_only
    );
}
