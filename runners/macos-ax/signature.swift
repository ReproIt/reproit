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

import ApplicationServices
import Cocoa
import Foundation

let actionBudgetDefault = 36
let maxLabelLen = 40
let maxLabelsPerState = 24

func emit(_ s: String) {
  print(s)
  fflush(stdout)
}

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
  var clipSel: String?  // element label to box (the finding's control/option)
  var clipLabel: String?  // caption text drawn on the box
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

func rememberActions(_ actionsByState: inout [String: [String]], _ sig: String, _ actions: [String])
{
  var known = actionsByState[sig] ?? []
  for action in actions where !known.contains(action) { known.append(action) }
  actionsByState[sig] = known
}

func firstUntriedAction(_ actionsByState: [String: [String]], _ tried: Set<String>, _ sig: String)
  -> String?
{
  for action in actionsByState[sig] ?? [] {
    if !tried.contains(edgeKey(sig, action)) { return action }
  }
  return nil
}

func hasFrontier(_ actionsByState: [String: [String]], _ tried: Set<String>) -> Bool {
  actionsByState.keys.contains { firstUntriedAction(actionsByState, tried, $0) != nil }
}

func rememberEdge(
  _ graph: inout [String: [(String, String)]], _ from: String, _ action: String, _ to: String
) {
  var edges = graph[from] ?? []
  if !edges.contains(where: { $0.0 == action && $0.1 == to }) {
    edges.append((action, to))
  }
  graph[from] = edges
}

func pathToFrontier(
  _ graph: [String: [(String, String)]], _ actionsByState: [String: [String]], _ tried: Set<String>,
  _ start: String
) -> [String]? {
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
    let text = String(data: data, encoding: .utf8)
  else { return [] }
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
    if (v.hasPrefix("\"") && v.hasSuffix("\"")) || (v.hasPrefix("'") && v.hasSuffix("'")),
      v.count >= 2
    {
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
          let v = clean(part)
          if !v.isEmpty { out.append(v) }
        }
        return out
      }
      var j = i + 1
      while j < lines.count {
        let raw = lines[j]
        let t = raw.trimmingCharacters(in: .whitespaces)
        if t.isEmpty || t.hasPrefix("#") {
          j += 1
          continue
        }
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
    s ^= s << 13
    s ^= s >> 17
    s ^= s << 5
    return Int(s & 0x7fff_ffff) % n
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
    role = r
    type = t
    icon = ic
    id = i
    children = c
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
  for b in bytes {
    h ^= UInt32(b)
    h = h &* 0x0100_0193
  }
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
    let sizeV = axCopy(el, kAXSizeAttribute as String)
  else { return nil }
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
    || text.range(of: "\\$\\{[^}]*\\}", options: .regularExpression) != nil
  {
    return "unrendered-template"
  }
  // A bare value coerced into the label as a WHOLE word. The surrounding-char
  // guards match the web runner so ordinary prose ("Cancellation", "Null
  // Island") is not flagged: the token must stand alone.
  if text.range(of: "(^|[\\s:>(\\[,])undefined($|[\\s.,!?)\\]<])", options: .regularExpression)
    != nil
  {
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
  if role == axSheet || role == axDrawer || role == axPopover || role == axWindow {
    return "dialog"
  }
  if role == axProgress || role == axBusy { return "progress" }  // transient
  if role == axGroup || role == axScrollArea || role == axSplitGroup
    || role == axToolbar || role == axUnknown || role.isEmpty
  {
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
    let arr = names as? [String]
  else { return false }
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

