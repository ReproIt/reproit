// ReproIt macOS desktop runner (AXUIElement backend).
//
// Drives ANY native macOS app (AppKit, SwiftUI, and Qt / GTK / wxWidgets /
// Avalonia builds, which all publish to the same accessibility API) through
// the system AX tree, and prints the framework-agnostic marker protocol that
// `reproit` parses. Same contract as web-runner/runner.mjs and explorer.dart:
//
//   JOURNEY claimed role=a            ready
//   EXPLORE:STATE {"sig","labels"}    new state
//   EXPLORE:EDGE  {"from","action","to"}
//   FUZZ:ACT tap:<label> | back       chosen action
//   JOURNEY DONE                      finished
//   EXCEPTION CAUGHT BY ... ╡..╞      crash / lost target (the oracle)
//
// Target via REPROIT_TARGET (bundle id e.g. com.apple.calculator, or app name).
// Fuzz config via REPROIT_FUZZ_CONFIG (host json path): {seed,budget,replay,
// prefix,edgeWeights} exactly like the Dart explorer, so seeds replay.
//
// Run:  swift runners/macos-ax.swift   (needs Accessibility permission)

import Cocoa
import ApplicationServices
import Foundation

let actionBudgetDefault = 36
let maxLabelLen = 40
let maxLabelsPerState = 24

func emit(_ s: String) { print(s); fflush(stdout) }

// ---- fuzz config (mirrors explorer.dart) --------------------------------
struct FuzzCfg {
    var seed: UInt32 = 0
    var budget: Int = actionBudgetDefault
    var replay: [String]?
    var prefix: [String]?
    var edgeWeights: [String: [String: Int]] = [:]
}

func loadFuzz() -> FuzzCfg {
    var c = FuzzCfg()
    guard let p = ProcessInfo.processInfo.environment["REPROIT_FUZZ_CONFIG"], !p.isEmpty,
          let data = FileManager.default.contents(atPath: p),
          let j = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
    else { return c }
    if let s = j["seed"] as? NSNumber { c.seed = UInt32(truncatingIfNeeded: s.intValue) }
    if let b = j["budget"] as? NSNumber { c.budget = b.intValue }
    c.replay = j["replay"] as? [String]
    c.prefix = j["prefix"] as? [String]
    if let ew = j["edgeWeights"] as? [String: [String: Int]] { c.edgeWeights = ew }
    return c
}

// ---- Layer 3 opt-in: value_nodes from reproit.yaml ----------------------
// Read the `value_nodes:` selector list from reproit.yaml (docs/signature.md
// "Value-state"), marking EXTRA nodes value-bearing even when their role is not
// in the value-role set. No YAML dependency: the block is a flat list of
// strings, so a tiny line parser is enough. Path precedence: REPROIT_CONFIG env,
// else ./reproit.yaml in the cwd. A missing/unparseable file yields an empty
// list (value-less behavior, fully backward-compatible). Same grammar as the
// web runner: key:<id> | role:<role>#<idx>.
func loadValueNodes() -> [String] {
    let env = ProcessInfo.processInfo.environment
    var p = (env["REPROIT_CONFIG"] ?? "").trimmingCharacters(in: .whitespaces)
    if p.isEmpty {
        let def = URL(fileURLWithPath: FileManager.default.currentDirectoryPath)
            .appendingPathComponent("reproit.yaml").path
        if FileManager.default.fileExists(atPath: def) { p = def }
    }
    guard !p.isEmpty, FileManager.default.fileExists(atPath: p),
          let data = FileManager.default.contents(atPath: p),
          let text = String(data: data, encoding: .utf8) else { return [] }
    return parseValueNodes(text)
}

// Extract the `value_nodes:` list items: a block sequence (`value_nodes:` then
// indented `- sel` lines) or an inline flow sequence (`value_nodes: [a, b]`).
// Comments and quotes are stripped. Only the value_nodes key is read.
func parseValueNodes(_ text: String) -> [String] {
    let lines = text.components(separatedBy: .newlines)
    var out: [String] = []
    func clean(_ s: String) -> String {
        var v = s.trimmingCharacters(in: .whitespaces)
        if let h = v.firstIndex(of: "#") { v = String(v[..<h]).trimmingCharacters(in: .whitespaces) }
        if (v.hasPrefix("\"") && v.hasSuffix("\"")) || (v.hasPrefix("'") && v.hasSuffix("'")), v.count >= 2 {
            v = String(v.dropFirst().dropLast())
        }
        return v.trimmingCharacters(in: .whitespaces)
    }
    var i = 0
    while i < lines.count {
        let line = lines[i]
        let trimmed = line.trimmingCharacters(in: .whitespaces)
        if let r = trimmed.range(of: "value_nodes"), trimmed[..<r.lowerBound].isEmpty {
            let afterColon = trimmed.range(of: ":").map { String(trimmed[$0.upperBound...]) } ?? ""
            let inline = afterColon.trimmingCharacters(in: .whitespaces)
            let indent = line.prefix { $0 == " " }.count
            if inline.hasPrefix("[") {
                var body = inline
                if let lb = body.firstIndex(of: "[") { body = String(body[body.index(after: lb)...]) }
                if let rb = body.firstIndex(of: "]") { body = String(body[..<rb]) }
                for part in body.components(separatedBy: ",") {
                    let v = clean(part); if !v.isEmpty { out.append(v) }
                }
                return out
            }
            var j = i + 1
            while j < lines.count {
                let raw = lines[j]
                let t = raw.trimmingCharacters(in: .whitespaces)
                if t.isEmpty || t.hasPrefix("#") { j += 1; continue }
                let childIndent = raw.prefix { $0 == " " }.count
                if childIndent <= indent { break }
                if !t.hasPrefix("-") { break }
                let v = clean(String(t.dropFirst()))
                if !v.isEmpty { out.append(v) }
                j += 1
            }
            return out
        }
        i += 1
    }
    return out
}

// xorshift32: deterministic per seed, same recurrence as the Dart explorer.
final class Rng {
    var s: UInt32
    init(_ seed: UInt32) { s = seed == 0 ? 1 : seed }
    func next(_ n: Int) -> Int {
        s ^= s << 13; s ^= s >> 17; s ^= s << 5
        return Int(s & 0x7fffffff) % n
    }
    func unit() -> Double { Double(next(1 << 20)) / Double(1 << 20) }
}

// ====================================================================
// Canonical STRUCTURAL signature (docs/signature.md). Byte-identical to the
// Rust oracle (crates/reproit/src/model/signature.rs), the iOS/Flutter/web
// SDKs, and proven against signature_vectors.json (see the #if DEBUG self-test
// at the bottom). It hashes the normalized accessibility-node tree (roles + ids
// + types + icons + shape), NOT localized names, so maps merge across platforms.
// ====================================================================

let kRoles: Set<String> = [
    "screen", "header", "text", "button", "link", "textfield", "image", "icon",
    "list", "listitem", "tab", "switch", "checkbox", "radio", "slider", "menu",
    "menuitem", "dialog", "group", "node",
]
let kTransientRoles: Set<String> = [
    "toast", "snackbar", "spinner", "progress", "tooltip", "badge",
]
// Value-role set (docs/signature.md "Value-state", Layer 2). A node is value-
// bearing iff it has a `value` AND either its RAW role is one of these OR it
// carries the opt-in value_node flag (Layer 3). status/log/progressbar/meter/
// timer/output are NOT in the structural vocabulary so they normalize to "node"
// in the body; the value-role test uses the RAW role on purpose. Chrome roles
// (button/header/text/link) are NEVER value-bearing (rule 1 preserved).
let kValueRoles: Set<String> = [
    "textfield", "status", "log", "progressbar", "meter", "timer", "output",
]

// A normalized accessibility node: the input to the signature. Mirrors the Rust
// `Node` JSON shape so signature_vectors.json parses directly via `nodeFromJSON`.
struct SigNode {
    var role: String
    var id: String?
    var type: String?
    var icon: String?
    var transient: Bool = false
    // Layer 2 value-state (docs/signature.md "Value-state"): the node's displayed
    // value, consulted only when the node is value-bearing. nil keeps a tree byte-
    // identical to a pre-value-state tree (no V: section).
    var value: String?
    // Layer 3 opt-in flag: treat the node as value-bearing even when its role is
    // not in kValueRoles (a reproit.yaml value_nodes: selector resolves to this).
    var valueNode: Bool = false
    var children: [SigNode] = []
}

func nodeFromJSON(_ j: [String: Any]) -> SigNode {
    let kids = (j["children"] as? [[String: Any]])?.map { nodeFromJSON($0) } ?? []
    return SigNode(
        role: (j["role"] as? String) ?? "node",
        id: j["id"] as? String,
        type: j["type"] as? String,
        icon: j["icon"] as? String,
        transient: (j["transient"] as? Bool) ?? false,
        value: j["value"] as? String,
        valueNode: (j["value_node"] as? Bool) ?? false,
        children: kids)
}

func normalizeRole(_ role: String) -> String { kRoles.contains(role) ? role : "node" }
func isTransientNode(_ n: SigNode) -> Bool { n.transient || kTransientRoles.contains(n.role) }

// Rules 1, 2, 4: exclude text (no text field exists), drop transient subtrees,
// keep document order. Returns nil if this node itself is transient.
final class NormNode {
    let role: String, type: String?, icon: String?, id: String?
    let children: [NormNode]
    init(_ r: String, _ t: String?, _ ic: String?, _ i: String?, _ c: [NormNode]) {
        role = r; type = t; icon = ic; id = i; children = c
    }
}
func normalizeNode(_ n: SigNode) -> NormNode? {
    if isTransientNode(n) { return nil }
    let kids = n.children.compactMap { normalizeNode($0) }
    return NormNode(normalizeRole(n.role), n.type, n.icon, n.id, kids)
}

// One node's token body: `<role>[:<type>][#<icon>][@<id>]`.
func tokenBody(_ n: NormNode) -> String {
    var s = n.role
    if let t = n.type { s += ":" + t }
    if let ic = n.icon { s += "#" + ic }
    if let i = n.id { s += "@" + i }
    return s
}

// Subtree key for collapse comparison (rule 3): pre-order token list, depths
// re-based to 0, so two sibling subtrees compare equal regardless of depth.
func subtreeKey(_ n: NormNode) -> String {
    var tokens: [String] = []
    func walk(_ n: NormNode, _ d: Int) {
        tokens.append("\(d):\(tokenBody(n))")
        for c in n.children { walk(c, d + 1) }
    }
    walk(n, 0)
    return tokens.joined(separator: ";")
}

func serializeNode(_ n: NormNode, _ depth: Int, _ repeated: Bool, _ tokens: inout [String]) {
    var tok = "\(depth):\(tokenBody(n))"
    if repeated { tok += "*" }
    tokens.append(tok)
    serializeChildren(n.children, depth + 1, &tokens)
}
// Collapse maximal runs of >= 2 consecutive children with identical subtreeKey.
func serializeChildren(_ children: [NormNode], _ depth: Int, _ tokens: inout [String]) {
    var i = 0
    while i < children.count {
        let key = subtreeKey(children[i])
        var j = i + 1
        while j < children.count && subtreeKey(children[j]) == key { j += 1 }
        serializeNode(children[i], depth, (j - i) >= 2, &tokens)
        i = j
    }
}

// ---- Layer 2: value-class identity (canonical, mirrors the Rust oracle) ----
func isValueBearing(_ n: SigNode) -> Bool {
    n.value != nil && (kValueRoles.contains(n.role) || n.valueNode)
}

// Strict ^[+-]?[0-9]+(\.[0-9]+)?$: optional sign, >=1 ASCII digits, optional
// period + >=1 ASCII digits. No grouping, no exponent, no leading/trailing dot.
func isStrictDecimal(_ s: String) -> Bool {
    let u = Array(s.utf8)
    var i = 0
    if i < u.count && (u[i] == 0x2b || u[i] == 0x2d) { i += 1 }
    let intStart = i
    while i < u.count && u[i] >= 0x30 && u[i] <= 0x39 { i += 1 }
    if i == intStart { return false }
    if i < u.count && u[i] == 0x2e {
        i += 1
        let fracStart = i
        while i < u.count && u[i] >= 0x30 && u[i] <= 0x39 { i += 1 }
        if i == fracStart { return false }
    }
    return i == u.count
}

// Bounded, deterministic, locale-safe value-class token (docs/signature.md
// "Value-state"). Identical rule to the oracle's value_class.
func valueClass(_ s: String) -> String {
    let t = s.trimmingCharacters(in: .whitespacesAndNewlines)
    if t.isEmpty { return "EMPTY" }
    if isStrictDecimal(t) {
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

// The V:-section key for a value-bearing node: its stable id if present, else the
// structural fallback role:<role>#<idx> using the NORMALIZED role.
func valueKeyOf(_ n: SigNode, _ structuralIndex: Int) -> String {
    if let id = n.id { return "key:\(id)" }
    return "role:\(normalizeRole(n.role))#\(structuralIndex)"
}

// Collect (value_key, value_class) pairs in pre-order, skipping transient
// subtrees (rule 2) so the V: section stays consistent with the structural body.
func collectValueChildren(_ node: SigNode, _ out: inout [(String, String)]) {
    var roleCounts: [String: Int] = [:]
    for c in node.children {
        if isTransientNode(c) { continue }
        let role = normalizeRole(c.role)
        let idx = roleCounts[role] ?? 0
        roleCounts[role] = idx + 1
        if isValueBearing(c) { out.append((valueKeyOf(c, idx), valueClass(c.value ?? ""))) }
        collectValueChildren(c, &out)
    }
}
func valuePairs(_ root: SigNode) -> [(String, String)] {
    var out: [(String, String)] = []
    if isTransientNode(root) { return out }
    if isValueBearing(root) { out.append((valueKeyOf(root, 0), valueClass(root.value ?? ""))) }
    collectValueChildren(root, &out)
    out.sort { $0.0 < $1.0 }
    return out
}

// The V: section suffix. "" when no value-bearing node exists (byte-identical to
// a pre-value-state tree); else "\nV:" + sorted key=class entries. `excludeKeys`
// drops capped value-keys (Layer 2 "Hard cap"); empty for the canonical sig.
func valueSection(_ pairs: [(String, String)], _ excludeKeys: Set<String>) -> String {
    let kept = excludeKeys.isEmpty ? pairs : pairs.filter { !excludeKeys.contains($0.0) }
    if kept.isEmpty { return "" }
    return "\nV:" + kept.map { "\($0.0)=\($0.1)" }.joined(separator: ";")
}

// The exact UTF-8 descriptor that gets hashed: `"A:" + anchor + "\n" + body`,
// with the Layer 2 V: section appended only when >=1 value-bearing node exists.
func descriptorFrom(_ anchor: String?, _ root: SigNode, _ excludeKeys: Set<String>) -> String {
    var tokens: [String] = []
    if let norm = normalizeNode(root) { serializeNode(norm, 0, false, &tokens) }
    let v = valueSection(valuePairs(root), excludeKeys)
    return "A:\(anchor ?? "")\n\(tokens.joined(separator: ";"))\(v)"
}
func descriptorOf(_ anchor: String?, _ root: SigNode) -> String {
    return descriptorFrom(anchor, root, [])
}

func fnv1a32hex(_ bytes: [UInt8]) -> String {
    var h: UInt32 = 0x811c_9dc5
    for b in bytes { h ^= UInt32(b); h = h &* 0x0100_0193 }
    return String(format: "%08x", h)
}

// Canonical structural+value signature: FNV-1a 32-bit over the descriptor, 8 hex.
func signatureOf(_ anchor: String?, _ root: SigNode) -> String {
    return fnv1a32hex(Array(descriptorOf(anchor, root).utf8))
}
// The canonical signature with capped value-keys excluded (runner cap).
func signatureFrom(_ anchor: String?, _ root: SigNode, _ excludeKeys: Set<String>) -> String {
    return fnv1a32hex(Array(descriptorFrom(anchor, root, excludeKeys).utf8))
}

// ---- AX helpers ---------------------------------------------------------
func axCopy(_ el: AXUIElement, _ attr: String) -> CFTypeRef? {
    var v: CFTypeRef?
    return AXUIElementCopyAttributeValue(el, attr as CFString, &v) == .success ? v : nil
}
func axStr(_ el: AXUIElement, _ attr: String) -> String { (axCopy(el, attr) as? String) ?? "" }
func axChildren(_ el: AXUIElement) -> [AXUIElement] {
    (axCopy(el, kAXChildrenAttribute as String) as? [AXUIElement]) ?? []
}
func axActions(_ el: AXUIElement) -> [String] {
    var names: CFArray?
    return AXUIElementCopyActionNames(el, &names) == .success ? (names as? [String] ?? []) : []
}

// A named, interactive node: title > description > value, like the a11y
// "named" rule in the Dart explorer (any of the three gives a screen reader
// something to announce). DISPLAY-ONLY: names never enter the signature.
func labelOf(_ el: AXUIElement) -> String {
    let t = axStr(el, kAXTitleAttribute as String)
    if !t.isEmpty { return t }
    let d = axStr(el, kAXDescriptionAttribute as String)
    if !d.isEmpty { return d }
    return axStr(el, kAXValueAttribute as String)
}

// ---- AXRole -> canonical role mapping ----------------------------------
// Derived from AXRole (+ AXSubrole / AXRoleDescription), never from the visible
// label. Covers AppKit, SwiftUI, and the Qt/GTK/wxWidgets/Avalonia bridges that
// publish to the same AX API. Anything unknown falls to `group`/`node`.
// AXRole / AXSubrole string constants, captured into a single table. Some of
// these constants live in the AppKit (NSAccessibility) overlay and some in
// HIServices; referencing them in `switch`/`case` *pattern* position trips a
// Swift module-overload lookup bug ("cannot find ... in scope"), so we bind
// them to plain String values here and compare with `==` instead.
private let axButton = kAXButtonRole as String
private let axPopUp = kAXPopUpButtonRole as String
private let axMenuButton = kAXMenuButtonRole as String
// kAXToolbarButtonRole / kAXLinkRole are not exported as global constants when
// AppKit is imported (they live only under NSAccessibility.Role), so use their
// stable underlying AXRole string values directly.
private let axToolbarButton = "AXToolbarButton"
private let axLink = "AXLink"
private let axStaticText = kAXStaticTextRole as String
private let axHeading = kAXHeadingRole as String
private let axTextField = kAXTextFieldRole as String
private let axTextArea = kAXTextAreaRole as String
private let axComboBox = kAXComboBoxRole as String
private let axImage = kAXImageRole as String
private let axCheckBox = kAXCheckBoxRole as String
private let axRadioButton = kAXRadioButtonRole as String
private let axSlider = kAXSliderRole as String
private let axIncrementor = kAXIncrementorRole as String
private let axTabGroup = kAXTabGroupRole as String
private let axRadioGroup = kAXRadioGroupRole as String
private let axList = kAXListRole as String
private let axTable = kAXTableRole as String
private let axOutline = kAXOutlineRole as String
private let axBrowser = kAXBrowserRole as String
private let axRow = kAXRowRole as String
private let axCell = kAXCellRole as String
private let axMenu = kAXMenuRole as String
private let axMenuBar = kAXMenuBarRole as String
private let axMenuItem = kAXMenuItemRole as String
private let axMenuBarItem = kAXMenuBarItemRole as String
private let axSheet = kAXSheetRole as String
private let axDrawer = kAXDrawerRole as String
private let axPopover = kAXPopoverRole as String
private let axWindow = kAXWindowRole as String
private let axProgress = kAXProgressIndicatorRole as String
private let axBusy = kAXBusyIndicatorRole as String
private let axGroup = kAXGroupRole as String
private let axScrollArea = kAXScrollAreaRole as String
private let axSplitGroup = kAXSplitGroupRole as String
private let axToolbar = kAXToolbarRole as String
private let axUnknown = kAXUnknownRole as String
private let axSwitchSub = kAXSwitchSubrole as String
private let axToggleSub = kAXToggleSubrole as String

func axRoleOf(_ el: AXUIElement) -> String {
    let role = axStr(el, kAXRoleAttribute)
    let sub = axStr(el, kAXSubroleAttribute)
    if role == axButton || role == axPopUp || role == axMenuButton || role == axToolbarButton {
        return "button"
    }
    if role == axLink { return "link" }
    if role == axStaticText { return "text" }
    if role == axHeading { return "header" }
    // A search field is a text field with the search subrole.
    if role == axTextField || role == axTextArea || role == axComboBox { return "textfield" }
    if role == axImage { return "image" }
    if role == axCheckBox {
        // AppKit models switches as a checkbox with the "Switch"/toggle subrole.
        if sub == axSwitchSub || sub == "AXSwitch" || sub == axToggleSub { return "switch" }
        return "checkbox"
    }
    if role == axRadioButton { return "radio" }
    if role == axSlider || role == axIncrementor { return "slider" }
    if role == axTabGroup { return "tab" }
    if role == axRadioGroup { return "group" }
    if role == axList || role == axTable || role == axOutline || role == axBrowser { return "list" }
    if role == axRow || role == axCell { return "listitem" }
    if role == axMenu || role == axMenuBar { return "menu" }
    if role == axMenuItem || role == axMenuBarItem { return "menuitem" }
    if role == axSheet || role == axDrawer || role == axPopover || role == axWindow { return "dialog" }
    if role == axProgress || role == axBusy { return "progress" } // transient
    if role == axGroup || role == axScrollArea || role == axSplitGroup
        || role == axToolbar || role == axUnknown || role.isEmpty {
        return "group"
    }
    return "node"
}

// Stable developer identifier: AXIdentifier (the macOS analogue of a test-id /
// resource-id). Empty -> nil so it is omitted from the token.
func axIdentifierOf(_ el: AXUIElement) -> String? {
    let id = axStr(el, "AXIdentifier")
    return id.isEmpty ? nil : id
}

// Optional input-type refinement, only for textfields. AX exposes a secure-text
// subrole for password fields and a search subrole for search fields; otherwise
// default to text.
private let axSecureSub = kAXSecureTextFieldSubrole as String
private let axSearchSub = kAXSearchFieldSubrole as String

func axTypeOf(_ el: AXUIElement, _ role: String) -> String? {
    guard role == "textfield" else { return nil }
    let sub = axStr(el, kAXSubroleAttribute)
    if sub == axSecureSub { return "password" }
    if sub == axSearchSub { return "search" }
    return "text"
}

// ---- AX value-state detection (docs/signature.md "Value-state") --------
// AXValue is the live/value semantic AX exposes on a value-bearing element. We
// treat an element as value-bearing when it exposes an AXValue AND it sits on a
// value-role: a text field / text area (its entered text), a slider / value
// indicator (its measured value), or a status/live-region role (AXStaticText
// the developer keeps current). Chrome roles (button/header/link/text label)
// are never value-bearing, so the chrome-text exclusion (rule 1) is preserved.
private let axValueIndicator = kAXValueIndicatorRole as String
private let axLevelIndicator = kAXLevelIndicatorRole as String

// Does the element publish a live AXValue attribute at all (regardless of role)?
func axHasValueAttribute(_ el: AXUIElement) -> Bool {
    var names: CFArray?
    guard AXUIElementCopyAttributeNames(el, &names) == .success,
          let arr = names as? [String] else { return false }
    return arr.contains(kAXValueAttribute as String)
}

// True if the raw AX element exposes a live/value semantic on a value role:
// AXValue present on a text field / text area / slider / value-or-level
// indicator. The canonical SigNode then carries the value + value_node flag so
// the oracle folds a bounded value-class into the V: section.
func axIsValueBearing(_ el: AXUIElement) -> Bool {
    let role = axStr(el, kAXRoleAttribute)
    let valueRoles: Set<String> = [
        axTextField, axTextArea, axComboBox, axSlider, axIncrementor,
        axValueIndicator, axLevelIndicator,
    ]
    if valueRoles.contains(role) { return axHasValueAttribute(el) }
    return false
}

// The displayed value of a value-bearing element: its AXValue rendered to a
// string (numbers, booleans, and text all reduce to one bounded value-class by
// the oracle). Secure text fields never expose their content via AX, so they
// classify to EMPTY naturally. The raw value never enters the hash verbatim.
func axValueOf(_ el: AXUIElement) -> String {
    guard let v = axCopy(el, kAXValueAttribute as String) else { return "" }
    if let s = v as? String { return s }
    if let n = v as? NSNumber { return n.stringValue }
    return "\(v)"
}

// Heuristic transient detection: progress/busy indicators by role, or an
// AXIdentifier hint a developer set (toast/snackbar/spinner/tooltip/badge).
func axIsTransient(_ el: AXUIElement, _ role: String) -> Bool {
    if role == "progress" { return true }
    let id = (axStr(el, "AXIdentifier")).lowercased()
    for hint in ["toast", "snackbar", "spinner", "progress", "tooltip", "badge"] {
        if id.contains(hint) { return true }
    }
    return false
}

struct Snapshot {
    var sig: String            // canonical (structural + value) signature
    var structuralSig: String  // structural-only sig: the per-node key the cap tracks
    var vsection: String       // the V: section body ("" when none)
    var content: String        // Layer-1 content fingerprint (runner-local, ephemeral)
    var labels: [String]
    var tappables: [String]
    var nodeByLabel: [String: AXUIElement]
}

func snapshot(_ app: AXUIElement, _ valueNodeSelectors: [String]) -> Snapshot {
    var labels: [String] = []
    var tappables: [String] = []
    var nodeByLabel: [String: AXUIElement] = [:]
    // Layer-1 content fingerprint source: (stable-key, trimmed raw text) over
    // value-bearing / keyed-text nodes. Sorted before joining so it is order-
    // independent. Carries raw localized text; NEVER folded into the canonical key.
    var textNodes: [(String, String)] = []

    // Resolve the Layer-3 role:<role>#<idx> selectors once: walk the same tree the
    // snapshot walks and record the element each selector points at, so a keyless
    // value-node can be matched by identity below.
    var roleIndexTargets: [String: AXUIElement] = [:] // "role:r#i" -> element
    let needRoleResolution = valueNodeSelectors.contains { $0.hasPrefix("role:") }

    // Build the canonical SigNode tree AND gather display labels in one pass.
    func build(_ el: AXUIElement, _ depth: Int, isRoot: Bool, roleCounter: inout [String: Int]) -> SigNode? {
        if depth > 60 { return nil }
        let label = labelOf(el).trimmingCharacters(in: .whitespacesAndNewlines)
        if !label.isEmpty && label.count <= maxLabelLen {
            labels.append(label)
            if axActions(el).contains(kAXPressAction as String) {
                tappables.append(label)
                if nodeByLabel[label] == nil { nodeByLabel[label] = el }
            }
        }
        let role = isRoot ? "screen" : axRoleOf(el)
        let id = axIdentifierOf(el)

        // Layer 2/3 value detection. A value-bearing node (an AX value role with a
        // live AXValue, or a Layer-3 opt-in selector match) carries its value + the
        // value_node flag so the oracle folds a bounded value-class into V:. A
        // value-bearing node WINS over the transient heuristic.
        let optIn = !isRoot && matchesValueNodeAX(
            el, id: id, role: role, selectors: valueNodeSelectors, roleIndexTargets: roleIndexTargets)
        let valueBearing = !isRoot && (axIsValueBearing(el) || optIn)
        let value: String? = valueBearing ? axValueOf(el) : nil
        if valueBearing {
            let fkey = id != nil ? "key:\(id!)" : "role:\(normalizeRole(role))"
            textNodes.append((fkey, value ?? ""))
        }
        let transient = !isRoot && !valueBearing && axIsTransient(el, role)

        var kids: [SigNode] = []
        for c in axChildren(el) {
            if let n = build(c, depth + 1, isRoot: false, roleCounter: &roleCounter) { kids.append(n) }
        }
        return SigNode(
            role: role,
            id: id,
            type: axTypeOf(el, role),
            icon: nil, // AX exposes no language-independent icon identity
            transient: transient,
            value: value,
            valueNode: valueBearing,
            children: kids)
    }

    // First pass: resolve role:<role>#<idx> selector targets by walking the tree
    // in the same document order the build pass uses.
    func resolveRoleTargets(_ roots: [AXUIElement]) {
        var counts: [String: Int] = [:]
        func walk(_ el: AXUIElement, _ depth: Int) {
            if depth > 60 { return }
            let role = normalizeRole(axRoleOf(el))
            let idx = counts[role] ?? 0
            counts[role] = idx + 1
            let keyEl = "role:\(role)#\(idx)"
            for sel in valueNodeSelectors where sel == keyEl { roleIndexTargets[sel] = el }
            for c in axChildren(el) { walk(c, depth + 1) }
        }
        for r in roots { walk(r, 1) }
    }

    // Wrap the app's windows in a single `screen` root so the structure is
    // anchored the same way as the SDKs (one screen node at depth 0).
    let windows = (axCopy(app, kAXWindowsAttribute as String) as? [AXUIElement]) ?? []
    var windowKids: [AXUIElement] = []
    for w in windows { windowKids.append(contentsOf: axChildren(w)) }
    if needRoleResolution { resolveRoleTargets(windowKids) }

    var rootKids: [SigNode] = []
    var roleCounter: [String: Int] = [:]
    for c in windowKids {
        // Each window's own children become the screen's children; the window
        // chrome itself is not a separate structural level.
        if let n = build(c, 1, isRoot: false, roleCounter: &roleCounter) { rootKids.append(n) }
    }
    let root = SigNode(role: "screen", children: rootKids)

    let sig = signatureOf(nil, root)
    // Structural-only signature (no V: section): the per-node key the Layer-1 cap
    // tracks. Strip the V: suffix from the descriptor and re-hash, so it is the
    // exact pre-value-state signature of this structure.
    let full = descriptorOf(nil, root)
    var structuralSig = sig
    var vsection = ""
    if let range = full.range(of: "\nV:") {
        vsection = String(full[range.upperBound...])
        structuralSig = fnv1a32hex(Array(full[..<range.lowerBound].utf8))
    }
    // Layer-1 content fingerprint: structural sig + sorted (stable-key, raw text).
    textNodes.sort { $0.0 != $1.0 ? $0.0 < $1.0 : $0.1 < $1.1 }
    let content = sig + "|" + textNodes.map { "\($0.0)=\($0.1)" }.joined(separator: ";")

    return Snapshot(
        sig: sig,
        structuralSig: structuralSig,
        vsection: vsection,
        content: content,
        labels: Array(Set(labels)),
        tappables: Array(Set(tappables)),
        nodeByLabel: nodeByLabel
    )
}

// True if the AX element matches a Layer-3 value_nodes selector (docs/signature.md
// "Value-state"): key:<id> matches AXIdentifier; role:<role>#<idx> matches the
// pre-resolved element at that role index.
func matchesValueNodeAX(
    _ el: AXUIElement, id: String?, role: String, selectors: [String],
    roleIndexTargets: [String: AXUIElement]
) -> Bool {
    if selectors.isEmpty { return false }
    for sel in selectors {
        if sel.isEmpty { continue }
        if sel.hasPrefix("key:") {
            let want = String(sel.dropFirst(4))
            if !want.isEmpty, let id = id, id == want { return true }
        } else if sel.hasPrefix("role:") {
            if let target = roleIndexTargets[sel], CFEqual(target, el) { return true }
        }
    }
    return false
}

// ---- canonical-signature self-test (golden vectors) ---------------------
// `swift runners/macos-ax.swift --selftest` loads signature_vectors.json and
// asserts signatureOf(anchor, tree) == expected_sig for ALL vectors, exactly
// like the Rust oracle's golden_vectors_match. Run in CI to gate drift without
// needing a live app or Accessibility permission. Also runs automatically under
// a DEBUG build when REPROIT_SELFTEST=1 is set.
func runSelfTest() -> Bool {
    func vectorsPath() -> String? {
        let env = ProcessInfo.processInfo.environment
        if let p = env["REPROIT_VECTORS"], !p.isEmpty { return p }
        // This source lives at <repo>/runners/macos-ax.swift; vectors at root.
        let here = URL(fileURLWithPath: #filePath)
        let root = here.deletingLastPathComponent().deletingLastPathComponent()
        let cand = root.appendingPathComponent("signature_vectors.json").path
        if FileManager.default.fileExists(atPath: cand) { return cand }
        let cwd = URL(fileURLWithPath: FileManager.default.currentDirectoryPath)
            .appendingPathComponent("signature_vectors.json").path
        return FileManager.default.fileExists(atPath: cwd) ? cwd : nil
    }
    guard let path = vectorsPath(),
          let data = FileManager.default.contents(atPath: path),
          let arr = (try? JSONSerialization.jsonObject(with: data)) as? [[String: Any]]
    else {
        FileHandle.standardError.write("selftest: could not load signature_vectors.json\n".data(using: .utf8)!)
        return false
    }
    var ok = true
    for v in arr {
        let anchor = v["anchor"] as? String
        let tree = nodeFromJSON((v["tree"] as? [String: Any]) ?? [:])
        let expected = (v["expected_sig"] as? String) ?? ""
        let got = signatureOf(anchor, tree)
        if got != expected {
            ok = false
            let desc = (v["description"] as? String) ?? ""
            let line = "selftest FAIL '\(desc)': expected \(expected) got \(got)\n  descriptor=\(descriptorOf(anchor, tree).debugDescription)\n"
            FileHandle.standardError.write(line.data(using: .utf8)!)
        }
    }
    // The current contract ships 24 golden vectors (structural + value-state).
    // Assert ALL of them are present, so a truncated vectors file fails the gate.
    let expectedCount = 24
    if arr.count != expectedCount {
        ok = false
        FileHandle.standardError.write(
            "selftest FAIL: expected \(expectedCount) vectors, got \(arr.count)\n".data(using: .utf8)!)
    }
    // Spot-check the value-state relationships the spec promises (Layer 2), so a
    // value-class regression is caught even if a golden hash were updated wrong.
    if !runValueStateChecks() { ok = false }
    emit(ok ? "SELFTEST PASS \(arr.count) vectors" : "SELFTEST FAIL")
    return ok
}

// Assert the Layer-2 value-class behaviors directly (mirrors the oracle unit
// tests). Returns true on success. Logs the first failure to stderr.
func runValueStateChecks() -> Bool {
    var ok = true
    func check(_ cond: Bool, _ msg: String) {
        if !cond { ok = false; FileHandle.standardError.write("selftest value-state FAIL: \(msg)\n".data(using: .utf8)!) }
    }
    // value_class buckets.
    let buckets: [(String, String)] = [
        ("", "EMPTY"), ("   ", "EMPTY"), ("0", "ZERO"), ("-0", "ZERO"), ("-3", "NEG"),
        ("3", "POS1"), ("+7", "POS1"), ("99", "POS2"), ("100", "POS3"), ("1000", "POSL"),
        ("1,234", "NONEMPTY"), ("3.", "NONEMPTY"), (".5", "NONEMPTY"), ("$5", "NONEMPTY"),
    ]
    for (s, want) in buckets { check(valueClass(s) == want, "value_class(\(s.debugDescription))=\(valueClass(s)) want \(want)") }
    // chrome value is NOT value-bearing: byte-identical to no value.
    let header = SigNode(role: "header", id: "title", value: "Welcome")
    check(descriptorOf(nil, header) == "A:\n0:header@title", "chrome value leaked into V:")
    // value-role textfield folds a V: entry; status normalizes to node in body.
    let tf = SigNode(role: "textfield", id: "email", value: "a@b.com")
    check(descriptorOf(nil, tf) == "A:\n0:textfield@email\nV:key:email=NONEMPTY", "textfield V: wrong")
    let status = SigNode(role: "status", id: "count", value: "5")
    check(descriptorOf(nil, status) == "A:\n0:node@count\nV:key:count=POS1", "status V: wrong")
    // opt-in value_node folds a chrome node's value-class into V:.
    var optIn = SigNode(role: "text", id: "display", value: "42")
    check(descriptorOf(nil, optIn) == "A:\n0:text@display", "chrome text leaked without flag")
    optIn.valueNode = true
    check(descriptorOf(nil, optIn) == "A:\n0:text@display\nV:key:display=POS2", "opt-in value_node V: wrong")
    // keyless value nodes collapse structurally but stay distinct in V:.
    let keyless = SigNode(role: "screen", children: [
        SigNode(role: "textfield", value: "3"),
        SigNode(role: "textfield", value: "99"),
    ])
    check(descriptorOf(nil, keyless) == "A:\n0:screen;1:textfield*\nV:role:textfield#0=POS1;role:textfield#1=POS2",
          "keyless value index wrong")
    // runner cap drops a capped key from V:, falling back to structural-only.
    let capped = signatureFrom(nil, tf, ["key:email"])
    check(capped == signatureOf(nil, SigNode(role: "textfield", id: "email")), "cap exclude wrong")
    return ok
}

func emitEdge(_ from: String, _ action: String, _ to: String) {
    let payload: [String: Any] = ["from": from, "action": action, "to": to]
    if let d = try? JSONSerialization.data(withJSONObject: payload),
       let s = String(data: d, encoding: .utf8) {
        emit("EXPLORE:EDGE \(s)")
    }
}

func pressKey(_ code: CGKeyCode) {
    let src = CGEventSource(stateID: .hidSystemState)
    CGEvent(keyboardEventSource: src, virtualKey: code, keyDown: true)?.post(tap: .cghidEventTap)
    CGEvent(keyboardEventSource: src, virtualKey: code, keyDown: false)?.post(tap: .cghidEventTap)
}

func crashBlock(_ title: String, _ detail: String) {
    emit("EXCEPTION CAUGHT BY REPROIT ╡ \(title) ╞")
    emit("The following condition was hit: \(detail)")
    emit("════════")
}

// ---- screenshot capture (SHOOT contract, see crates/.../backends/drive.rs) --
// The orchestrator passes REPROIT_SHOTS_DIR (absolute) and, on a named shoot
// point, expects <dir>/<name>.png to exist before it sees `SHOOT:<name>` on
// stdout. <name> is [A-Za-z0-9_/-]. If REPROIT_SHOTS_DIR is unset we still print
// the marker (capture is best-effort, the orchestrator just logs a miss).

// The CGWindowID of the target app's frontmost on-screen window, matched by pid
// via the CGWindowList. `screencapture -l <id>` then captures exactly that
// window (chrome + shadow) rather than the whole desktop, which is what we want
// even when the window was moved off-screen.
func targetWindowID(_ pid: pid_t) -> CGWindowID? {
    let opts: CGWindowListOption = [.optionOnScreenOnly, .excludeDesktopElements]
    guard let infos = CGWindowListCopyWindowInfo(opts, kCGNullWindowID) as? [[String: Any]] else {
        return nil
    }
    for info in infos {
        guard let owner = info[kCGWindowOwnerPID as String] as? pid_t, owner == pid,
              let num = info[kCGWindowNumber as String] as? CGWindowID else { continue }
        // Skip zero-area helper layers; take the first real window for this pid.
        if let bounds = info[kCGWindowBounds as String] as? [String: Any],
           let w = bounds["Width"] as? CGFloat, let h = bounds["Height"] as? CGFloat,
           w < 1 || h < 1 { continue }
        return num
    }
    return nil
}

// The focused window's AX frame (screen coordinates), as a fallback when no
// CGWindowID is on-screen (e.g. the window was pushed fully off the display).
func targetWindowFrame(_ app: AXUIElement) -> CGRect? {
    guard let windows = axCopy(app, kAXWindowsAttribute as String) as? [AXUIElement],
          let w = windows.first else { return nil }
    var origin = CGPoint.zero
    var size = CGSize.zero
    if let posV = axCopy(w, kAXPositionAttribute as String) {
        AXValueGetValue(posV as! AXValue, .cgPoint, &origin)
    }
    if let sizeV = axCopy(w, kAXSizeAttribute as String) {
        AXValueGetValue(sizeV as! AXValue, .cgSize, &size)
    }
    if size.width < 1 || size.height < 1 { return nil }
    return CGRect(origin: origin, size: size)
}

// Capture the target window to <shotsDir>/<name>.png, then print SHOOT:<name>.
// Targets the window (by CGWindowID, else its AX frame rect), never the whole
// desktop. With REPROIT_SHOTS_DIR unset, skips capture but still emits the marker.
func shoot(_ name: String, _ pid: pid_t, _ app: AXUIElement) {
    let shotsDir = ProcessInfo.processInfo.environment["REPROIT_SHOTS_DIR"] ?? ""
    if !shotsDir.isEmpty {
        let outURL = URL(fileURLWithPath: shotsDir).appendingPathComponent("\(name).png")
        try? FileManager.default.createDirectory(
            at: outURL.deletingLastPathComponent(), withIntermediateDirectories: true)
        let out = outURL.path
        let proc = Process()
        proc.executableURL = URL(fileURLWithPath: "/usr/sbin/screencapture")
        if let wid = targetWindowID(pid) {
            // -x: no capture sound. -l <id>: capture just that window.
            proc.arguments = ["-x", "-l", "\(wid)", out]
        } else if let f = targetWindowFrame(app) {
            // -R<x,y,w,h>: capture the window's screen rect (off-screen windows
            // still capture from the framebuffer region they occupy).
            proc.arguments = ["-x", "-R\(Int(f.origin.x)),\(Int(f.origin.y)),\(Int(f.size.width)),\(Int(f.size.height))", out]
        } else {
            proc.arguments = ["-x", out] // last resort: whole desktop
        }
        try? proc.run()
        proc.waitUntilExit()
    }
    emit("SHOOT:\(name)")
}

// Robust "did the target die?" check. A synchronous in-press crash makes
// AXUIElementPerformAction return a non-success status (the app went away
// mid-action), but the process / running-applications state is the ground
// truth, so check both: (1) the AX status indicating a gone/invalid target,
// and (2) the actual process / running-app state. Either signal counts as a
// real termination, so a press that fails *because the app crashed* is not
// mistaken for a benign FUZZ:MISS.
func axErrorMeansAppGone(_ status: AXError) -> Bool {
    switch status {
    // The app/element no longer exists, the process is gone, or AX can no
    // longer reach it: all consistent with the target having died mid-press.
    case .invalidUIElement, .cannotComplete, .notImplemented, .apiDisabled:
        return true
    default:
        return false
    }
}

func targetIsDead(_ app: NSRunningApplication, _ appEl: AXUIElement, _ pressStatus: AXError) -> Bool {
    // 1) NSRunningApplication's own view of the process.
    if app.isTerminated { return true }
    // 2) Is the pid still a live process at all? (kill 0 probes existence.)
    if kill(app.processIdentifier, 0) != 0 && errno == ESRCH { return true }
    // 3) Is the bundle id / app still in the running-applications list?
    if let bid = app.bundleIdentifier {
        let stillListed = NSWorkspace.shared.runningApplications.contains {
            $0.bundleIdentifier == bid && $0.processIdentifier == app.processIdentifier
        }
        if !stillListed { return true }
    }
    // 4) The press status says the AX element / app is gone, and a fresh AX
    //    probe of the application element now fails too (so it is not a
    //    one-off transient on a single control).
    if axErrorMeansAppGone(pressStatus) {
        var pidOut: pid_t = 0
        if AXUIElementGetPid(appEl, &pidOut) != .success { return true }
        var v: CFTypeRef?
        let probe = AXUIElementCopyAttributeValue(appEl, kAXRoleAttribute as CFString, &v)
        if axErrorMeansAppGone(probe) { return true }
    }
    return false
}

// ---- launch / attach ----------------------------------------------------
func runningApp(_ target: String) -> NSRunningApplication? {
    if let a = NSRunningApplication.runningApplications(withBundleIdentifier: target).first { return a }
    return NSWorkspace.shared.runningApplications.first { $0.localizedName == target }
}

func launch(_ target: String) -> NSRunningApplication? {
    if let a = runningApp(target) { return a }
    guard let url = NSWorkspace.shared.urlForApplication(withBundleIdentifier: target) else { return nil }
    let cfg = NSWorkspace.OpenConfiguration()
    // Most macOS apps don't build their window accessibility tree until they
    // are foregrounded at least once (verified: Calculator returns an empty AX
    // tree when launched in the background). So activate by default. On a
    // dedicated test agent or VM, where the focus blip is harmless, that's
    // fine; set REPROIT_MAC_ACTIVATE=0 to attempt a background launch anyway.
    cfg.activates = ProcessInfo.processInfo.environment["REPROIT_MAC_ACTIVATE"] != "0"
    let sem = DispatchSemaphore(value: 0)
    var result: NSRunningApplication?
    NSWorkspace.shared.openApplication(at: url, configuration: cfg) { app, _ in
        result = app; sem.signal()
    }
    _ = sem.wait(timeout: .now() + 12)
    return result
}

/// Move the app's window off the visible display so it never shows on the
/// user's desktop, while staying on the ACTIVE Space (off-screen avoids the
/// occlusion / App Nap throttling a background Space would impose on the a11y
/// tree). Disable with REPROIT_MAC_OFFSCREEN=0.
func moveOffscreen(_ app: AXUIElement) {
    if ProcessInfo.processInfo.environment["REPROIT_MAC_OFFSCREEN"] == "0" { return }
    guard let windows = axCopy(app, kAXWindowsAttribute as String) as? [AXUIElement] else { return }
    var pt = CGPoint(x: -12000, y: 0)
    guard let value = AXValueCreate(.cgPoint, &pt) else { return }
    for w in windows {
        AXUIElementSetAttributeValue(w, kAXPositionAttribute as CFString, value)
    }
}

// ---- main ---------------------------------------------------------------
let env = ProcessInfo.processInfo.environment

// Self-test mode: validate the canonical signature against the golden vectors
// without launching an app or needing Accessibility permission. Used by CI.
if CommandLine.arguments.contains("--selftest") || env["REPROIT_SELFTEST"] == "1" {
    exit(runSelfTest() ? 0 : 1)
}

guard let target = env["REPROIT_TARGET"], !target.isEmpty else {
    FileHandle.standardError.write("REPROIT_TARGET (bundle id or app name) required\n".data(using: .utf8)!)
    exit(2)
}
emit("JOURNEY claimed role=a")
guard AXIsProcessTrusted() else {
    crashBlock("accessibility not trusted",
               "grant Accessibility to this process in System Settings > Privacy & Security")
    exit(3)
}
guard let nsApp = launch(target) else {
    crashBlock("target not found", "could not launch \(target)")
    exit(3)
}
if env["REPROIT_MAC_ACTIVATE"] != "0" { nsApp.activate() }
let appEl = AXUIElementCreateApplication(nsApp.processIdentifier)
Thread.sleep(forTimeInterval: 1.2)
moveOffscreen(appEl)
Thread.sleep(forTimeInterval: 0.8)

let fuzz = loadFuzz()
let rng = Rng(fuzz.seed)
if fuzz.seed != 0 { emit("JOURNEY[a] step: fuzz seed=\(fuzz.seed)") }

// Layer-3 opt-in value-node selectors from reproit.yaml (empty if none).
let valueNodeSelectors = loadValueNodes()
if !valueNodeSelectors.isEmpty { emit("JOURNEY[a] step: value_nodes=\(valueNodeSelectors.count)") }

var seen = Set<String>()
var tried = Set<String>()

// Layer-1/2 hard cap (docs/signature.md "Value-state"): per structural node,
// track the DISTINCT value-class combinations seen. Once a node exceeds
// valueClassCap, fall back to its structural-only signature for the rest of the
// run so an adversarial value generator cannot explode the graph. The oracle is
// stateless; the cap is purely runner-local.
let valueClassCap = 8
var valueCombos: [String: Set<String>] = [:]   // structuralSig -> set of V: sections
var cappedNodes = Set<String>()                // structuralSig that hit the cap

// The EFFECTIVE signature for a snapshot, applying the runner-local cap: the
// full value-folded sig unless this structural node is capped, then structural.
func effectiveSig(_ snap: Snapshot) -> String {
    if cappedNodes.contains(snap.structuralSig) { return snap.structuralSig }
    if !snap.vsection.isEmpty {
        var set = valueCombos[snap.structuralSig] ?? Set<String>()
        set.insert(snap.vsection)
        valueCombos[snap.structuralSig] = set
        if set.count > valueClassCap {
            cappedNodes.insert(snap.structuralSig)
            emit("JOURNEY[a] step: value-cap hit (\(snap.structuralSig))")
            return snap.structuralSig
        }
    }
    return snap.sig
}

func observe() -> Snapshot {
    var snap = snapshot(appEl, valueNodeSelectors)
    snap.sig = effectiveSig(snap)
    if seen.insert(snap.sig).inserted {
        let payload: [String: Any] = ["sig": snap.sig, "labels": Array(snap.labels.prefix(maxLabelsPerState))]
        if let d = try? JSONSerialization.data(withJSONObject: payload),
           let s = String(data: d, encoding: .utf8) {
            emit("EXPLORE:STATE \(s)")
        }
    }
    return snap
}

var current = observe()
var stuck = 0
var failed = false
let prefixLen = fuzz.prefix?.count ?? 0
let budget = fuzz.replay?.count ?? (fuzz.budget + prefixLen)
var i = 0
while i < budget && stuck < 3 {
    var act: String?
    if let r = fuzz.replay { act = i < r.count ? r[i] : nil }
    else if i < prefixLen { act = fuzz.prefix![i] }
    else if fuzz.seed != 0 {
        // Inverse-visit-count weighted pick, identical to the other runners.
        let taps = current.tappables.sorted()
        let ew = fuzz.edgeWeights[current.sig] ?? [:]
        var options = taps.map { "tap:\($0)" }
        options.append("back")
        let weights = options.map { 1.0 / (1.0 + Double(ew[$0] ?? 0)) }
        let total = weights.reduce(0, +)
        var r = rng.unit() * total
        act = options.last
        for k in 0..<options.count { r -= weights[k]; if r <= 0 { act = options[k]; break } }
    } else {
        for label in current.tappables where !tried.contains("\(current.sig)|\(label)") {
            act = "tap:\(label)"; break
        }
        if act == nil { act = "back" }
    }
    guard let a = act else { break }
    emit("FUZZ:ACT \(a)")
    // Named screenshot point (from a replay/prefix script): capture the target
    // window to REPROIT_SHOTS_DIR and print SHOOT:<name>. Sanitize <name> to the
    // contract's [A-Za-z0-9_/-]; not a UI action, so it does not affect stuck.
    if a.hasPrefix("shoot:") {
        let raw = String(a.dropFirst("shoot:".count))
        let name = String(raw.unicodeScalars.filter {
            CharacterSet(charactersIn: "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_/-").contains($0)
        })
        if !name.isEmpty { shoot(name, nsApp.processIdentifier, appEl) }
        i += 1
        continue
    }
    if a == "back" {
        // Non-hijacking "back": press an in-app Back/Close via AXPress (no
        // global input, no cursor move), so the runner does not take over the
        // host keyboard. Only fall back to a synthetic Escape if the operator
        // opts in (REPROIT_ALLOW_KEYS=1), e.g. on a dedicated test agent.
        let backLabels: Set<String> = ["Back", "Close", "Done", "Cancel", "OK", "‹", "×"]
        var didBack = false
        for (lbl, el) in current.nodeByLabel where backLabels.contains(lbl) {
            if AXUIElementPerformAction(el, kAXPressAction as CFString) == .success { didBack = true; break }
        }
        if !didBack && ProcessInfo.processInfo.environment["REPROIT_ALLOW_KEYS"] == "1" {
            pressKey(53)
            didBack = true
        }
        if !didBack { stuck += 1; i += 1; continue }
        Thread.sleep(forTimeInterval: 0.6)
        let next = observe()
        // Layer-1 effect detection (docs/signature.md "Value-state"): an action
        // is EFFECTIVE iff the (effective) signature changed OR the content
        // fingerprint changed; a value-only change (a counter ticking) still
        // counts, so a value-state app does not stall to a single dead state.
        if next.sig != current.sig {
            emitEdge(current.sig, "back", next.sig); stuck = 0
        } else if next.content != current.content {
            stuck = 0 // effective (value changed) but same node: keep exploring
        } else {
            stuck += 1
        }
        current = next
        i += 1
        continue
    }
    let label = String(a.dropFirst("tap:".count))
    tried.insert("\(current.sig)|\(label)")
    if let el = current.nodeByLabel[label] {
        let status = AXUIElementPerformAction(el, kAXPressAction as CFString)
        if status == .success {
            Thread.sleep(forTimeInterval: 0.7)
        } else {
            // The press did not succeed. Before treating this as a benign miss,
            // rule out a synchronous in-press crash: the control may have torn
            // the app down DURING the press, which is exactly what surfaces as a
            // non-success status. Check the real process / running-app state (and
            // the AX status) so we do not silently swallow a crash as a MISS.
            if targetIsDead(nsApp, appEl, status) {
                crashBlock("target terminated", "the app process exited during \(a)")
                failed = true
                break
            }
            // App is alive; the element simply was not actionable. Genuine miss.
            emit("FUZZ:MISS \(a)"); stuck += 1; i += 1; continue
        }
    } else {
        emit("FUZZ:MISS \(a)"); stuck += 1; i += 1; continue
    }
    // Successful press: the app may still have died just after it (async crash).
    if targetIsDead(nsApp, appEl, .success) {
        crashBlock("target terminated", "the app process exited during \(a)")
        failed = true
        break
    }
    let next = observe()
    // Layer-1 effect detection: an effective action (signature OR content
    // fingerprint changed) resets the stall counter; only a true no-op (a dead
    // key, a disabled control) leaves both unchanged. A value-only change emits
    // no edge (same node) but still counts as progress.
    if next.sig != current.sig {
        emitEdge(current.sig, "tap:\(label)", next.sig); stuck = 0
    } else if next.content != current.content {
        stuck = 0
    }
    current = next
    i += 1
}
emit("JOURNEY[a] step: explored \(seen.count) states")
emit("JOURNEY DONE")
emit(failed ? "Some tests failed" : "All tests passed")
