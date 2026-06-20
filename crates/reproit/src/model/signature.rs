//! Canonical reference implementation of the structural screen signature.
//!
//! This module is THE parity oracle. `docs/signature.md` is the spec; this file
//! implements it exactly, and `signature_vectors.json` (at the repo root) holds
//! the golden vectors that every other implementation (the fuzz runners and the
//! production SDKs, in other languages) must reproduce bit-for-bit.
//!
//! ## Cross-language parity contract
//!
//! Another implementation proves parity by reading `signature_vectors.json` and,
//! for each entry, asserting `signature(anchor, tree) == expected_sig`. Each
//! entry is `{ description, anchor (string|null), tree, expected_sig }` where
//! `tree` is a `Node` serialized as JSON (see `Node` below for the field shape).
//! The Rust gate that does exactly this is `tests::golden_vectors_match` at the
//! bottom of this file; mirror its three lines in your language:
//!
//! 1. parse each vector's `tree` into your Node type,
//! 2. compute `signature(vector.anchor, &tree)`,
//! 3. assert it equals `vector.expected_sig`.
//!
//! If you also want to debug a mismatch, compare `descriptor(...)` (the exact
//! UTF-8 string that gets hashed) before comparing the hash.
#![allow(dead_code)]
// Reference/library module inside a binary crate: parts of the public API
// (Selector, selector, fnv1a32_hex via `signature`) are consumed by other-
// language implementations and the test gate, not yet by the binary's modes.

use serde::{Deserialize, Serialize};

/// The fixed, language-independent role vocabulary (see docs/signature.md
/// "Roles"). Anything outside this set normalizes to `node`.
const ROLES: &[&str] = &[
    "screen",
    "header",
    "text",
    "button",
    "link",
    "textfield",
    "image",
    "icon",
    "list",
    "listitem",
    "tab",
    "switch",
    "checkbox",
    "radio",
    "slider",
    "menu",
    "menuitem",
    "dialog",
    "group",
    "node",
];

/// Roles that flicker in and out of the tree and must be dropped before hashing
/// (docs/signature.md normalization rule 2). "transient error banner" is not a
/// distinct role in the vocabulary, so it is expressed via the `transient` flag
/// on a node (typically a `group`/`text`); both paths drop the node and its
/// subtree. `progress` is the role name for spinner/progress.
const TRANSIENT_ROLES: &[&str] = &[
    "toast", "snackbar", "spinner", "progress", "tooltip", "badge",
];

/// Value-role set (docs/signature.md "Value-state"). A node is value-bearing
/// only if it has a `value` AND its role is one of these (or it is explicitly
/// flagged via `value_node`). Chrome roles (button/label/header/text) are NEVER
/// value-bearing. Note that several of these roles (`status, log, progressbar,
/// meter, timer, output`) are NOT in the structural `ROLES` vocabulary, so they
/// normalize to `node` in the token body; the value-role check therefore uses the
/// RAW role, not the normalized one.
const VALUE_ROLES: &[&str] = &[
    "textfield",
    "status",
    "log",
    "progressbar",
    "meter",
    "timer",
    "output",
];

/// A normalized accessibility node: the input to the signature.
///
/// JSON shape (as stored in `signature_vectors.json`):
/// ```json
/// { "role": "button", "id": "submit", "type": "text",
///   "icon": "e5cd", "transient": false, "children": [ ... ] }
/// ```
/// All fields except `role` and `children` are optional. `type` is serialized as
/// `type` on the wire (the Rust field is `type_` to avoid the keyword). Note the
/// deliberate absence of any text/label/value field: localized text is excluded
/// from the descriptor by construction (rule 1), so there is nothing to hash.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Node {
    /// Role from the fixed vocabulary; unknown roles normalize to `node`.
    pub role: String,
    /// Stable developer identifier (key / test-id / a11y-id / resource-id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Optional input-type refinement (text, password, email, ...).
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub type_: Option<String>,
    /// Optional language-independent icon identity (codepoint / symbol / asset).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Explicit transient marker (e.g. a transient error banner). Dropped like a
    /// transient role. Defaults to false.
    #[serde(default, skip_serializing_if = "is_false")]
    pub transient: bool,
    /// The node's displayed data value (Layer 2, docs/signature.md
    /// "Value-state"). Only consulted when the node is value-bearing (a value-role
    /// or `value_node`-flagged). Chrome text never goes here. Defaults to None, so
    /// a tree with no values is byte-identical to a pre-value-state tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// Opt-in value-node flag (Layer 3). When true, the node is treated as
    /// value-bearing even if its role is not in `VALUE_ROLES` (a `reproit.yaml`
    /// `value_nodes:` selector resolves to this flag). Defaults to false.
    #[serde(default, skip_serializing_if = "is_false")]
    pub value_node: bool,
    /// Ordered children, in document order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Node>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl Node {
    /// Convenience constructor for a role-only node.
    pub fn new(role: &str) -> Node {
        Node {
            role: role.to_string(),
            id: None,
            type_: None,
            icon: None,
            transient: false,
            value: None,
            value_node: false,
            children: Vec::new(),
        }
    }
}

/// True if this node carries a canonical value-class in the V: section
/// (docs/signature.md "Value-state"): it has a `value` AND it is value-bearing,
/// i.e. its RAW role is a value-role OR it is `value_node`-flagged. The raw role
/// is used deliberately: roles like `status`/`meter` normalize to `node` but are
/// still value-roles.
fn is_value_bearing(node: &Node) -> bool {
    node.value.is_some() && (VALUE_ROLES.contains(&node.role.as_str()) || node.value_node)
}

/// Map a value string to a bounded, deterministic, locale-safe value-class token
/// (docs/signature.md "Value-state"). The numeric branch accepts ONLY the strict
/// period-decimal grammar `^[+-]?[0-9]+(\.[0-9]+)?$` with no grouping separators;
/// anything ambiguous (grouped/locale numbers, currency, text) falls back to
/// `NONEMPTY` because we do not guess locale formats.
pub fn value_class(s: &str) -> &'static str {
    let t = s.trim();
    if t.is_empty() {
        return "EMPTY";
    }
    if is_strict_decimal(t) {
        // Parse is safe: the grammar is a subset of f64's accepted syntax.
        let n: f64 = t.parse().unwrap_or(f64::NAN);
        let a = n.abs();
        if n == 0.0 {
            "ZERO"
        } else if n < 0.0 {
            "NEG"
        } else if a < 10.0 {
            "POS1"
        } else if a < 100.0 {
            "POS2"
        } else if a < 1000.0 {
            "POS3"
        } else {
            "POSL"
        }
    } else {
        "NONEMPTY"
    }
}

/// Strict `^[+-]?[0-9]+(\.[0-9]+)?$`: an optional sign, one or more ASCII digits,
/// optionally a period followed by one or more ASCII digits. No grouping
/// separators, no exponent, no leading/trailing dot. Locale-safe by construction.
fn is_strict_decimal(s: &str) -> bool {
    let bytes = s.as_bytes();
    let mut i = 0;
    if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
        i += 1;
    }
    let int_start = i;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i == int_start {
        return false; // need at least one integer digit
    }
    if i < bytes.len() && bytes[i] == b'.' {
        i += 1;
        let frac_start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i == frac_start {
            return false; // a trailing dot with no fraction digits is not allowed
        }
    }
    i == bytes.len()
}

/// The V:-section key for a value-bearing node: its stable `id` if present,
/// otherwise the structural fallback `role:<role>#<idx>` using the NORMALIZED role
/// (so the key namespace matches the selector grammar). This is the "stable-key"
/// the V: section sorts on.
fn value_key(node: &Node, structural_index: usize) -> String {
    if let Some(id) = &node.id {
        format!("key:{}", id)
    } else {
        format!("role:{}#{}", normalize_role(&node.role), structural_index)
    }
}

/// Collect `(value_key, value_class)` pairs for every value-bearing node in the
/// tree, in pre-order, skipping transient subtrees (rule 2) so the V: section is
/// consistent with the structural body. The structural index for a keyless node
/// is its position among same-(normalized-)role, non-transient siblings under the
/// same parent (matching `selector`'s `#idx`). The root has no peers, so it gets
/// index 0. The returned vector is later sorted by key for deterministic
/// serialization.
fn collect_values(node: &Node, out: &mut Vec<(String, &'static str)>) {
    if is_transient(node) {
        return;
    }
    if is_value_bearing(node) {
        out.push((
            value_key(node, 0),
            value_class(node.value.as_deref().unwrap_or("")),
        ));
    }
    collect_values_children(node, out);
}

/// Descend into a node's non-transient children, assigning each keyless child its
/// per-parent structural index among same-normalized-role peers, emitting any
/// value-bearing child, then recursing. The node itself is NOT re-emitted (the
/// caller already handled it).
fn collect_values_children(node: &Node, out: &mut Vec<(String, &'static str)>) {
    use std::collections::HashMap;
    let mut role_counts: HashMap<String, usize> = HashMap::new();
    for child in &node.children {
        if is_transient(child) {
            continue;
        }
        let role = normalize_role(&child.role).to_string();
        let idx = *role_counts.get(&role).unwrap_or(&0);
        role_counts.insert(role, idx + 1);
        if is_value_bearing(child) {
            out.push((
                value_key(child, idx),
                value_class(child.value.as_deref().unwrap_or("")),
            ));
        }
        collect_values_children(child, out);
    }
}

/// Build the V: section suffix (docs/signature.md "Value-state"). Returns an empty
/// string when there are NO value-bearing nodes, which keeps the descriptor (and
/// therefore the hash) byte-identical to a pre-value-state tree. Otherwise returns
/// `"\nV:" + key=class;key=class...` sorted by key.
fn value_section(root: &Node) -> String {
    let mut pairs: Vec<(String, &'static str)> = Vec::new();
    collect_values(root, &mut pairs);
    if pairs.is_empty() {
        return String::new();
    }
    pairs.sort_by(|a, b| a.0.cmp(&b.0));
    let body = pairs
        .iter()
        .map(|(k, c)| format!("{}={}", k, c))
        .collect::<Vec<_>>()
        .join(";");
    format!("\nV:{}", body)
}

/// Normalize a role to the fixed vocabulary: known roles pass through, unknown
/// roles map to `node` (docs/signature.md "Roles").
fn normalize_role(role: &str) -> &str {
    if ROLES.contains(&role) {
        role
    } else {
        "node"
    }
}

/// True if this node must be dropped during normalization (rule 2): a transient
/// role, or an explicit `transient` flag. The whole subtree goes with it.
fn is_transient(node: &Node) -> bool {
    node.transient || TRANSIENT_ROLES.contains(&node.role.as_str())
}

/// A normalized node after rules 1, 2, 4 are applied (transients removed,
/// children normalized in order). Rule 3 (collapse) is applied at serialization
/// time over the children of this tree.
struct NormNode {
    role: String,
    type_: Option<String>,
    icon: Option<String>,
    id: Option<String>,
    children: Vec<NormNode>,
}

/// Apply rules 1, 2, 4: exclude text (nothing to do, no text field), drop
/// transient subtrees, keep document order. Returns `None` if this node itself
/// is transient (caller drops it).
fn normalize(node: &Node) -> Option<NormNode> {
    if is_transient(node) {
        return None;
    }
    let children = node.children.iter().filter_map(normalize).collect();
    Some(NormNode {
        role: normalize_role(&node.role).to_string(),
        type_: node.type_.clone(),
        icon: node.icon.clone(),
        id: node.id.clone(),
        children,
    })
}

/// Serialize one node's token body (everything after `<depth>:`), without the
/// repeat marker: `<role>[:<type>][#<icon>][@<id>]`.
fn token_body(n: &NormNode) -> String {
    let mut s = String::new();
    s.push_str(&n.role);
    if let Some(t) = &n.type_ {
        s.push(':');
        s.push_str(t);
    }
    if let Some(ic) = &n.icon {
        s.push('#');
        s.push_str(ic);
    }
    if let Some(id) = &n.id {
        s.push('@');
        s.push_str(id);
    }
    s
}

/// The canonical subtree descriptor used for collapse comparison (rule 3): the
/// pre-order token list of this subtree, with depths normalized to start at 0 so
/// that two sibling subtrees at the same level compare equal regardless of their
/// absolute depth. This is the "child-descriptor" the spec collapses on.
fn subtree_key(n: &NormNode) -> String {
    let mut tokens = Vec::new();
    walk_key(n, 0, &mut tokens);
    tokens.join(";")
}

fn walk_key(n: &NormNode, depth: usize, tokens: &mut Vec<String>) {
    tokens.push(format!("{}:{}", depth, token_body(n)));
    for c in &n.children {
        walk_key(c, depth + 1, tokens);
    }
}

/// Pre-order serialization with rule 3 (collapse consecutive identical sibling
/// subtrees to one token carrying a `*` repeat marker, count dropped).
fn serialize(n: &NormNode, depth: usize, tokens: &mut Vec<String>) {
    serialize_node(n, depth, false, tokens);
}

/// Emit one node's token (optionally repeated) then recurse into children with
/// collapse applied across the children run.
fn serialize_node(n: &NormNode, depth: usize, repeated: bool, tokens: &mut Vec<String>) {
    let mut tok = format!("{}:{}", depth, token_body(n));
    if repeated {
        tok.push('*');
    }
    tokens.push(tok);
    serialize_children(&n.children, depth + 1, tokens);
}

/// Walk a run of siblings, collapsing maximal runs of >= 2 consecutive children
/// whose subtree_key is identical into a single emission with the `*` marker.
fn serialize_children(children: &[NormNode], depth: usize, tokens: &mut Vec<String>) {
    let mut i = 0;
    while i < children.len() {
        let key = subtree_key(&children[i]);
        let mut j = i + 1;
        while j < children.len() && subtree_key(&children[j]) == key {
            j += 1;
        }
        let run = j - i;
        // >= 2 identical consecutive siblings collapse to one `*`-marked token.
        serialize_node(&children[i], depth, run >= 2, tokens);
        i = j;
    }
}

/// Build the exact UTF-8 descriptor string that gets hashed (docs/signature.md
/// "Descriptor serialization"): `"A:" + anchor + "\n" + tokens.join(";")`. The
/// `A:` prefix line is always present, even when there is no anchor (then it is
/// the empty string `A:` followed by newline).
pub fn descriptor(anchor: Option<&str>, root: &Node) -> String {
    let mut tokens = Vec::new();
    if let Some(norm) = normalize(root) {
        serialize(&norm, 0, &mut tokens);
    }
    // If the root itself is transient the token list is empty; the descriptor is
    // still just the anchor line plus an empty body (deterministic).
    //
    // The V: section (Layer 2 value-classes) is appended only when at least one
    // value-bearing node exists; otherwise `value_section` returns "" and the
    // descriptor is byte-identical to a pre-value-state tree (backward-compatible).
    format!(
        "A:{}\n{}{}",
        anchor.unwrap_or(""),
        tokens.join(";"),
        value_section(root)
    )
}

/// FNV-1a, 32-bit, over the UTF-8 bytes of `descriptor`. 8-char zero-padded
/// lowercase hex (docs/signature.md "Hash").
pub fn signature(anchor: Option<&str>, root: &Node) -> String {
    let desc = descriptor(anchor, root);
    fnv1a32_hex(desc.as_bytes())
}

/// FNV-1a 32-bit -> 8-char zero-padded lowercase hex.
fn fnv1a32_hex(bytes: &[u8]) -> String {
    let mut h: u32 = 0x811c_9dc5;
    for &b in bytes {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    format!("{:08x}", h)
}

/// A selector that addresses an element for actions / repros, per
/// docs/signature.md "Selectors": `id  >  type+role  >  role + structural-index`.
///
/// Returns `key:<id>` if the node has a stable id; otherwise the structural
/// `role:<role>#<idx>` form, where `idx` is the structural index supplied by the
/// caller (the element's position among same-role siblings/peers in the emitted
/// elements list). `type+role` is the intermediate tier: it is folded into the
/// returned text only as documentation here, because the addressable string is
/// still `role:<role>#<idx>` when no id exists; the `type` discriminates the hash
/// but does not change the selector grammar.
///
/// `nokey` is true whenever no stable id was available; it is metadata for
/// `map show` (warn the developer to add an id) and does NOT affect the hash.
pub struct Selector {
    pub selector: String,
    pub nokey: bool,
}

/// Build a selector for a node given its structural index among peers.
pub fn selector(node: &Node, structural_index: usize) -> Selector {
    if let Some(id) = &node.id {
        Selector {
            selector: format!("key:{}", id),
            nokey: false,
        }
    } else {
        let role = normalize_role(&node.role);
        Selector {
            selector: format!("role:{}#{}", role, structural_index),
            nokey: true,
        }
    }
}

#[cfg(test)]
mod tests {
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
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {}", path.display(), e));
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
        // a chrome label with text (no value) is backward-compatible: identical to
        // the same structure with no value field (the empty-anchor structural form
        // is exercised here by comparing to a hand-built structural sig).
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
}
