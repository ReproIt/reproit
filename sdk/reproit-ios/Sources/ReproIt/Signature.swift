// Canonical structural screen signature for iOS (Foundation-only).
//
// This is the Swift port of the Rust parity oracle
// (`crates/reproit/src/model/signature.rs`). `docs/signature.md` is the spec;
// `signature_vectors.json` (repo root) holds the golden vectors every
// implementation must reproduce bit-for-bit. The production SDK (the UIKit
// capture in Capture.swift) and the host parity test compute the signature
// through THIS file, so they agree by construction.
//
// The descriptor string that gets hashed is built exactly as the spec defines:
//
//   token  = `<depth>:<role>[:<type>][#<icon>][@<id>]` (with a trailing `*`
//            on a collapsed repeat)
//   body   = tokens joined by `;`, pre-order
//   desc   = `"A:" + anchor + "\n" + body`
//   sig    = FNV-1a 32-bit over the UTF-8 bytes of desc, 8-char lowercase hex

import Foundation

/// The fixed, language-independent role vocabulary (docs/signature.md "Roles").
/// Anything outside this set normalizes to `node`.
public let kReproItRoles: Set<String> = [
    "screen", "header", "text", "button", "link", "textfield", "image", "icon",
    "list", "listitem", "tab", "switch", "checkbox", "radio", "slider", "menu",
    "menuitem", "dialog", "group", "node",
]

/// Roles that flicker in and out of the tree and must be dropped before hashing
/// (docs/signature.md normalization rule 2). "transient error banner" is not a
/// distinct role in the vocabulary, so it is expressed via the
/// ``ReproItNode/transient`` flag; both paths drop the node and its whole
/// subtree. `progress` is the role name for spinner/progress.
public let kReproItTransientRoles: Set<String> = [
    "toast", "snackbar", "spinner", "progress", "tooltip", "badge",
]

/// Value-role set (docs/signature.md "Value-state", Layer 2). A node carries a
/// canonical value-class in the `V:` section only if it has a
/// ``ReproItNode/value`` AND either its RAW role is in this set OR it is flagged
/// ``ReproItNode/valueNode`` (the Layer 3 opt-in). Several of these (`status,
/// log, progressbar, meter, timer, output`) are NOT in the structural
/// ``kReproItRoles`` vocabulary, so they normalize to `node` in the descriptor
/// body; the value-role test therefore uses the RAW role, not the normalized
/// one. Chrome roles (button/label/header/text/...) are NEVER value-bearing, so
/// rule 1's chrome-text exclusion is preserved exactly.
public let kReproItValueRoles: Set<String> = [
    "textfield", "status", "log", "progressbar", "meter", "timer", "output",
]

/// A normalized accessibility node: the input to the signature.
///
/// Mirrors the Rust `Node` JSON shape so each golden vector's `tree` parses
/// directly via ``ReproItNode/fromJSON(_:)``:
/// ```json
/// { "role": "button", "id": "submit", "type": "text",
///   "icon": "e5cd", "transient": false, "children": [ ... ] }
/// ```
/// All fields except `role`/`children` are optional. There is deliberately no
/// text/label/value field: localized text is excluded from the descriptor by
/// construction (rule 1), so there is nothing to hash.
public struct ReproItNode {
    /// Role from the fixed vocabulary; unknown roles normalize to `node`.
    public var role: String
    /// Stable developer identifier (key / test-id / a11y-id / resource-id).
    public var id: String?
    /// Optional input-type refinement (text, password, email, ...).
    public var type: String?
    /// Optional language-independent icon identity (codepoint / symbol / asset).
    public var icon: String?
    /// Explicit transient marker (e.g. a transient error banner). Dropped like a
    /// transient role.
    public var transient: Bool
    /// The node's displayed data value (Layer 2, docs/signature.md
    /// "Value-state"). Only consulted when the node is value-bearing (a
    /// value-role or a ``valueNode``-flagged node). Chrome text never goes here.
    /// Nil by default, so a tree with no values is byte-identical to a
    /// pre-value-state tree.
    public var value: String?
    /// Opt-in value-node flag (Layer 3). When true the node is treated as
    /// value-bearing even if its role is not in ``kReproItValueRoles`` (a
    /// `reproit.yaml` `value_nodes:` selector resolves to this flag). False by
    /// default.
    public var valueNode: Bool
    /// Ordered children, in document order.
    public var children: [ReproItNode]

    public init(
        role: String,
        id: String? = nil,
        type: String? = nil,
        icon: String? = nil,
        transient: Bool = false,
        value: String? = nil,
        valueNode: Bool = false,
        children: [ReproItNode] = []
    ) {
        self.role = role
        self.id = id
        self.type = type
        self.icon = icon
        self.transient = transient
        self.value = value
        self.valueNode = valueNode
        self.children = children
    }

    /// Parse the JSON shape stored in `signature_vectors.json`. This is the form
    /// the parity gate feeds in; it must accept exactly the fields the Rust
    /// `Node` serializes.
    public static func fromJSON(_ j: [String: Any]) -> ReproItNode {
        let kids = (j["children"] as? [[String: Any]])?.map { fromJSON($0) } ?? []
        return ReproItNode(
            role: (j["role"] as? String) ?? "node",
            id: j["id"] as? String,
            type: j["type"] as? String,
            icon: j["icon"] as? String,
            transient: (j["transient"] as? Bool) ?? false,
            value: j["value"] as? String,
            valueNode: (j["value_node"] as? Bool) ?? false,
            children: kids
        )
    }
}

/// Normalize a role to the fixed vocabulary: known roles pass through, unknown
/// roles map to `node` (docs/signature.md "Roles").
public func reproitNormalizeRole(_ role: String) -> String {
    kReproItRoles.contains(role) ? role : "node"
}

private func reproitIsTransient(_ n: ReproItNode) -> Bool {
    n.transient || kReproItTransientRoles.contains(n.role)
}

/// A normalized node after rules 1, 2, 4 are applied (transients removed,
/// children normalized in order). Rule 3 (collapse) is applied at serialization.
private final class NormNode {
    let role: String
    let type: String?
    let icon: String?
    let id: String?
    let children: [NormNode]
    init(_ role: String, _ type: String?, _ icon: String?, _ id: String?, _ children: [NormNode]) {
        self.role = role
        self.type = type
        self.icon = icon
        self.id = id
        self.children = children
    }
}

/// Apply rules 1, 2, 4: exclude text (no text field exists), drop transient
/// subtrees, keep document order. Returns nil if this node itself is transient.
private func reproitNormalize(_ node: ReproItNode) -> NormNode? {
    if reproitIsTransient(node) { return nil }
    let children = node.children.compactMap { reproitNormalize($0) }
    return NormNode(
        reproitNormalizeRole(node.role),
        node.type,
        node.icon,
        node.id,
        children
    )
}

/// One node's token body (everything after `<depth>:`), without the repeat
/// marker: `<role>[:<type>][#<icon>][@<id>]`.
private func reproitTokenBody(_ n: NormNode) -> String {
    var s = n.role
    if let t = n.type { s += ":" + t }
    if let ic = n.icon { s += "#" + ic }
    if let id = n.id { s += "@" + id }
    return s
}

/// The canonical subtree descriptor used for collapse comparison (rule 3): the
/// pre-order token list of this subtree, depths re-based to 0, so two sibling
/// subtrees at the same level compare equal regardless of absolute depth.
private func reproitSubtreeKey(_ n: NormNode) -> String {
    var tokens: [String] = []
    reproitWalkKey(n, 0, &tokens)
    return tokens.joined(separator: ";")
}

private func reproitWalkKey(_ n: NormNode, _ depth: Int, _ tokens: inout [String]) {
    tokens.append("\(depth):\(reproitTokenBody(n))")
    for c in n.children { reproitWalkKey(c, depth + 1, &tokens) }
}

/// Emit one node's token (optionally marked repeated) then recurse, collapsing
/// across the children run.
private func reproitSerializeNode(_ n: NormNode, _ depth: Int, _ repeated: Bool, _ tokens: inout [String]) {
    var tok = "\(depth):\(reproitTokenBody(n))"
    if repeated { tok += "*" }
    tokens.append(tok)
    reproitSerializeChildren(n.children, depth + 1, &tokens)
}

/// Walk a run of siblings, collapsing maximal runs of >= 2 consecutive children
/// whose subtreeKey is identical into a single emission with the `*` marker.
private func reproitSerializeChildren(_ children: [NormNode], _ depth: Int, _ tokens: inout [String]) {
    var i = 0
    while i < children.count {
        let key = reproitSubtreeKey(children[i])
        var j = i + 1
        while j < children.count && reproitSubtreeKey(children[j]) == key { j += 1 }
        let run = j - i
        reproitSerializeNode(children[i], depth, run >= 2, &tokens)
        i = j
    }
}

// MARK: Layer 2 - bounded, locale-safe value-classes (docs/signature.md
// "Value-state").

/// True if `n` carries a canonical value-class in the `V:` section: it has a
/// ``ReproItNode/value`` AND it is value-bearing, i.e. its RAW role is a
/// value-role OR it is ``ReproItNode/valueNode``-flagged. The raw role is used
/// deliberately (roles like `status`/`meter` normalize to `node` but are still
/// value-roles).
private func reproitIsValueBearing(_ n: ReproItNode) -> Bool {
    n.value != nil && (kReproItValueRoles.contains(n.role) || n.valueNode)
}

/// Strict `^[+-]?[0-9]+(\.[0-9]+)?$`: an optional sign, one or more ASCII
/// digits, optionally a period followed by one or more ASCII digits. No grouping
/// separators, no exponent, no leading/trailing dot. Locale-safe by construction.
private func reproitIsStrictDecimal(_ s: String) -> Bool {
    let u = Array(s.utf8)
    var i = 0
    if i < u.count && (u[i] == 0x2b || u[i] == 0x2d) { i += 1 } // + or -
    let intStart = i
    while i < u.count && u[i] >= 0x30 && u[i] <= 0x39 { i += 1 }
    if i == intStart { return false } // need at least one integer digit
    if i < u.count && u[i] == 0x2e { // '.'
        i += 1
        let fracStart = i
        while i < u.count && u[i] >= 0x30 && u[i] <= 0x39 { i += 1 }
        if i == fracStart { return false } // trailing dot with no fraction
    }
    return i == u.count
}

/// Map a value string to a bounded, deterministic, locale-safe value-class token
/// (docs/signature.md "Value-state"). EMPTY / ZERO / NEG / POS1<10 / POS2<100 /
/// POS3<1000 / POSL>=1000 for the strict period-decimal grammar; NONEMPTY for
/// anything ambiguous (grouped/locale numbers, currency, text) because we do not
/// guess locale formats.
public func reproitValueClass(_ s: String) -> String {
    let t = s.trimmingCharacters(in: .whitespacesAndNewlines)
    if t.isEmpty { return "EMPTY" }
    if reproitIsStrictDecimal(t) {
        let n = Double(t) ?? Double.nan
        let a = abs(n)
        if n == 0.0 { return "ZERO" }
        if n < 0.0 { return "NEG" }
        if a < 10.0 { return "POS1" }
        if a < 100.0 { return "POS2" }
        if a < 1000.0 { return "POS3" }
        return "POSL"
    }
    return "NONEMPTY"
}

/// The `V:`-section key for a value-bearing node: its stable `id` as `key:<id>`
/// if present, otherwise the structural fallback `role:<role>#<idx>` using the
/// NORMALIZED role (so the key namespace matches the selector grammar). This is
/// the "stable-key" the `V:` section sorts on.
private func reproitValueKey(_ n: ReproItNode, _ structuralIndex: Int) -> String {
    if let id = n.id { return "key:\(id)" }
    return "role:\(reproitNormalizeRole(n.role))#\(structuralIndex)"
}

/// Collect `(value_key, value_class)` pairs for every value-bearing node in the
/// tree, in pre-order, skipping transient subtrees (rule 2) so the `V:` section
/// stays consistent with the structural body. The structural index for a keyless
/// node is its position among same-(normalized-)role, non-transient siblings
/// under the same parent. The root has no peers, so it gets index 0.
private func reproitCollectValueChildren(_ node: ReproItNode, _ out: inout [(String, String)]) {
    var roleCounts: [String: Int] = [:]
    for c in node.children {
        if reproitIsTransient(c) { continue }
        let role = reproitNormalizeRole(c.role)
        let idx = roleCounts[role] ?? 0
        roleCounts[role] = idx + 1
        if reproitIsValueBearing(c) {
            out.append((reproitValueKey(c, idx), reproitValueClass(c.value ?? "")))
        }
        reproitCollectValueChildren(c, &out)
    }
}

private func reproitValuePairs(_ root: ReproItNode) -> [(String, String)] {
    var out: [(String, String)] = []
    if reproitIsTransient(root) { return out }
    if reproitIsValueBearing(root) {
        out.append((reproitValueKey(root, 0), reproitValueClass(root.value ?? "")))
    }
    reproitCollectValueChildren(root, &out)
    out.sort { $0.0 < $1.0 }
    return out
}

/// The `V:` section suffix (docs/signature.md "Value-state"). Empty string when
/// there are NO value-bearing pairs, which keeps the descriptor (and hash)
/// byte-identical to a pre-value-state tree. `excludeKeys` lets a RUNNER enforce
/// the per-node cap (Layer 2 "Hard cap") by dropping keys that exceeded their
/// distinct-value-class budget; the SDK passes none.
private func reproitValueSection(_ pairs: [(String, String)], _ excludeKeys: Set<String>?) -> String {
    let kept: [(String, String)]
    if let ex = excludeKeys, !ex.isEmpty {
        kept = pairs.filter { !ex.contains($0.0) }
    } else {
        kept = pairs
    }
    if kept.isEmpty { return "" }
    return "\nV:" + kept.map { "\($0.0)=\($0.1)" }.joined(separator: ";")
}

/// Build the exact UTF-8 descriptor string that gets hashed (docs/signature.md
/// "Descriptor serialization"): `"A:" + anchor + "\n" + tokens.join(";")`, with
/// the Layer 2 `V:` section appended only when at least one value-bearing node
/// exists. `excludeKeys` drops capped value-keys from the `V:` section (runner
/// cap; the SDK leaves it empty). The `A:` prefix line is always present.
public func reproitDescriptorFrom(_ anchor: String?, _ root: ReproItNode, _ excludeKeys: Set<String>?) -> String {
    var tokens: [String] = []
    if let norm = reproitNormalize(root) {
        reproitSerializeNode(norm, 0, false, &tokens)
    }
    let v = reproitValueSection(reproitValuePairs(root), excludeKeys)
    return "A:\(anchor ?? "")\n\(tokens.joined(separator: ";"))\(v)"
}

/// Build the exact UTF-8 descriptor string that gets hashed, with the full
/// (uncapped) `V:` section. The `A:` prefix line is always present, even with no
/// anchor. A tree with no value-bearing nodes is byte-identical to a
/// pre-value-state tree (backward-compatible).
public func reproitDescriptor(_ anchor: String?, _ root: ReproItNode) -> String {
    reproitDescriptorFrom(anchor, root, nil)
}

/// The canonical structural screen signature, byte-identical to the Rust oracle,
/// the Flutter SDK, and the web SDK.
public enum ReproItSignature {
    /// FNV-1a 32-bit over the UTF-8 bytes of the descriptor; 8-char zero-padded
    /// lowercase hex. This is THE signature.
    public static func of(anchor: String?, tree: ReproItNode) -> String {
        return fnv1a32Hex(Array(reproitDescriptor(anchor, tree).utf8))
    }

    /// The canonical signature with capped value-keys excluded from the `V:`
    /// section (runner cap, docs/signature.md "Value-state" Hard cap). With
    /// `excludeKeys` empty/nil this is identical to ``of(anchor:tree:)``.
    public static func from(anchor: String?, tree: ReproItNode, excludeKeys: Set<String>?) -> String {
        return fnv1a32Hex(Array(reproitDescriptorFrom(anchor, tree, excludeKeys).utf8))
    }

    /// FNV-1a, 32-bit, over `bytes`; 8-char zero-padded lowercase hex
    /// (docs/signature.md "Hash"). Exposed for tests / debugging.
    public static func fnv1a32Hex(_ bytes: [UInt8]) -> String {
        var h: UInt32 = 0x811c_9dc5
        for b in bytes {
            h ^= UInt32(b)
            h = h &* 0x0100_0193
        }
        return String(format: "%08x", h)
    }
}

/// A selector that addresses an element for actions / repros (docs/signature.md
/// "Selectors"): `key:<id>` when a stable id exists, else `role:<role>#<idx>`.
/// `nokey` is true when no id was available (metadata for `map --show`; it does
/// NOT affect the hash).
public struct ReproItSelector {
    public let selector: String
    public let nokey: Bool
    public init(selector: String, nokey: Bool) {
        self.selector = selector
        self.nokey = nokey
    }
}

/// Build a selector for a node given its structural index among same-role peers.
public func reproitSelector(id: String?, role: String, structuralIndex: Int) -> ReproItSelector {
    if let id = id { return ReproItSelector(selector: "key:\(id)", nokey: false) }
    return ReproItSelector(selector: "role:\(reproitNormalizeRole(role))#\(structuralIndex)", nokey: true)
}
