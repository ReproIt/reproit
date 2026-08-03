import ApplicationServices
import Cocoa
import Foundation

struct AXOracleNode {
  let key: String
  let element: AXUIElement
  let role: String
}

struct AXFocusArm {
  let key: String
  let element: AXUIElement
  let role: String
  let dialogCount: Int
}

func axFocusLossDecision(
  complete: Bool, role: String, sameIdentity: Bool, sameDialogs: Bool, sameScreen: Bool,
  focusedTarget: Bool, focusIsWindow: Bool
) -> Bool {
  complete && role != "link" && sameIdentity && sameDialogs && sameScreen && !focusedTarget
    && focusIsWindow
}

func axFocusArm(_ snapshot: Snapshot, _ target: AXUIElement?) -> AXFocusArm? {
  guard let target = target, axBool(target, kAXFocusedAttribute as String) == true else {
    return nil
  }
  guard let node = snapshot.oracleNodes.values.first(where: { CFEqual($0.element, target) }) else {
    return nil
  }
  return AXFocusArm(
    key: node.key, element: target, role: node.role, dialogCount: snapshot.dialogCount)
}

func axFocusWasLost(
  _ arm: AXFocusArm?, _ after: Snapshot?, _ app: AXUIElement?, _ sameScreen: Bool
) -> Bool {
  guard let arm = arm, let after = after, let app = app, arm.role != "link", sameScreen,
    arm.dialogCount == after.dialogCount, let target = after.oracleNodes[arm.key],
    CFEqual(arm.element, target.element)
  else { return false }
  var focusedValue: CFTypeRef?
  guard AXUIElementCopyAttributeValue(
    app, kAXFocusedUIElementAttribute as CFString, &focusedValue) == .success,
    let focusedValue = focusedValue, CFGetTypeID(focusedValue) == AXUIElementGetTypeID()
  else { return false }
  let focused = focusedValue as! AXUIElement
  let focusedTarget = CFEqual(focused, target.element)
  let role = axRoleOf(focused)
  return axFocusLossDecision(
    complete: true, role: arm.role, sameIdentity: true, sameDialogs: true, sameScreen: true,
    focusedTarget: focusedTarget,
    focusIsWindow: role == "dialog"
      || axStr(focused, kAXRoleAttribute) == (kAXWindowRole as String))
}

func axPointIsPresented(_ point: CGPoint) -> Bool {
  var count: UInt32 = 0
  guard CGGetActiveDisplayList(0, nil, &count) == .success, count > 0 else { return false }
  var displays = [CGDirectDisplayID](repeating: 0, count: Int(count))
  guard CGGetActiveDisplayList(count, &displays, &count) == .success else { return false }
  return displays.prefix(Int(count)).contains { CGDisplayBounds($0).contains(point) }
}

struct AXScrollPoint: Equatable {
  let position: String
  let text: String
  let shape: String
}

struct AXScrollSample {
  let offset: Int
  let points: [AXScrollPoint?]
}

func normalizeAXScrollText(_ text: String) -> String {
  var output = ""
  var inNumber = false
  for character in text.prefix(120) {
    if character.isNumber || character == "." || character == "," || character == ":" {
      if !inNumber { output.append("#") }
      inNumber = true
    } else {
      inNumber = false
      output.append(character)
    }
  }
  return output.split(whereSeparator: { $0.isWhitespace }).joined(separator: " ")
}

func axNumber(_ element: AXUIElement, _ attribute: String) -> Double? {
  (axCopy(element, attribute) as? NSNumber)?.doubleValue
}

func axScrollSample(
  _ viewport: AXUIElement, _ scrollbar: AXUIElement, _ active: Bool
) -> AXScrollSample? {
  guard active, let frame = axFrameOf(viewport), frame.width >= 8, frame.height >= 8,
    let value = axNumber(scrollbar, kAXValueAttribute as String),
    let minimum = axNumber(scrollbar, kAXMinValueAttribute as String),
    let maximum = axNumber(scrollbar, kAXMaxValueAttribute as String), maximum > minimum
  else { return nil }
  let system = AXUIElementCreateSystemWide()
  var points: [AXScrollPoint?] = []
  for fraction in [2, 5, 8] {
    let point = CGPoint(x: frame.midX, y: frame.minY + frame.height * CGFloat(fraction) / 10)
    guard axPointIsPresented(point) else { return nil }
    var hit: AXUIElement?
    let hitResult = AXUIElementCopyElementAtPosition(
      system, Float(point.x), Float(point.y), &hit)
    guard hitResult == .success,
      let hit = hit
    else { return nil }
    let text = labelOf(hit).trimmingCharacters(in: .whitespacesAndNewlines)
    if text.isEmpty || axFrameOf(hit) == nil {
      points.append(nil)
    } else {
      let bounds = axFrameOf(hit)!
      points.append(AXScrollPoint(
        position: "y=\(fraction)", text: normalizeAXScrollText(text),
        shape: "\(axRoleOf(hit))|\(Int(bounds.width))|\(Int(bounds.height))"))
    }
  }
  return AXScrollSample(
    offset: Int((((value - minimum) / (maximum - minimum)) * 1000).rounded()), points: points)
}

func axScrollRoundTripItems(
  _ before: AXScrollSample?, _ away: AXScrollSample?, _ returned: AXScrollSample?,
  _ confirmed: AXScrollSample?
) -> [[String: String]] {
  guard let before = before, let away = away, let returned = returned, let confirmed = confirmed,
    before.points.count == away.points.count, before.points.count == returned.points.count,
    before.points.count == confirmed.points.count, before.offset == returned.offset,
    returned.offset == confirmed.offset, away.offset != before.offset
  else { return [] }
  var items: [[String: String]] = []
  for index in before.points.indices {
    guard let old = before.points[index], let moved = away.points[index],
      let back = returned.points[index], let stable = confirmed.points[index],
      old.position == back.position, back.position == stable.position, old.shape == back.shape,
      back.shape == stable.shape, back.text == stable.text, old.text != back.text,
      old.text != moved.text
    else { continue }
    items.append(["pos": old.position, "before": old.text, "after": back.text])
  }
  return items
}

func axScrollRoundTrip(_ app: AXUIElement, _ active: Bool) -> [[String: String]] {
  guard active else { return [] }
  var candidates: [(area: CGFloat, viewport: AXUIElement, scrollbar: AXUIElement)] = []
  func walk(_ element: AXUIElement, _ depth: Int) {
    if depth > 60 { return }
    if axStr(element, kAXRoleAttribute) == (kAXScrollAreaRole as String),
      let scrollbarValue = axCopy(element, kAXVerticalScrollBarAttribute as String),
      CFGetTypeID(scrollbarValue) == AXUIElementGetTypeID(), let frame = axFrameOf(element)
    {
      candidates.append((frame.width * frame.height, element, scrollbarValue as! AXUIElement))
    }
    for child in axChildren(element) { walk(child, depth + 1) }
  }
  walk(app, 0)
  guard let candidate = candidates.max(by: { $0.area < $1.area }),
    let original = axNumber(candidate.scrollbar, kAXValueAttribute as String),
    let minimum = axNumber(candidate.scrollbar, kAXMinValueAttribute as String),
    let maximum = axNumber(candidate.scrollbar, kAXMaxValueAttribute as String), maximum > minimum
  else { return [] }
  var settable = DarwinBoolean(false)
  guard AXUIElementIsAttributeSettable(
    candidate.scrollbar, kAXValueAttribute as CFString, &settable) == .success,
    settable.boolValue
  else { return [] }
  let before = axScrollSample(candidate.viewport, candidate.scrollbar, active)
  let awayValue = original < (minimum + maximum) / 2 ? maximum : minimum
  guard AXUIElementSetAttributeValue(
    candidate.scrollbar, kAXValueAttribute as CFString, NSNumber(value: awayValue)) == .success
  else { return [] }
  Thread.sleep(forTimeInterval: 0.12)
  let away = axScrollSample(candidate.viewport, candidate.scrollbar, active)
  guard AXUIElementSetAttributeValue(
    candidate.scrollbar, kAXValueAttribute as CFString, NSNumber(value: original)) == .success
  else { return [] }
  Thread.sleep(forTimeInterval: 0.12)
  let returned = axScrollSample(candidate.viewport, candidate.scrollbar, active)
  Thread.sleep(forTimeInterval: 0.12)
  let confirmed = axScrollSample(candidate.viewport, candidate.scrollbar, active)
  return axScrollRoundTripItems(before, away, returned, confirmed)
}

enum InspectionControlError: Error {
  case stopped
  case timedOut
}

func inspectPlatformStep(_ action: String, _ step: Int, _ total: Int) throws -> Bool {
  let env = ProcessInfo.processInfo.environment
  guard let control = env["REPROIT_INSPECT_CONTROL"], !control.isEmpty else { return true }
  try FileManager.default.createDirectory(
    atPath: control, withIntermediateDirectories: true)
  let request = URL(fileURLWithPath: control).appendingPathComponent("request.json")
  let temp = URL(fileURLWithPath: control)
    .appendingPathComponent("request-\(ProcessInfo.processInfo.processIdentifier).tmp")
  let body: [String: Any] = [
    "sequence": step, "step": step, "total": total, "action": action, "target": action,
  ]
  let data = try JSONSerialization.data(withJSONObject: body)
  try data.write(to: temp, options: .atomic)
  _ = try? FileManager.default.removeItem(at: request)
  try FileManager.default.moveItem(at: temp, to: request)
  let response = URL(fileURLWithPath: control).appendingPathComponent("response.json")
  let deadline = Date().addingTimeInterval(240)
  while Date() < deadline {
    if let data = try? Data(contentsOf: response),
      let value = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
      value["sequence"] as? Int == step
    {
      let decision = value["decision"] as? String
      if decision == "abort" { throw InspectionControlError.stopped }
      return decision == "continue"
    }
    Thread.sleep(forTimeInterval: 0.05)
  }
  throw InspectionControlError.timedOut
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
    let arr = String(data: d, encoding: .utf8)
  {
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
    result = app
    sem.signal()
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
          settable.boolValue
        {
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
    emitJSON(
      "FUZZ:OBS",
      [
        "sig": snap.sig,
        "labels": Array(snap.labels.prefix(maxLabelsPerState)),
        "elements": snap.elements,
      ])
    if seen.insert(snap.sig).inserted {
      emitJSON(
        "EXPLORE:STATE",
        [
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
      let name = String(
        raw.unicodeScalars.filter {
          CharacterSet(
            charactersIn: "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_/-"
          ).contains($0)
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
        emit(
          "FUZZ:ASSERT \(got == want ? "pass" : "fail") count \(finder) "
            + "want=\(want) got=\(got) actor=\(role)"
        )
      } else {
        emit("FUZZ:ASSERT fail unsupported \(a) actor=\(role)")
      }
    } else if act.hasPrefix("auth:") {
      emit(
        "JOURNEY[a] step: auth-restore unsupported on desktop-ax runner; "
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
        if AXUIElementIsAttributeSettable(el, kAXFocusedAttribute as CFString, &settableFocus)
          == .success,
          settableFocus.boolValue
        {
          AXUIElementSetAttributeValue(el, kAXFocusedAttribute as CFString, kCFBooleanTrue)
        }
        typed =
          AXUIElementSetAttributeValue(el, kAXValueAttribute as CFString, value as CFTypeRef)
          == .success
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
