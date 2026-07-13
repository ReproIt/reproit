// ReproIt macOS desktop runner (AXUIElement backend).
//
// Drives ANY native macOS app (AppKit, SwiftUI, and Qt / GTK / wxWidgets /
// Avalonia builds, which all publish to the same accessibility API) through
// the system AX tree, and prints the framework-agnostic marker protocol that
// `reproit` parses. Same contract as runners/web/runner.mjs and explorer.dart:
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
    var configured: Bool = false
    var replay: [String]?
    var prefix: [String]?
    var edgeWeights: [String: [String: Int]] = [:]
    // --record clip plan (replay mode only). When present AND REPROIT_VIDEO_DIR is
    // set, the runner films the target window for the whole replay and, after it
    // settles, resolves the finding's element to a window-relative rect + a time
    // window, writing box-spec.json next to clip.mov so the host box-overlay step
    // draws the finding box uniformly (same contract as every non-DOM backend).
    var clipSel: String?     // element label to box (the finding's control/option)
    var clipLabel: String?   // caption text drawn on the box
    var clipOracle: String?  // oracle id, echoed back on the FINDING:BOXED marker
}

func loadFuzz() -> FuzzCfg {
    var c = FuzzCfg()
    guard let p = ProcessInfo.processInfo.environment["REPROIT_FUZZ_CONFIG"], !p.isEmpty,
          let data = FileManager.default.contents(atPath: p),
          let j = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
    else { return c }
    c.configured = true
    if let s = j["seed"] as? NSNumber { c.seed = UInt32(truncatingIfNeeded: s.intValue) }
    if let b = j["budget"] as? NSNumber { c.budget = b.intValue }
    c.replay = j["replay"] as? [String]
    c.prefix = j["prefix"] as? [String]
    if let ew = j["edgeWeights"] as? [String: [String: Int]] { c.edgeWeights = ew }
    if let clip = j["clip"] as? [String: Any] {
        c.clipSel = clip["sel"] as? String
        c.clipLabel = clip["label"] as? String
        c.clipOracle = clip["oracle"] as? String
    }
    return c
}

func edgeKey(_ sig: String, _ action: String) -> String { "\(sig)|\(action)" }

func rememberActions(_ actionsByState: inout [String: [String]], _ sig: String, _ actions: [String]) {
    var known = actionsByState[sig] ?? []
    for action in actions where !known.contains(action) { known.append(action) }
    actionsByState[sig] = known
}

func firstUntriedAction(_ actionsByState: [String: [String]], _ tried: Set<String>, _ sig: String) -> String? {
    for action in actionsByState[sig] ?? [] {
        if !tried.contains(edgeKey(sig, action)) { return action }
    }
    return nil
}

func hasFrontier(_ actionsByState: [String: [String]], _ tried: Set<String>) -> Bool {
    actionsByState.keys.contains { firstUntriedAction(actionsByState, tried, $0) != nil }
}

func rememberEdge(_ graph: inout [String: [(String, String)]], _ from: String, _ action: String, _ to: String) {
    var edges = graph[from] ?? []
    if !edges.contains(where: { $0.0 == action && $0.1 == to }) {
        edges.append((action, to))
    }
    graph[from] = edges
}

func pathToFrontier(_ graph: [String: [(String, String)]], _ actionsByState: [String: [String]], _ tried: Set<String>, _ start: String) -> [String]? {
    if firstUntriedAction(actionsByState, tried, start) != nil { return [] }
    var seen: Set<String> = [start]
    var q: [(String, [String])] = [(start, [])]
    var idx = 0
    while idx < q.count {
        let (sig, path) = q[idx]
        idx += 1
        for (action, to) in graph[sig] ?? [] {
            if seen.contains(to) { continue }
            seen.insert(to)
            let nextPath = path + [action]
            if firstUntriedAction(actionsByState, tried, to) != nil { return nextPath }
            q.append((to, nextPath))
        }
    }
    return nil
}

// ---- Layer 3 opt-in: value_nodes from reproit.yaml ----------------------
// Read the `value_nodes:` selector list from reproit.yaml (docs/signature.md
// "Value-state"), marking EXTRA nodes value-bearing even when their role is not
// in the value-role set. No YAML dependency: the block is a flat list of
// strings, so a tiny line parser is enough. Path precedence: REPROIT_CONFIG env,
// else ./reproit.yaml in the cwd. A missing/unparseable file yields an empty
// list, so value-state is strictly opt-in. Same grammar as the
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

// A STABLE, locale-invariant key for an offending node, mirroring the web
// runner's keyOf grammar: AXIdentifier (the test-id analogue) when present, else
// role-typed. NEVER the visible text, so a translated label keeps the same
// finding id. Same node always yields the same key, so CONTENTBUG
// findings reproduce byte-for-byte across runs and on replay.
func axKeyOf(_ el: AXUIElement, _ role: String) -> String {
    if let id = axIdentifierOf(el) { return "id:" + id }
    return "role:" + role
}

// The screen-coordinate frame (AXPosition + AXSize) of an element, the SAME
// attributes the screenshot path already reads (targetWindowFrame). Returns nil
// when either is unavailable, so a node with no geometry is simply skipped (no
// false positive). Pure structural measurement, no pixels.
func axFrameOf(_ el: AXUIElement) -> CGRect? {
    guard let posV = axCopy(el, kAXPositionAttribute as String),
          let sizeV = axCopy(el, kAXSizeAttribute as String) else { return nil }
    var origin = CGPoint.zero
    var size = CGSize.zero
    AXValueGetValue(posV as! AXValue, .cgPoint, &origin)
    AXValueGetValue(sizeV as! AXValue, .cgSize, &size)
    if size.width < 1 || size.height < 1 { return nil }
    return CGRect(origin: origin, size: size)
}

// ---- CONTENT-BUG oracle (deterministic, label-based) --------------------
// Mirrors runners/web/runner.mjs detectContentBugs: a rendered label that is
// clearly broken CONTENT (a stringify/template artifact leaked to the screen).
// Each classifier is a pure substring/structure test over the trimmed label, so
// the same a11y tree yields the same finding every run and on replay. The match
// is on STRUCTURE (a literal artifact token), never natural language, so a real
// label that merely mentions "null" in prose is not flagged: the token must BE
// the artifact (whole-word undefined/null/NaN, the bracketed literal). A clean
// app renders none of these, so the control stays silent (no marker, no finding).
// Order is fixed and first match wins, so a label carries at most one reason.
func contentBugReason(_ text: String) -> String? {
    if text.isEmpty { return nil }
    if text.contains("[object Object]") { return "object-object" }
    // An unrendered template placeholder: a `{{ expr }}` or `${ expr }` survived
    // into the label (the binding engine never evaluated it).
    if text.range(of: "\\{\\{[^}]*\\}\\}", options: .regularExpression) != nil
        || text.range(of: "\\$\\{[^}]*\\}", options: .regularExpression) != nil {
        return "unrendered-template"
    }
    // A bare value coerced into the label as a WHOLE word. The surrounding-char
    // guards match the web runner so ordinary prose ("Cancellation", "Null
    // Island") is not flagged: the token must stand alone.
    if text.range(of: "(^|[\\s:>(\\[,])undefined($|[\\s.,!?)\\]<])", options: .regularExpression) != nil {
        return "undefined"
    }
    if text.range(of: "(^|[\\s:>(\\[,])null($|[\\s.,!?)\\]<])", options: .regularExpression) != nil {
        return "null"
    }
    if text.range(of: "(^|[\\s:>(\\[,])NaN($|[\\s.,!?)\\]<])", options: .regularExpression) != nil {
        return "nan"
    }
    return nil
}

// ---- BROKEN-ASSET oracle (tofu: rendered U+FFFD) -------------------------
// Mirrors the tofu class of runners/web/hygiene-oracles.mjs brokenAssetScan: a
// rendered U+FFFD replacement character in an element's title/description/value
// is broken text encoding reaching the screen. U+FFFD is what a decoder emits
// on malformed input, never a glyph an app renders on purpose, so the test is
// a pure substring check with no false positives. AX exposes no image pixel
// status and no font load status, so tofu is the only broken-asset class with
// AX ground truth here (the img/font classes stay web-only). Returns a short
// clipped excerpt around the first U+FFFD (the human detail; the stable node
// key is the finding identity), or nil when no replacement char is rendered.
func tofuExcerpt(_ text: String) -> String? {
    guard let hit = text.firstIndex(of: "\u{FFFD}") else { return nil }
    let start = text.index(hit, offsetBy: -20, limitedBy: text.startIndex) ?? text.startIndex
    let end = text.index(hit, offsetBy: 21, limitedBy: text.endIndex) ?? text.endIndex
    return String(text[start..<end]).trimmingCharacters(in: .whitespacesAndNewlines)
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
    var elements: [[String: Any]]
    var tappables: [String]
    var nodeByLabel: [String: AXUIElement]
    // CONTENT-BUG items: a label carrying a stringify/template artifact.
    var contentBugs: [(key: String, reason: String, text: String)] = []
    // BROKEN-ASSET items: a rendered U+FFFD (tofu) in a label or live value.
    var brokenAssets: [(key: String, detail: String)] = []
}


func snapshot(_ app: AXUIElement, _ valueNodeSelectors: [String]) -> Snapshot {
    var labels: [String] = []
    var tappables: [String] = []
    var elements: [[String: Any]] = []
    var nodeByLabel: [String: AXUIElement] = [:]
    // Oracle accumulators, filled during the single canonical tree walk below.
    var contentBugs: [(String, String, String)] = []
    var contentBugSeen = Set<String>()
    var brokenAssets: [(String, String)] = []
    var brokenAssetSeen = Set<String>()
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
    func build(_ el: AXUIElement, _ depth: Int, isRoot: Bool,
               roleCounter: inout [String: Int]) -> SigNode? {
        if depth > 60 { return nil }
        let role = isRoot ? "screen" : axRoleOf(el)
        let id = axIdentifierOf(el)
        let actionable = axActions(el).contains(kAXPressAction as String)
        let label = labelOf(el).trimmingCharacters(in: .whitespacesAndNewlines)
        if role == "textfield", let id = id, !id.isEmpty {
            let sel = "key:\(id)"
            var purpose: String? = nil
            if let r = id.range(of: "reproit-purpose-") {
                purpose = String(id[r.upperBound...].split(separator: "--", maxSplits: 1)[0])
            } else if axTypeOf(el, role) == "password" {
                purpose = "password"
            }
            var item: [String: Any] = ["sel": sel, "role": role, "label": label]
            if let purpose = purpose { item["inputPurpose"] = purpose }
            elements.append(item)
        }
        if !label.isEmpty && label.count <= maxLabelLen {
            labels.append(label)
            if actionable {
                tappables.append(label)
                if nodeByLabel[label] == nil { nodeByLabel[label] = el }
            }
        }
        // CONTENT-BUG oracle: scan this element's label for a stringify/template
        // artifact. Keyed by the stable node key + reason, deduped, so the marker
        // is byte-identical run to run and addressed by id/role, never the text.
        if !label.isEmpty, let reason = contentBugReason(label) {
            let key = axKeyOf(el, role)
            let dedup = key + "|" + reason
            if !contentBugSeen.contains(dedup) {
                contentBugSeen.insert(dedup)
                contentBugs.append((key, reason, String(label.prefix(80))))
            }
        }
        // Layer 2/3 value detection. A value-bearing node (an AX value role with a
        // live AXValue, or a Layer-3 opt-in selector match) carries its value + the
        // value_node flag so the oracle folds a bounded value-class into V:. A
        // value-bearing node WINS over the transient heuristic.
        let optIn = !isRoot && matchesValueNodeAX(
            el, id: id, role: role, selectors: valueNodeSelectors, roleIndexTargets: roleIndexTargets)
        let valueBearing = !isRoot && (axIsValueBearing(el) || optIn)
        let value: String? = valueBearing ? axValueOf(el) : nil
        // BROKEN-ASSET (tofu) oracle: a rendered U+FFFD in this element's label
        // (title > description > value) or live AXValue is broken text encoding
        // on screen. Keyed by the stable node key, deduped, so the marker is
        // byte-identical run to run and addressed by id/role, never the text.
        if let excerpt = tofuExcerpt(label) ?? value.flatMap(tofuExcerpt) {
            let key = axKeyOf(el, role)
            if !brokenAssetSeen.contains(key) {
                brokenAssetSeen.insert(key)
                brokenAssets.append((key, excerpt))
            }
        }
        if valueBearing {
            let fkey = id != nil ? "key:\(id!)" : "role:\(normalizeRole(role))"
            textNodes.append((fkey, value ?? ""))
        }
        let transient = !isRoot && !valueBearing && axIsTransient(el, role)

        var kids: [SigNode] = []
        for c in axChildren(el) {
            if let n = build(c, depth + 1, isRoot: false,
                             roleCounter: &roleCounter) { kids.append(n) }
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
    for w in windows {
        windowKids.append(contentsOf: axChildren(w))
    }
    if needRoleResolution { resolveRoleTargets(windowKids) }

    var rootKids: [SigNode] = []
    var roleCounter: [String: Int] = [:]
    for c in windowKids {
        // Each window's own children become the screen's children; the window
        // chrome itself is not a separate structural level.
        if let n = build(c, 1, isRoot: false,
                         roleCounter: &roleCounter) { rootKids.append(n) }
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

    // Stable order so the OVERFLOW/CONTENTBUG markers are byte-identical run to
    // run (the finding id keys off key+kind/reason, never walk order).
    contentBugs.sort { $0.0 != $1.0 ? $0.0 < $1.0 : $0.1 < $1.1 }
    brokenAssets.sort { $0.0 < $1.0 }

    return Snapshot(
        sig: sig,
        structuralSig: structuralSig,
        vsection: vsection,
        content: content,
        labels: Array(Set(labels)),
        elements: elements,
        tappables: Array(Set(tappables)),
        nodeByLabel: nodeByLabel,
        contentBugs: contentBugs.map { (key: $0.0, reason: $0.1, text: $0.2) },
        brokenAssets: brokenAssets.map { (key: $0.0, detail: $0.1) }
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
    // The current contract ships 25 golden vectors (structural + value-state).
    // Assert ALL of them are present, so a truncated vectors file fails the gate.
    let expectedCount = 25
    if arr.count != expectedCount {
        ok = false
        FileHandle.standardError.write(
            "selftest FAIL: expected \(expectedCount) vectors, got \(arr.count)\n".data(using: .utf8)!)
    }
    // Spot-check the value-state relationships the spec promises (Layer 2), so a
    // value-class regression is caught even if a golden hash were updated wrong.
    if !runValueStateChecks() { ok = false }
    // Spot-check the tofu scan (the BROKEN-ASSET oracle's pure text test), so
    // the selftest gates it without a live app or Accessibility permission.
    if !runTofuChecks() { ok = false }
    if !runInvariantChecks() { ok = false }
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

// Assert the tofu-excerpt scan (BROKEN-ASSET oracle) both directions: clean
// text is silent, a rendered U+FFFD flags with a clipped excerpt around the
// char. Returns true on success. Logs the first failure to stderr.
func runTofuChecks() -> Bool {
    var ok = true
    func check(_ cond: Bool, _ msg: String) {
        if !cond { ok = false; FileHandle.standardError.write("selftest tofu FAIL: \(msg)\n".data(using: .utf8)!) }
    }
    // Clean labels never flag: no U+FFFD, no finding, however odd the text.
    check(tofuExcerpt("") == nil, "empty text must not flag")
    check(tofuExcerpt("Save changes") == nil, "plain text must not flag")
    check(tofuExcerpt("caf\u{e9} \u{4f60}\u{597d} \u{1f600}") == nil, "non-ASCII text is not tofu")
    // A rendered replacement char flags, keeping context around the char.
    check(tofuExcerpt("glitch \u{FFFD} here") == "glitch \u{FFFD} here", "short text keeps full context")
    // Long text clips to a bounded excerpt that still shows the char.
    let long = String(repeating: "a", count: 60) + "\u{FFFD}" + String(repeating: "b", count: 60)
    if let ex = tofuExcerpt(long) {
        check(ex.count <= 41 && ex.contains("\u{FFFD}"), "long text must clip around the char")
    } else {
        check(false, "long tofu text must flag")
    }
    return ok
}

// Assert the app-invariant marker parse (the pure text half of the EXPLORE:INVARIANT
// scrape) both directions: a well-formed marker yields its sig + the violated
// (id, message) pairs; a non-marker line, malformed json, and an empty item list
// are all silent. The live NSWorkspace-launch + file-scrape path is exercised by
// the operability CI job. Returns true on success.
func runInvariantChecks() -> Bool {
    var ok = true
    func check(_ cond: Bool, _ msg: String) {
        if !cond { ok = false; FileHandle.standardError.write("selftest invariant FAIL: \(msg)\n".data(using: .utf8)!) }
    }
    if let (sig, items) = parseInvariantMarker(
        "REPROIT_INVARIANT {\"sig\":\"s1\",\"items\":[{\"id\":\"total\",\"message\":\"NaN\"}]}") {
        check(sig == "s1", "marker carries the SDK sig")
        check(items.count == 1 && items[0]["id"] == "total" && items[0]["message"] == "NaN",
              "marker carries the violated id + message")
    } else {
        check(false, "a well-formed marker must parse")
    }
    check(parseInvariantMarker("ordinary log line") == nil, "a non-marker line is silent")
    check(parseInvariantMarker("REPROIT_INVARIANT {oops") == nil, "malformed json is silent")
    check(parseInvariantMarker("REPROIT_INVARIANT {\"items\":[]}") == nil, "an empty item list is silent")
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

// ---- LEAK sampler (MEMORY:SAMPLE, --soak) -------------------------------
// Under the soak tier (a replay script) we sample the target's resident set size
// (RSS) once per replay cycle so the Rust soak oracle (modes/soak.rs) gets an
// RSS-vs-time series and reads the slope. RSS is the native analogue of the web
// runner's v8 heap_used; the marker shape is IDENTICAL ({"t_ms","heap_used"}) so
// soak.rs parses it unchanged (heap_used carries RSS bytes). `ps -o rss=` reports
// KiB on macOS, so multiply to bytes. No measurement is taken outside replay (a
// plain fuzz walk is not a soak), matching the web runner.
func sampleRSS(_ pid: pid_t, _ tMs: Int) {
    let proc = Process()
    proc.executableURL = URL(fileURLWithPath: "/bin/ps")
    proc.arguments = ["-o", "rss=", "-p", "\(pid)"]
    let pipe = Pipe()
    proc.standardOutput = pipe
    proc.standardError = Pipe()
    do { try proc.run() } catch { return }
    proc.waitUntilExit()
    let data = pipe.fileHandleForReading.readDataToEndOfFile()
    guard let out = String(data: data, encoding: .utf8)?.trimmingCharacters(in: .whitespacesAndNewlines),
          let kib = Int(out) else { return }
    emitJSON("MEMORY:SAMPLE", ["t_ms": tMs, "heap_used": kib * 1024])
}

// ---- HANG watchdog (EXPLORE:HANG) ---------------------------------------
// A deterministic wall-clock watchdog around each action+observe. macOS AX has no
// main-thread Long-Tasks trace (the web runner's signal), so we can only time the
// blocking AXUIElementPerformAction round trip from THIS process: AX calls are
// synchronous and block until the target's main run loop services them, so an app
// that froze its main thread makes the press/observe wall time spike. We bucket
// into coarse, well-separated floors so timing jitter can never flip the verdict,
// matching the web runner's HANG_FLOOR_MS. CAVEAT (documented gap): unlike the
// web Long-Tasks API this is host-side wall time, so it can be perturbed by host
// scheduling; the high floor keeps it false-positive-free but the duration is not
// as deterministic as a frame trace. Keyed by (from, action) like the web HANG.
let hangFloorMs = 2000
func maybeEmitHang(_ from: String, _ action: String, _ elapsedMs: Int) {
    if elapsedMs >= hangFloorMs {
        emitJSON("EXPLORE:HANG", ["from": from, "action": action, "bucket": hangFloorMs])
    }
}

// ---- screenshot capture (SHOOT contract, see crates/.../backends/drive.rs) --
// The orchestrator passes REPROIT_SHOTS_DIR (absolute) and, on a named shoot
// point, expects <dir>/<name>.png to exist before it sees `SHOOT:<name>` on
// stdout. <name> is [A-Za-z0-9_/-]. If REPROIT_SHOTS_DIR is unset we still print
// the marker (capture is best-effort, the orchestrator just logs a miss).

// The target app's PRIMARY on-screen window: the LARGEST-area window owned by the
// pid, returned as (CGWindowID, screen bounds in points, top-left origin). We pick
// by AREA, not CGWindowList order, because a running `screencapture -v` injects a
// small "screen sharing session" indicator window UNDER the app's pid while it
// records; that helper (~66x20) sorts BEFORE the real content window, so taking
// the first match would mis-size the clip and mis-place the box. Selecting the
// largest window skips it and keeps the capture (-l <id>) and the box-spec
// (videoW/H + window-relative rect) locked to exactly one window. `screencapture
// -l <id>` then films that window (chrome + shadow), never the whole desktop.
func targetPrimaryWindow(_ pid: pid_t) -> (id: CGWindowID, bounds: CGRect)? {
    let opts: CGWindowListOption = [.optionOnScreenOnly, .excludeDesktopElements]
    guard let infos = CGWindowListCopyWindowInfo(opts, kCGNullWindowID) as? [[String: Any]] else {
        return nil
    }
    var best: (id: CGWindowID, bounds: CGRect)? = nil
    for info in infos {
        guard let owner = info[kCGWindowOwnerPID as String] as? pid_t, owner == pid,
              let num = info[kCGWindowNumber as String] as? CGWindowID,
              let b = info[kCGWindowBounds as String] as? [String: Any],
              let x = b["X"] as? CGFloat, let y = b["Y"] as? CGFloat,
              let w = b["Width"] as? CGFloat, let h = b["Height"] as? CGFloat,
              w >= 1, h >= 1 else { continue }
        let rect = CGRect(x: x, y: y, width: w, height: h)
        if best == nil || rect.width * rect.height > best!.bounds.width * best!.bounds.height {
            best = (id: num, bounds: rect)
        }
    }
    return best
}

// The CGWindowID of the target app's primary window (see targetPrimaryWindow).
func targetWindowID(_ pid: pid_t) -> CGWindowID? { targetPrimaryWindow(pid)?.id }

// The screen-coordinate bounds (top-left origin, points) of the SAME window that
// targetWindowID(pid) returns -- i.e. exactly the rect `screencapture -l` frames.
// Used to size the clip video's logical space and to make a captured element's
// screen rect window-relative; CGWindowBounds and AXPosition share the top-left
// screen convention, so `element - bounds.origin` lands the box correctly.
func targetWindowBounds(_ pid: pid_t) -> CGRect? { targetPrimaryWindow(pid)?.bounds }

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

// --record clip capture. Films ONLY the target window (never the desktop, a hard
// privacy rule) for the duration of a replay, using `screencapture -v -l <id>`.
// Returns the still-running Process handle; stopClipCapture() sends SIGINT so
// screencapture finalizes the .mov, exactly as a Control-C would. A window video
// (points, top-left origin) pairs with a window-relative box rect, so the host
// box-overlay step draws the finding box in the same coordinate space.
func startClipCapture(_ pid: pid_t, _ outMov: String) -> Process? {
    guard let wid = targetWindowID(pid) else { return nil }
    try? FileManager.default.createDirectory(
        at: URL(fileURLWithPath: outMov).deletingLastPathComponent(),
        withIntermediateDirectories: true)
    let proc = Process()
    proc.executableURL = URL(fileURLWithPath: "/usr/sbin/screencapture")
    // -x: silent. -v: video. -o: OMIT the window shadow, so the captured frame is
    // exactly the window's content rect (no drop-shadow border). This makes the
    // clip's pixel space equal the window's own logical space (a 1:1 map on a 1x
    // display, a clean 2x on retina), so box-overlay's linear logical->pixel scale
    // lands the box precisely; WITH the shadow the frame is larger and offset
    // (asymmetric bottom shadow), which pushes the drawn box off the element.
    // -l <id>: just that window. Records until SIGINT.
    proc.arguments = ["-x", "-v", "-o", "-l", "\(wid)", outMov]
    do { try proc.run() } catch { return nil }
    return proc
}

func stopClipCapture(_ proc: Process?) {
    guard let proc = proc, proc.isRunning else { return }
    proc.interrupt() // SIGINT -> screencapture flushes and closes the .mov
    proc.waitUntilExit()
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
    // 3) Is the bundle id / app still running under this pid? Query it FRESH via
    //    NSRunningApplication.runningApplications(withBundleIdentifier:) (the same
    //    lookup runningApp() trusts), NOT NSWorkspace.shared.runningApplications:
    //    that cached list is only refreshed by main-run-loop notifications this
    //    synchronous runner never pumps, so for an app the runner LAUNCHED itself
    //    (as opposed to attached to an already-running one) it can be empty even
    //    while the process is alive and AX-reachable, misfiring as a false
    //    "target terminated" on the very first action. The fresh query reflects
    //    the live process table directly, independent of the run loop.
    if let bid = app.bundleIdentifier {
        let stillListed = NSRunningApplication
            .runningApplications(withBundleIdentifier: bid)
            .contains { $0.processIdentifier == app.processIdentifier }
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
    // Hand the app the invariant marker file path (and the fuzzer-detection gate).
    // OpenConfiguration.environment REPLACES the app's environment, so start from
    // our own and add the one variable; the SDK writes REPROIT_INVARIANT markers
    // there when it sees this var, and the runner scrapes the file.
    var appEnv = ProcessInfo.processInfo.environment
    appEnv["REPROIT_INVARIANT_FILE"] = invariantMarkerPath
    cfg.environment = appEnv
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

/// Bring the app's window(s) back ONTO the visible display so a --record clip
/// captures REAL pixels. `screencapture -v` records the live display stream, so a
/// window sitting off-screen -- whether from moveOffscreen (the privacy default)
/// or an AppKit-autosaved off-screen frame left by a prior run -- films as solid
/// black even though a still `-l` capture of its backing store would succeed.
/// Recording a finding clip inherently needs the window shown, so the clip path
/// overrides the off-screen move for its duration, parking the window at a fixed
/// on-screen origin near the top-left of the main display.
func moveOnscreen(_ app: AXUIElement) {
    guard let windows = axCopy(app, kAXWindowsAttribute as String) as? [AXUIElement] else { return }
    var pt = CGPoint(x: 60, y: 60)
    guard let value = AXValueCreate(.cgPoint, &pt) else { return }
    for w in windows {
        AXUIElementSetAttributeValue(w, kAXPositionAttribute as CFString, value)
    }
}

// ---- multi-actor scenario client (the conductor protocol) ----------------
//
// The host conductor (modes/barrier.rs) owns identity and ordering for an
// authored multi-user scenario; this runner only has to speak three verbs over
// localhost HTTP and execute one action at a time:
//   GET  /claim               -> role letter (`a`, `b`, ...) | `ERR full`
//   GET  /next?device=<role>  -> `WAIT` | `ACT\t<action>` | `DONE`
//   POST /done?device=<role>  -> `OK`
// Same client the web/electron/tauri/rn runners, the flutter explorer and the
// tui backend implement; only the action execution is AX-specific. Each actor
// drives its OWN app instance (see launchNewInstance), and the conductor
// serializes actions globally (one ACT outstanding at a time), so a shared
// desktop session needs no input isolation: the actor just brings its own
// window forward before acting.

/// One blocking HTTP exchange with the conductor. URLSession + a semaphore
/// keeps the synchronous runner free of an async runtime.
func barrierHit(_ base: String, _ method: String, _ path: String) -> String? {
    guard let url = URL(string: base + path) else { return nil }
    var req = URLRequest(url: url)
    req.httpMethod = method
    req.timeoutInterval = 10
    let sem = DispatchSemaphore(value: 0)
    var body: String?
    URLSession.shared.dataTask(with: req) { data, _, _ in
        if let d = data {
            body = String(data: d, encoding: .utf8)?
                .trimmingCharacters(in: .whitespacesAndNewlines)
        }
        sem.signal()
    }.resume()
    _ = sem.wait(timeout: .now() + 12)
    return body
}

/// JSON-quote a string (for the FUZZ:ASSERT text=… marker, which carries the
/// wanted text as a JSON string like every other runner).
func jsonQuote(_ s: String) -> String {
    if let d = try? JSONSerialization.data(withJSONObject: [s]),
       let arr = String(data: d, encoding: .utf8) {
        return String(arr.dropFirst().dropLast())
    }
    return "\"\(s)\""
}

/// Launch a FRESH app instance for this actor. Two scenario actors on the same
/// target must never share a process (launch() would attach both to the first
/// instance): an executable path is spawned as our own child, a bundle id is
/// opened with createsNewApplicationInstance so every actor gets its own pid.
func launchNewInstance(_ target: String) -> NSRunningApplication? {
    if FileManager.default.isExecutableFile(atPath: target) && target.contains("/") {
        let proc = Process()
        proc.executableURL = URL(fileURLWithPath: target)
        var childEnv = ProcessInfo.processInfo.environment
        childEnv["REPROIT_INVARIANT_FILE"] = invariantMarkerPath
        proc.environment = childEnv
        do { try proc.run() } catch { return nil }
        // Wait for the child to register with Launch Services (an NSApplication
        // process registers once its run loop starts).
        for _ in 0..<40 {
            if let a = NSRunningApplication(processIdentifier: proc.processIdentifier) { return a }
            Thread.sleep(forTimeInterval: 0.25)
        }
        return nil
    }
    guard let url = NSWorkspace.shared.urlForApplication(withBundleIdentifier: target) else {
        // App-name targets have no by-name "new instance" API; fall back to
        // attach (a single-actor scenario still works).
        return launch(target)
    }
    let cfg = NSWorkspace.OpenConfiguration()
    cfg.activates = ProcessInfo.processInfo.environment["REPROIT_MAC_ACTIVATE"] != "0"
    cfg.createsNewApplicationInstance = true
    let sem = DispatchSemaphore(value: 0)
    var result: NSRunningApplication?
    NSWorkspace.shared.openApplication(at: url, configuration: cfg) { app, _ in
        result = app; sem.signal()
    }
    _ = sem.wait(timeout: .now() + 12)
    return result
}

/// Find an element to type into: an AXIdentifier or label match that carries a
/// settable AXValue (text field / text area / combo box / search field). The
/// journey finder may arrive with the cross-surface `key:` prefix; both forms
/// match the identifier.
func axFindTypable(_ app: AXUIElement, _ finder: String) -> AXUIElement? {
    let want = finder.hasPrefix("key:") ? String(finder.dropFirst(4)) : finder
    var found: AXUIElement?
    func walk(_ el: AXUIElement, _ depth: Int) {
        if found != nil || depth > 60 { return }
        if axHasValueAttribute(el) {
            let id = axIdentifierOf(el) ?? ""
            let label = labelOf(el).trimmingCharacters(in: .whitespacesAndNewlines)
            if (!id.isEmpty && (id == want || id == finder)) || (!label.isEmpty && label == want) {
                var settable = DarwinBoolean(false)
                if AXUIElementIsAttributeSettable(el, kAXValueAttribute as CFString, &settable) == .success,
                   settable.boolValue {
                    found = el
                    return
                }
            }
        }
        for c in axChildren(el) { walk(c, depth + 1) }
    }
    let windows = (axCopy(app, kAXWindowsAttribute as String) as? [AXUIElement]) ?? []
    for w in windows { walk(w, 1) }
    return found
}

/// Play ONE actor of a multi-user scenario: pull this actor's actions from the
/// conductor, execute each against this actor's own app instance, and ack
/// completion, so N runner processes interleave exactly as the journey
/// specifies. The AX action vocabulary:
///   tap:<label>           AXPress the actionable element with that label
///   type:<finder>=<v>     set the AXValue of the id/label-matched text field
///   back                  press an in-app Back/Close control (synthetic Esc
///                         only with REPROIT_ALLOW_KEYS=1, as in fuzzing)
///   shoot:<name>          screenshot point (same contract as replay)
///   assert:text=<t>       the visible labels contain <t>
///   assert:count:<f>=<n>  the visible labels contain <f> exactly <n> times
///   auth:<acct>           unsupported (no session store to restore); loud
///                         no-op so ordering still advances
/// Crash detection is the same oracle as fuzzing (targetIsDead); a crashed
/// actor deliberately does NOT ack its step, so the conductor's diagnose()
/// names this actor and action as the stall point. Returns the failure flag.
func runScenarioActor(
    _ base: String, _ nsApp: NSRunningApplication, _ appEl: AXUIElement,
    _ valueNodeSelectors: [String]
) -> Bool {
    let procEnv = ProcessInfo.processInfo.environment
    var role = procEnv["REPROIT_DEVICE"] ?? ""
    if role.isEmpty {
        if let r = barrierHit(base, "GET", "/claim"), !r.isEmpty, !r.hasPrefix("ERR") {
            role = r
        } else {
            role = "a"
        }
    }
    emit("JOURNEY claimed role=\(role)")
    Thread.sleep(forTimeInterval: 0.9)

    var seen = Set<String>()
    var failed = false
    // Scenario-side twin of the fuzz loop's observe(): states a scenario
    // reaches (often only reachable with a peer acting) still land in the map.
    func observeScenario() -> Snapshot {
        let snap = snapshot(appEl, valueNodeSelectors)
        emitJSON("FUZZ:OBS", [
            "sig": snap.sig,
            "labels": Array(snap.labels.prefix(maxLabelsPerState)),
            "elements": snap.elements,
        ])
        if seen.insert(snap.sig).inserted {
            emitJSON("EXPLORE:STATE", [
                "sig": snap.sig,
                "labels": Array(snap.labels.prefix(maxLabelsPerState)),
                "elements": snap.elements,
            ])
        }
        return snap
    }
    var cur = observeScenario()

    actor: for _ in 0..<100_000 {
        guard let body = barrierHit(base, "GET", "/next?device=\(role)") else {
            Thread.sleep(forTimeInterval: 0.1)
            continue
        }
        if body == "DONE" { break }
        if body == "WAIT" {
            Thread.sleep(forTimeInterval: 0.04)
            continue
        }
        let act = body.hasPrefix("ACT\t") ? String(body.dropFirst(4)) : body
        emit("FUZZ:ACT \(role) \(act)")
        // Bring THIS actor's own instance forward before acting. Actions are
        // globally serialized by the conductor (one ACT outstanding at a time),
        // so actors never fight over focus; AXPress/AXValue do not strictly
        // need it, but synthetic keys and user-visible recordings do.
        if procEnv["REPROIT_MAC_ACTIVATE"] != "0" { nsApp.activate() }

        if act.hasPrefix("shoot:") {
            let raw = String(act.dropFirst("shoot:".count))
            let name = String(raw.unicodeScalars.filter {
                CharacterSet(charactersIn: "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_/-").contains($0)
            })
            if !name.isEmpty { shoot(name, nsApp.processIdentifier, appEl) }
        } else if act.hasPrefix("assert:") {
            let a = String(act.dropFirst("assert:".count))
            // Assert against a FRESH snapshot: a peer's action may have changed
            // this device's screen (e.g. an incoming message) since the last
            // observe.
            let contents = snapshot(appEl, valueNodeSelectors).labels.joined(separator: "\n")
            if a.hasPrefix("text=") {
                let want = String(a.dropFirst("text=".count))
                let ok = contents.contains(want)
                emit("FUZZ:ASSERT \(ok ? "pass" : "fail") text=\(jsonQuote(want)) actor=\(role)")
            } else if a.hasPrefix("count:") {
                let rest = String(a.dropFirst("count:".count))
                let eqAt = rest.range(of: "=", options: .backwards)
                let finder = eqAt.map { String(rest[..<$0.lowerBound]) } ?? rest
                let want = eqAt.flatMap { Int(rest[$0.upperBound...]) } ?? 0
                let got = finder.isEmpty ? 0 : contents.components(separatedBy: finder).count - 1
                emit("FUZZ:ASSERT \(got == want ? "pass" : "fail") count \(finder) want=\(want) got=\(got) actor=\(role)")
            } else {
                emit("FUZZ:ASSERT fail unsupported \(a) actor=\(role)")
            }
        } else if act.hasPrefix("auth:") {
            emit("JOURNEY[a] step: auth-restore unsupported on desktop-ax runner; "
                + "drive the login UI explicitly for \(act)")
        } else if act == "back" {
            // Non-hijacking "back", same rules as fuzzing: an in-app Back/Close
            // via AXPress; a synthetic Escape only when the operator opted in.
            let backLabels: Set<String> = ["Back", "Close", "Done", "Cancel", "OK", "‹", "×"]
            var didBack = false
            for (lbl, el) in cur.nodeByLabel where backLabels.contains(lbl) {
                if AXUIElementPerformAction(el, kAXPressAction as CFString) == .success {
                    didBack = true
                    break
                }
            }
            if !didBack && procEnv["REPROIT_ALLOW_KEYS"] == "1" {
                pressKey(53)
                didBack = true
            }
            if !didBack { emit("FUZZ:MISS \(role) back") }
            Thread.sleep(forTimeInterval: 0.6)
        } else if act.hasPrefix("type:") {
            let b = String(act.dropFirst("type:".count))
            let eqAt = b.range(of: "=", options: .backwards)
            let finder = eqAt.map { String(b[..<$0.lowerBound]) } ?? b
            let value = eqAt.map { String(b[$0.upperBound...]) } ?? ""
            var typed = false
            if let el = axFindTypable(appEl, finder) {
                var settableFocus = DarwinBoolean(false)
                if AXUIElementIsAttributeSettable(el, kAXFocusedAttribute as CFString, &settableFocus) == .success,
                   settableFocus.boolValue {
                    AXUIElementSetAttributeValue(el, kAXFocusedAttribute as CFString, kCFBooleanTrue)
                }
                typed = AXUIElementSetAttributeValue(el, kAXValueAttribute as CFString, value as CFTypeRef) == .success
            }
            if !typed { emit("FUZZ:MISS \(role) \(act)") }
            Thread.sleep(forTimeInterval: 0.6)
        } else if act.hasPrefix("tap:") {
            let label = String(act.dropFirst("tap:".count))
            // Resolve against a FRESH snapshot: a peer's action may have moved
            // this device's UI since the last observe.
            let snap = snapshot(appEl, valueNodeSelectors)
            if let el = snap.nodeByLabel[label] {
                let status = AXUIElementPerformAction(el, kAXPressAction as CFString)
                if status == .success {
                    Thread.sleep(forTimeInterval: 0.7)
                } else if targetIsDead(nsApp, appEl, status) {
                    crashBlock("target terminated", "the app process exited during \(act)")
                    failed = true
                    break actor
                } else {
                    emit("FUZZ:MISS \(role) \(act)")
                }
            } else {
                emit("FUZZ:MISS \(role) \(act)")
            }
        } else {
            // A key:<Name> or other cross-surface action authored for a
            // different backend: fail loudly instead of silently passing.
            emit("FUZZ:MISS \(role) \(act)")
        }

        // Crash oracle after every action, same rules as fuzzing. Deliberately
        // no /done ack on a crash, so the conductor names this actor + action.
        if targetIsDead(nsApp, appEl, .success) {
            crashBlock("target terminated", "the app process exited during \(act)")
            failed = true
            break actor
        }
        let next = observeScenario()
        if next.sig != cur.sig {
            emitEdge(cur.sig, act, next.sig)
        }
        cur = next
        _ = barrierHit(base, "POST", "/done?device=\(role)")
    }

    emit("JOURNEY DONE")
    emit(failed ? "Some tests failed" : "All tests passed")
    return failed
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
// Multi-actor scenario: this process plays ONE actor of an authored multi-user
// journey, pulling each action from the host conductor instead of fuzzing.
// Same env contract as the web runner (the orchestrator passes defines as env
// to every non-flutter backend). The claimed-role line is emitted by the
// scenario actor itself (with its real role) on this path.
let scenarioBase = (env["REPROIT_SCENARIO_BARRIER"] ?? "").isEmpty ? nil : env["REPROIT_SCENARIO_BARRIER"]
if scenarioBase == nil { emit("JOURNEY claimed role=a") }
guard AXIsProcessTrusted() else {
    crashBlock("accessibility not trusted",
               "grant Accessibility to this process in System Settings > Privacy & Security")
    exit(3)
}
// App-invariant marker file (per-pid): the launched app's SDK writes its
// REPROIT_INVARIANT markers here (path handed to it in the launch environment,
// which is also the SDK's fuzzer-detection gate); the runner scrapes it after
// each settle. Truncated up front so the first read starts empty. Defined before
// launch() so the launch environment can carry the path.
let invariantMarkerPath = NSTemporaryDirectory()
    + "reproit-invariant-\(ProcessInfo.processInfo.processIdentifier).ndjson"
try? "".write(toFile: invariantMarkerPath, atomically: false, encoding: .utf8)

// A scenario actor must own a FRESH instance (two actors on the same target
// can never share a process); single-actor fuzzing keeps attach-or-launch.
guard let nsApp = (scenarioBase != nil ? launchNewInstance(target) : launch(target)) else {
    crashBlock("target not found", "could not launch \(target)")
    exit(3)
}
if env["REPROIT_MAC_ACTIVATE"] != "0" { nsApp.activate() }
let appEl = AXUIElementCreateApplication(nsApp.processIdentifier)
Thread.sleep(forTimeInterval: 1.2)
moveOffscreen(appEl)
Thread.sleep(forTimeInterval: 0.8)

if let base = scenarioBase {
    // The verdict travels through the markers (JOURNEY DONE + pass/fail lines),
    // same as the fuzz walk; the exit code carries nothing.
    _ = runScenarioActor(base, nsApp, appEl, loadValueNodes())
    // The actor's own instance was launched by us; leave nothing behind.
    nsApp.terminate()
    exit(0)
}

let fuzz = loadFuzz()
let rng = Rng(fuzz.seed)
if fuzz.seed != 0 { emit("JOURNEY[a] step: fuzz seed=\(fuzz.seed)") }

// Layer-3 opt-in value-node selectors from reproit.yaml (empty if none).
let valueNodeSelectors = loadValueNodes()
if !valueNodeSelectors.isEmpty { emit("JOURNEY[a] step: value_nodes=\(valueNodeSelectors.count)") }

var seen = Set<String>()
var tried = Set<String>()
var actionsByState: [String: [String]] = [:]
var graph: [String: [(String, String)]] = [:]
var launchSig: String?

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

// Emit a marker carrying a JSON object payload (helper for the oracle markers).
func emitJSON(_ marker: String, _ payload: [String: Any]) {
    if let d = try? JSONSerialization.data(withJSONObject: payload),
       let s = String(data: d, encoding: .utf8) {
        emit("\(marker) \(s)")
    }
}

// ---- APP-INVARIANT oracle (EXPLORE:INVARIANT, SDK-self-triggered) --------
//
// The app declares its own predicates via the reproit SDK (ReproIt.invariant).
// Under the fuzzer the SDK evaluates them on its state-observe hook and reports
// the FAILURES as a marker line
//   REPROIT_INVARIANT {"sig":"<sig-or-empty>","items":[{"id","message"}...]}
// This runner maps each into the CLI wire line EXPLORE:INVARIANT keyed on the
// signature the runner is CURRENTLY on, de-duped per state.
//
// CHANNEL: a macOS app is launched via NSWorkspace (a GUI activation), which
// gives this runner no stderr pipe (unlike the Linux/Windows runners), so the
// SDK writes markers to a runner-provisioned file whose path is handed to the
// app via the launch environment (REPROIT_INVARIANT_FILE, also the SDK's
// fuzzer-detection gate). The runner scrapes that file after each settle.

// Parse one line for the SDK marker. Returns (sig, items) with items the
// VIOLATED (id, message) pairs and sig the SDK's own signature (empty when
// unknown). nil for a non-marker line, malformed json, or an empty item list.
func parseInvariantMarker(_ line: String) -> (String, [[String: String]])? {
    guard let r = line.range(of: "REPROIT_INVARIANT ") else { return nil }
    let jsonStr = line[r.upperBound...].trimmingCharacters(in: .whitespaces)
    guard let data = jsonStr.data(using: .utf8),
          let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
          let rawItems = obj["items"] as? [[String: Any]] else { return nil }
    var items: [[String: String]] = []
    for it in rawItems {
        guard let id = it["id"] as? String else { continue }
        let message = (it["message"] as? String) ?? ""
        items.append(["id": id, "message": message])
    }
    if items.isEmpty { return nil }
    let sig = (obj["sig"] as? String) ?? ""
    return (sig, items)
}

// Scrapes the runner-provisioned marker file for REPROIT_INVARIANT lines and
// re-emits each as EXPLORE:INVARIANT keyed on the runner's current sig. The SDK
// and the runner compute the SAME canonical a11y signature, so a marker carrying
// the SDK's sig matches the runner's identical sig; an empty-sig marker lands on
// the next observed state. Per-sig de-dup keeps a standing violation from
// repeating on every settle.
final class InvariantScrape {
    let path: String
    var processedLines = 0
    var bySig: [String: [[String: String]]] = [:]
    var fallback: [[String: String]]?
    var emitted = Set<String>()

    init(_ path: String) { self.path = path }

    // Fold any newly appended complete marker lines into the pending maps.
    func ingest() {
        guard let text = try? String(contentsOfFile: path, encoding: .utf8) else { return }
        let lines = text.split(separator: "\n", omittingEmptySubsequences: false).map(String.init)
        // The trailing element is a partial line unless the file ends in "\n";
        // process only complete lines, and each line only once.
        let complete = text.hasSuffix("\n") ? lines.count : max(0, lines.count - 1)
        var idx = 0
        while idx < complete {
            if idx >= processedLines, let (sig, items) = parseInvariantMarker(lines[idx]) {
                if sig.isEmpty { fallback = items } else { bySig[sig] = items }
            }
            idx += 1
        }
        processedLines = complete
    }

    // Re-emit EXPLORE:INVARIANT for sig once if the app reported a violation there.
    func flush(_ sig: String) {
        ingest()
        var items = bySig[sig]
        if items == nil { items = fallback; fallback = nil }
        guard let items = items, !items.isEmpty, !emitted.contains(sig) else { return }
        emitted.insert(sig)
        let arr = items.map { ["id": $0["id"] ?? "", "message": $0["message"] ?? ""] as [String: Any] }
        emitJSON("EXPLORE:INVARIANT", ["sig": sig, "items": arr])
    }
}

let invariantScrape = InvariantScrape(invariantMarkerPath)

// LIFECYCLE-metamorphic oracles (rotation, background-restore) are NOT ported to
// the macOS AX backend: a desktop window has no device orientation to rotate, and
// this backend drives the app by walking the accessibility tree and clicking -- it
// has no app-lifecycle background/foreground hook (hiding/minimizing is a window-
// server action, not a paused->resumed lifecycle, and a hidden app's AX tree is
// unreliable), so the ground truth those oracles need cannot be produced here.
func observe() -> Snapshot {
    var snap = snapshot(appEl, valueNodeSelectors)
    snap.sig = effectiveSig(snap)
    emitJSON("FUZZ:OBS", [
        "sig": snap.sig,
        "labels": Array(snap.labels.prefix(maxLabelsPerState)),
        "elements": snap.elements,
    ])
    if seen.insert(snap.sig).inserted {
        emitJSON("EXPLORE:STATE", [
            "sig": snap.sig,
            "labels": Array(snap.labels.prefix(maxLabelsPerState)),
            "elements": snap.elements,
        ])
        // CONTENT-BUG for this newly-seen state, keyed by the SAME sig. Only
        // emitted when a broken-content artifact is actually rendered.
        if !snap.contentBugs.isEmpty {
            let items = snap.contentBugs.map { ["key": $0.key, "reason": $0.reason, "text": $0.text] as [String: Any] }
            emitJSON("EXPLORE:CONTENTBUG", ["sig": snap.sig, "items": items])
        }
        // BROKEN-ASSET (tofu) for this newly-seen state, keyed by the SAME sig.
        // Only emitted when a U+FFFD replacement character actually rendered,
        // so a clean state stays silent (no marker, no finding).
        if !snap.brokenAssets.isEmpty {
            let items = snap.brokenAssets.map { ["key": $0.key, "reason": "tofu", "detail": $0.detail] as [String: Any] }
            emitJSON("EXPLORE:BROKENASSET", ["sig": snap.sig, "items": items])
        }
    }
    // APP-INVARIANT (EXPLORE:INVARIANT): re-emit any violation the app's SDK
    // reported for this state (scraped from the marker file). Runs every settle,
    // not just new states, so a violation on a revisit is caught; de-duped per sig.
    invariantScrape.flush(snap.sig)
    return snap
}

var current = observe()
launchSig = current.sig
var stuck = 0
var failed = false
let prefixLen = fuzz.prefix?.count ?? 0
let mapMode = fuzz.replay == nil && fuzz.prefix == nil && fuzz.seed == 0
let budget = fuzz.replay?.count ?? ((mapMode && !fuzz.configured ? Int.max / 4 : fuzz.budget) + prefixLen)
// LEAK sampler (--soak): only in REPLAY mode (the soak tier writes {"replay":[..]})
// do we sample the target's RSS, once at start and after each cycle, forming the
// RSS-vs-time series soak.rs reads. No-op outside replay (a plain fuzz is no soak).
let isSoak = fuzz.replay != nil
let soakStart = Date()
if isSoak { sampleRSS(nsApp.processIdentifier, 0) }

// --record clip capture: film the target window for the whole replay, then box
// the finding's element after it settles. Only armed in replay mode with a clip
// plan and REPROIT_VIDEO_DIR set. clipEl/clipRect are captured live during the
// replay (the element handle is freshest at the tap that triggered the finding).
let clipVideoDir = ProcessInfo.processInfo.environment["REPROIT_VIDEO_DIR"] ?? ""
let clipArmed = !clipVideoDir.isEmpty && fuzz.clipSel != nil && fuzz.replay != nil
let clipMov = clipArmed ? URL(fileURLWithPath: clipVideoDir).appendingPathComponent("clip.mov").path : ""
var clipProc: Process? = nil
var clipCaptureStart = Date()
var clipEl: AXUIElement? = nil
// The finding element's ABSOLUTE screen frame, captured at the triggering tap
// (when the handle is guaranteed valid and the window is on-screen). Stored as a
// value rect, NOT re-read at finalize, so a stale/torn-down handle or a post-tap
// state change (the app can navigate to a different, Save-less view) cannot lose
// the box. Made window-relative at finalize against the stationary window bounds.
var clipRect: CGRect? = nil
var clipActionAt: TimeInterval = 0
if clipArmed {
    // A clip must film real pixels: bring the window on-screen first (it may have
    // been parked off-screen by moveOffscreen or an autosaved frame), then let it
    // settle before the capture starts so the first frames are the live UI.
    moveOnscreen(appEl)
    Thread.sleep(forTimeInterval: 0.5)
    clipProc = startClipCapture(nsApp.processIdentifier, clipMov)
    clipCaptureStart = Date()
    // Small lead-in so the first frames exist before the triggering action.
    Thread.sleep(forTimeInterval: 0.4)
}
var i = 0
while i < budget && stuck < 3 {
    // In replay/soak, sample the heap once per cycle (BEFORE acting, so cycle k's
    // sample reflects RSS after the previous action settled), matching the web
    // runner's per-action sampling that the soak slope is read from.
    if isSoak && i > 0 { sampleRSS(nsApp.processIdentifier, Int(Date().timeIntervalSince(soakStart) * 1000)) }
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
        let options = current.tappables.sorted().map { "tap:\($0)" } + ["back"]
        rememberActions(&actionsByState, current.sig, options)
        act = firstUntriedAction(actionsByState, tried, current.sig)
        if act == nil, let path = pathToFrontier(graph, actionsByState, tried, current.sig) {
            act = path.first
        }
        if act == nil && hasFrontier(actionsByState, tried) && current.sig != launchSig {
            break
        }
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
        tried.insert(edgeKey(current.sig, "back"))
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
        // HANG watchdog: time ONLY the observe() round trip, after the fixed
        // settle sleep, so the sleep is excluded by construction. The synchronous
        // AX reads block until the target's run loop services them, so a frozen
        // main thread makes this spike past the floor.
        let hangFrom = current.sig
        let observeStart = Date()
        let next = observe()
        maybeEmitHang(hangFrom, "back", Int(Date().timeIntervalSince(observeStart) * 1000))
        // Layer-1 effect detection (docs/signature.md "Value-state"): an action
        // is EFFECTIVE iff the (effective) signature changed OR the content
        // fingerprint changed; a value-only change (a counter ticking) still
        // counts, so a value-state app does not stall to a single dead state.
        if next.sig != current.sig {
            emitEdge(current.sig, "back", next.sig)
            rememberEdge(&graph, current.sig, "back", next.sig)
            stuck = 0
        } else if next.content != current.content {
            stuck = 0 // effective (value changed) but same node: keep exploring
        } else { stuck += 1 }
        current = next
        i += 1
        continue
    }
    let label = String(a.dropFirst("tap:".count))
    tried.insert(edgeKey(current.sig, a))
    // HANG watchdog: time the synchronous press + observe round trip. AX calls
    // block on the target's main run loop, so a freeze spikes this. The fixed
    // settle sleep is subtracted below so only blocking time crosses the floor.
    let hangFrom = current.sig
    let pressStart = Date()
    let pressedEl = current.nodeByLabel[label]
    // --record: the tap on the finding's element is the moment to box. Grab the
    // freshest element handle and the capture-relative timestamp now, before the
    // press may mutate the tree (post-loop resolution can fall back to this).
    if clipArmed, let sel = fuzz.clipSel, label == sel {
        // The FIRST tap on the finding's element is when it triggered, so anchor the
        // box's on-screen time there (earliest appearance, longest dwell). But keep
        // filling the frame on later taps if an earlier AXPosition/AXSize read came
        // back nil (an occasional race just after the window moves on-screen), so one
        // transient nil can never lose the box.
        if clipEl == nil { clipEl = pressedEl }
        if clipRect == nil, let r = pressedEl.flatMap(axFrameOf) { clipRect = r }
        if clipActionAt == 0 { clipActionAt = Date().timeIntervalSince(clipCaptureStart) }
    }
    if let el = pressedEl {
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
    // Blocking time = total elapsed minus the fixed 0.7s settle sleep, so only a
    // genuine main-thread freeze (not the deliberate settle) can cross the floor.
    maybeEmitHang(hangFrom, "tap:\(label)", Int(Date().timeIntervalSince(pressStart) * 1000) - 700)
    // Layer-1 effect detection: an effective action (signature OR content
    // fingerprint changed) resets the stall counter; only a true no-op (a dead
    // key, a disabled control) leaves both unchanged. A value-only change emits
    // no edge (same node) but still counts as progress.
    if next.sig != current.sig {
        emitEdge(current.sig, "tap:\(label)", next.sig)
        rememberEdge(&graph, current.sig, "tap:\(label)", next.sig)
        stuck = 0
    } else if next.content != current.content {
        stuck = 0
    } else {
        stuck += 1
    }
    current = next
    i += 1
}

// --record clip finalize: resolve the finding's element to a window-relative rect
// (both element frame and window frame are AX screen coords, top-left origin, so
// the box is element - windowOrigin), write box-spec.json in the window's own
// point space, then SIGINT screencapture so it flushes clip.mov. The host runs
// box-overlay.mjs (clip.mov + box-spec.json -> boxed clip), the uniform path for
// every runner that cannot inject a live overlay.
if clipArmed {
    // Prefer the frame snapshotted at the triggering tap; only if that never
    // captured (e.g. the sel was never tapped) fall back to a live resolution.
    let elRect = clipRect
        ?? (clipEl ?? fuzz.clipSel.flatMap { current.nodeByLabel[$0] }).flatMap(axFrameOf)
    // Use the captured window's CGWindowBounds (what screencapture -l framed), not
    // the AX window frame -- AXWindows.first can be a tiny helper layer.
    let winRect = targetWindowBounds(nsApp.processIdentifier) ?? targetWindowFrame(appEl)
    stopClipCapture(clipProc)
    var drew = false
    if let er = elRect, let wr = winRect {
        let rel: [String: Any] = [
            "x": Double(er.origin.x - wr.origin.x),
            "y": Double(er.origin.y - wr.origin.y),
            "w": Double(er.size.width),
            "h": Double(er.size.height),
            "tStart": max(0, clipActionAt - 0.3),
            "tEnd": 1e9,
            "label": fuzz.clipLabel ?? (fuzz.clipOracle ?? "finding"),
            "color": "red",
        ]
        let spec: [String: Any] = [
            "videoW": Double(wr.size.width),
            "videoH": Double(wr.size.height),
            "boxes": [rel],
        ]
        let specPath = URL(fileURLWithPath: clipVideoDir)
            .appendingPathComponent("box-spec.json").path
        if let data = try? JSONSerialization.data(withJSONObject: spec, options: []) {
            try? data.write(to: URL(fileURLWithPath: specPath))
            drew = true
        }
    }
    emitJSON("FINDING:BOXED", [
        "oracle": fuzz.clipOracle ?? "",
        "sel": fuzz.clipSel ?? "",
        "mov": clipMov,
        "drew": drew,
    ])
}

emit("JOURNEY[a] step: explored \(seen.count) states")
emit("JOURNEY DONE")
emit(failed ? "Some tests failed" : "All tests passed")
