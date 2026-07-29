import ApplicationServices
import Cocoa
import Foundation

struct Snapshot {
  var sig: String  // canonical (structural + value) signature
  var structuralSig: String  // structural-only sig: the per-node key the cap tracks
  var vsection: String  // the V: section body ("" when none)
  var content: String  // Layer-1 content fingerprint (runner-local, ephemeral)
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
  var roleIndexTargets: [String: AXUIElement] = [:]  // "role:r#i" -> element
  let needRoleResolution = valueNodeSelectors.contains { $0.hasPrefix("role:") }

  // Build the canonical SigNode tree AND gather display labels in one pass.
  func build(
    _ el: AXUIElement, _ depth: Int, isRoot: Bool,
    roleCounter: inout [String: Int]
  ) -> SigNode? {
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
    let optIn =
      !isRoot
      && matchesValueNodeAX(
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
      if let n = build(
        c, depth + 1, isRoot: false,
        roleCounter: &roleCounter)
      {
        kids.append(n)
      }
    }
    return SigNode(
      role: role,
      id: id,
      type: axTypeOf(el, role),
      icon: nil,  // AX exposes no language-independent icon identity
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
    if let n = build(
      c, 1, isRoot: false,
      roleCounter: &roleCounter)
    {
      rootKids.append(n)
    }
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
    FileHandle.standardError.write(
      "selftest: could not load signature_vectors.json\n".data(using: .utf8)!)
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
      let line =
        "selftest FAIL '\(desc)': expected \(expected) got \(got)\n"
          + "  descriptor=\(descriptorOf(anchor, tree).debugDescription)\n"
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
    if !cond {
      ok = false
      FileHandle.standardError.write("selftest value-state FAIL: \(msg)\n".data(using: .utf8)!)
    }
  }
  // value_class buckets.
  let buckets: [(String, String)] = [
    ("", "EMPTY"), ("   ", "EMPTY"), ("0", "ZERO"), ("-0", "ZERO"), ("-3", "NEG"),
    ("3", "POS1"), ("+7", "POS1"), ("99", "POS2"), ("100", "POS3"), ("1000", "POSL"),
    ("1,234", "NONEMPTY"), ("3.", "NONEMPTY"), (".5", "NONEMPTY"), ("$5", "NONEMPTY"),
  ]
  for (s, want) in buckets {
    check(valueClass(s) == want, "value_class(\(s.debugDescription))=\(valueClass(s)) want \(want)")
  }
  // chrome value is NOT value-bearing: byte-identical to no value.
  let header = SigNode(role: "header", id: "title", value: "Welcome")
  check(descriptorOf(nil, header) == "A:\n0:header@title", "chrome value leaked into V:")
  // value-role textfield folds a V: entry; status normalizes to node in body.
  let tf = SigNode(role: "textfield", id: "email", value: "a@b.com")
  check(
    descriptorOf(nil, tf) == "A:\n0:textfield@email\nV:key:email=NONEMPTY", "textfield V: wrong")
  let status = SigNode(role: "status", id: "count", value: "5")
  check(descriptorOf(nil, status) == "A:\n0:node@count\nV:key:count=POS1", "status V: wrong")
  // opt-in value_node folds a chrome node's value-class into V:.
  var optIn = SigNode(role: "text", id: "display", value: "42")
  check(descriptorOf(nil, optIn) == "A:\n0:text@display", "chrome text leaked without flag")
  optIn.valueNode = true
  check(
    descriptorOf(nil, optIn) == "A:\n0:text@display\nV:key:display=POS2",
    "opt-in value_node V: wrong")
  // keyless value nodes collapse structurally but stay distinct in V:.
  let keyless = SigNode(
    role: "screen",
    children: [
      SigNode(role: "textfield", value: "3"),
      SigNode(role: "textfield", value: "99"),
    ])
  check(
    descriptorOf(nil, keyless)
      == "A:\n0:screen;1:textfield*\nV:role:textfield#0=POS1;role:textfield#1=POS2",
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
    if !cond {
      ok = false
      FileHandle.standardError.write("selftest tofu FAIL: \(msg)\n".data(using: .utf8)!)
    }
  }
  // Clean labels never flag: no U+FFFD, no finding, however odd the text.
  check(tofuExcerpt("") == nil, "empty text must not flag")
  check(tofuExcerpt("Save changes") == nil, "plain text must not flag")
  check(tofuExcerpt("caf\u{e9} \u{4f60}\u{597d} \u{1f600}") == nil, "non-ASCII text is not tofu")
  // A rendered replacement char flags, keeping context around the char.
  check(
    tofuExcerpt("glitch \u{FFFD} here") == "glitch \u{FFFD} here", "short text keeps full context")
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
    if !cond {
      ok = false
      FileHandle.standardError.write("selftest invariant FAIL: \(msg)\n".data(using: .utf8)!)
    }
  }
  if let (sig, items) = parseInvariantMarker(
    "REPROIT_INVARIANT {\"sig\":\"s1\",\"items\":[{\"id\":\"total\",\"message\":\"NaN\"}]}")
  {
    check(sig == "s1", "marker carries the SDK sig")
    check(
      items.count == 1 && items[0]["id"] == "total" && items[0]["message"] == "NaN",
      "marker carries the violated id + message")
  } else {
    check(false, "a well-formed marker must parse")
  }
  check(parseInvariantMarker("ordinary log line") == nil, "a non-marker line is silent")
  check(parseInvariantMarker("REPROIT_INVARIANT {oops") == nil, "malformed json is silent")
  check(
    parseInvariantMarker("REPROIT_INVARIANT {\"items\":[]}") == nil, "an empty item list is silent")
  return ok
}

func emitEdge(_ from: String, _ action: String, _ to: String) {
  let payload: [String: Any] = ["from": from, "action": action, "to": to]
  if let d = try? JSONSerialization.data(withJSONObject: payload),
    let s = String(data: d, encoding: .utf8)
  {
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
  guard
    let out = String(data: data, encoding: .utf8)?.trimmingCharacters(in: .whitespacesAndNewlines),
    let kib = Int(out)
  else { return }
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
      w >= 1, h >= 1
    else { continue }
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
    let w = windows.first
  else { return nil }
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
      proc.arguments = [
        "-x", "-R\(Int(f.origin.x)),\(Int(f.origin.y)),\(Int(f.size.width)),\(Int(f.size.height))",
        out,
      ]
    } else {
      proc.arguments = ["-x", out]  // last resort: whole desktop
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
  proc.interrupt()  // SIGINT -> screencapture flushes and closes the .mov
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

func targetIsDead(_ app: NSRunningApplication, _ appEl: AXUIElement, _ pressStatus: AXError) -> Bool
{
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
    let stillListed =
      NSRunningApplication
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
  if let a = NSRunningApplication.runningApplications(withBundleIdentifier: target).first {
    return a
  }
  return NSWorkspace.shared.runningApplications.first { $0.localizedName == target }
}

func launch(_ target: String) -> NSRunningApplication? {
  if let a = runningApp(target) { return a }
  guard let url = NSWorkspace.shared.urlForApplication(withBundleIdentifier: target) else {
    return nil
  }
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
    result = app
    sem.signal()
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

