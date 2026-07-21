//! Canonical reference implementation of the structural screen signature.
//!
//! This module is THE parity oracle. `docs/signature.md` is the spec; this file
//! implements it exactly, and `signature_vectors.json` (at the repo root) holds
//! the golden vectors that every other implementation (the fuzz runners and the
//! production SDKs, in other languages) must reproduce bit-for-bit.
//!
//! ## Cross-language parity contract
//!
//! Another implementation proves parity by reading `signature_vectors.json`
//! and, for each entry, asserting `signature(anchor, tree) == expected_sig`.
//! Each entry is `{ description, anchor (string|null), tree, expected_sig }`
//! where `tree` is a `Node` serialized as JSON (see `Node` below for the field
//! shape). The Rust gate that does exactly this is
//! `tests::golden_vectors_match` at the bottom of this file; mirror its three
//! lines in your language:
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
/// normalize to `node` in the token body; the value-role check therefore uses
/// the RAW role, not the normalized one.
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
/// All fields except `role` and `children` are optional. `type` is serialized
/// as `type` on the wire (the Rust field is `type_` to avoid the keyword). Note
/// the deliberate absence of any text/label/value field: localized text is
/// excluded from the descriptor by construction (rule 1), so there is nothing
/// to hash.
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
    /// Optional language-independent icon identity (codepoint / symbol /
    /// asset).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Explicit transient marker (e.g. a transient error banner). Dropped like
    /// a transient role. Defaults to false.
    #[serde(default, skip_serializing_if = "is_false")]
    pub transient: bool,
    /// The node's displayed data value (Layer 2, docs/signature.md
    /// "Value-state"). Only consulted when the node is value-bearing (a
    /// value-role or `value_node`-flagged). Chrome text never goes here.
    /// Defaults to None, so a tree with no values is byte-identical to a
    /// pre-value-state tree.
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
/// i.e. its RAW role is a value-role OR it is `value_node`-flagged. The raw
/// role is used deliberately: roles like `status`/`meter` normalize to `node`
/// but are still value-roles.
fn is_value_bearing(node: &Node) -> bool {
    node.value.is_some() && (VALUE_ROLES.contains(&node.role.as_str()) || node.value_node)
}

/// True if `role` is one of the value-roles (docs/signature.md "Value-state"):
/// the RAW roles whose displayed value folds into the signature. The desktop
/// runners use this to decide whether to READ a control's value at all,
/// matching the former Python runners' `if role not in VALUE_ROLES` guard.
pub fn is_value_role(role: &str) -> bool {
    VALUE_ROLES.contains(&role)
}

/// Map a value string to a bounded, deterministic, locale-safe value-class
/// token (docs/signature.md "Value-state"). The numeric branch accepts ONLY the
/// strict period-decimal grammar `^[+-]?[0-9]+(\.[0-9]+)?$` with no grouping
/// separators; anything ambiguous (grouped/locale numbers, currency, text)
/// falls back to `NONEMPTY` because we do not guess locale formats.
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

/// Strict `^[+-]?[0-9]+(\.[0-9]+)?$`: an optional sign, one or more ASCII
/// digits, optionally a period followed by one or more ASCII digits. No
/// grouping separators, no exponent, no leading/trailing dot. Locale-safe by
/// construction.
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
            return false; // a trailing dot with no fraction digits is not
                          // allowed
        }
    }
    i == bytes.len()
}

/// The V:-section key for a value-bearing node: its stable `id` if present,
/// otherwise the structural fallback `role:<role>#<idx>` using the NORMALIZED
/// role (so the key namespace matches the selector grammar). This is the
/// "stable-key" the V: section sorts on.
fn value_key(node: &Node, structural_index: usize) -> String {
    if let Some(id) = &node.id {
        format!("key:{}", id)
    } else {
        format!("role:{}#{}", normalize_role(&node.role), structural_index)
    }
}

/// Collect `(value_key, value_class)` pairs for every value-bearing node in the
/// tree, in pre-order, skipping transient subtrees (rule 2) so the V: section
/// is consistent with the structural body. The structural index for a keyless
/// node is its position among same-(normalized-)role, non-transient siblings
/// under the same parent (matching `selector`'s `#idx`). The root has no peers,
/// so it gets index 0. The returned vector is later sorted by key for
/// deterministic serialization.
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

/// Descend into a node's non-transient children, assigning each keyless child
/// its per-parent structural index among same-normalized-role peers, emitting
/// any value-bearing child, then recursing. The node itself is NOT re-emitted
/// (the caller already handled it).
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

/// Build the V: section suffix (docs/signature.md "Value-state"). Returns an
/// empty string when there are NO value-bearing nodes, which keeps the
/// descriptor (and therefore the hash) byte-identical to a pre-value-state
/// tree. Otherwise returns `"\nV:" + key=class;key=class...` sorted by key.
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
/// pre-order token list of this subtree, with depths normalized to start at 0
/// so that two sibling subtrees at the same level compare equal regardless of
/// their absolute depth. This is the "child-descriptor" the spec collapses on.
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
    // descriptor stays purely structural.
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
/// docs/signature.md "Selectors": `id  >  type+role  >  role +
/// structural-index`.
///
/// Returns `key:<id>` if the node has a stable id; otherwise the structural
/// `role:<role>#<idx>` form, where `idx` is the structural index supplied by
/// the caller (the element's position among same-role siblings/peers in the
/// emitted elements list). `type+role` is the intermediate tier: it is folded
/// into the returned text only as documentation here, because the addressable
/// string is still `role:<role>#<idx>` when no id exists; the `type`
/// discriminates the hash but does not change the selector grammar.
///
/// `nokey` is true whenever no stable id was available; it is metadata for
/// `debug map show` (warn the developer to add an id) and does NOT affect the
/// hash.
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

// =============================================================================
// Value-state plumbing shared by the desktop runners (Layers 1/2/3). These are
// pure functions of a Node tree, so they live beside the canonical signature
// and the in-process Windows (UIA) and Linux (AT-SPI) runners reuse them
// directly instead of re-porting them. They were previously the Python runners'
// own re-port; the deleted runners/test_signature.py locked their behavior, now
// pinned by the tests below.
// =============================================================================

/// The selector string for a node (matches the V: key namespace): `key:<id>` if
/// present, else `role:<role>#<idx>` over the NORMALIZED role. The structural
/// index is the node's position among same-normalized-role, non-transient peers
/// under its parent.
pub fn node_selector(node: &Node, structural_index: usize) -> String {
    if let Some(id) = &node.id {
        format!("key:{}", id)
    } else {
        format!("role:{}#{}", normalize_role(&node.role), structural_index)
    }
}

/// Layer-1 effect-detection fingerprint (docs/signature.md "Value-state").
///
/// The canonical structural signature, then `\x1f`, then the sorted
/// `stable-key=trimmed-RAW-value` pairs over every value-bearing node. Unlike
/// the canonical signature this carries the raw localized value, so it is
/// EPHEMERAL: a per-step liveness check only and MUST NOT enter the canonical
/// graph key. An action is effective iff the structural signature OR this
/// fingerprint changed (so a counter 5->6, both POS1, is still effective).
/// Transient subtrees are skipped, consistent with the signature body.
pub fn content_fingerprint(anchor: Option<&str>, root: &Node) -> String {
    let mut pairs: Vec<(String, String)> = Vec::new();
    content_pairs_root(root, &mut pairs);
    pairs.sort_by(|a, b| a.0.cmp(&b.0));
    let body = pairs
        .iter()
        .map(|(k, v)| format!("{}={}", k, v))
        .collect::<Vec<_>>()
        .join(";");
    format!("{}\u{1f}{}", signature(anchor, root), body)
}

fn content_pairs_root(node: &Node, out: &mut Vec<(String, String)>) {
    if is_transient(node) {
        return;
    }
    if is_value_bearing(node) {
        out.push((
            node_selector(node, 0),
            node.value.as_deref().unwrap_or("").trim().to_string(),
        ));
    }
    content_pairs_children(node, out);
}

fn content_pairs_children(node: &Node, out: &mut Vec<(String, String)>) {
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
                node_selector(child, idx),
                child.value.as_deref().unwrap_or("").trim().to_string(),
            ));
        }
        content_pairs_children(child, out);
    }
}

/// Layer-3 opt-in: set `value_node` on every node whose selector is in
/// `selectors` (a `reproit.yaml` `value_nodes:` list), so the oracle then
/// treats it as value-bearing through the same path as a value-role node. No-op
/// when `selectors` is empty (the value-less tree is unchanged).
pub fn apply_value_nodes(root: &mut Node, selectors: &[String]) {
    if selectors.is_empty() {
        return;
    }
    let set: std::collections::HashSet<&str> = selectors.iter().map(|s| s.as_str()).collect();
    apply_value_root(root, &set);
}

fn apply_value_root(node: &mut Node, set: &std::collections::HashSet<&str>) {
    if is_transient(node) {
        return;
    }
    if set.contains(node_selector(node, 0).as_str()) {
        node.value_node = true;
    }
    apply_value_children(node, set);
}

fn apply_value_children(node: &mut Node, set: &std::collections::HashSet<&str>) {
    use std::collections::HashMap;
    let mut role_counts: HashMap<String, usize> = HashMap::new();
    for child in &mut node.children {
        if is_transient(child) {
            continue;
        }
        let role = normalize_role(&child.role).to_string();
        let idx = *role_counts.get(&role).unwrap_or(&0);
        role_counts.insert(role, idx + 1);
        if set.contains(node_selector(child, idx).as_str()) {
            child.value_node = true;
        }
        apply_value_children(child, set);
    }
}

/// A shallow copy of the tree with every `value`/`value_node` cleared, so its
/// signature is the pure structural sig (no V: section). Used as the `ValueCap`
/// key and as the fallback signature once a node blows the cap.
pub fn structural_only(root: &Node) -> Node {
    Node {
        role: root.role.clone(),
        id: root.id.clone(),
        type_: root.type_.clone(),
        icon: root.icon.clone(),
        transient: root.transient,
        value: None,
        value_node: false,
        children: root.children.iter().map(structural_only).collect(),
    }
}

/// Layer-2 runner bound (docs/signature.md "Value-state"): at most 8 DISTINCT
/// value-class combinations per structural node. The oracle is stateless and
/// always computes the per-state value-class; this observes variants over time
/// and, once a structural node has shown more than 8 distinct value states,
/// drops it from the V: section (falls back to structural-only) so an
/// adversarial value generator cannot explode the graph. Keying is by the
/// structural (value-less) signature; once a sig blows the cap it stays capped
/// (sticky).
#[derive(Default)]
pub struct ValueCap {
    variants: std::collections::HashMap<String, std::collections::HashSet<String>>,
    capped: std::collections::HashSet<String>,
}

impl ValueCap {
    pub const CAP: usize = 8;

    pub fn new() -> Self {
        ValueCap::default()
    }

    /// The signature to use for this state: the full (value-bearing) signature
    /// until this structural node has shown more than `CAP` distinct value
    /// combinations, then the structural-only signature (sticky).
    pub fn effective_signature(&mut self, anchor: Option<&str>, root: &Node) -> String {
        let struct_sig = signature(anchor, &structural_only(root));
        if self.capped.contains(&struct_sig) {
            return struct_sig;
        }
        let full_sig = signature(anchor, root);
        let seen = self.variants.entry(struct_sig.clone()).or_default();
        seen.insert(full_sig.clone());
        if seen.len() > Self::CAP {
            self.capped.insert(struct_sig.clone());
            return struct_sig;
        }
        full_sig
    }
}

#[cfg(test)]
mod tests;
