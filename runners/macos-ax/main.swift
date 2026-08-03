import ApplicationServices
import Cocoa
import Foundation

// Self-test mode: validate the canonical signature against the golden vectors
// without launching an app or needing Accessibility permission. Used by CI.
if CommandLine.arguments.contains("--selftest") || env["REPROIT_SELFTEST"] == "1" {
  exit(runSelfTest() ? 0 : 1)
}

guard let target = env["REPROIT_TARGET"], !target.isEmpty else {
  FileHandle.standardError.write(
    "REPROIT_TARGET (bundle id or app name) required\n".data(using: .utf8)!)
  exit(2)
}
// Multi-actor scenario: this process plays ONE actor of an authored multi-user
// journey, pulling each action from the host conductor instead of fuzzing.
// Same env contract as the web runner (the orchestrator passes defines as env
// to every non-flutter backend). The claimed-role line is emitted by the
// scenario actor itself (with its real role) on this path.
let scenarioBase =
  (env["REPROIT_SCENARIO_BARRIER"] ?? "").isEmpty ? nil : env["REPROIT_SCENARIO_BARRIER"]
if scenarioBase == nil { emit("JOURNEY claimed role=a") }
guard AXIsProcessTrusted() else {
  crashBlock(
    "accessibility not trusted",
    "grant Accessibility to this process in System Settings > Privacy & Security")
  exit(3)
}
// App-invariant marker file (per-pid): the launched app's SDK writes its
// REPROIT_INVARIANT markers here (path handed to it in the launch environment,
// which is also the SDK's fuzzer-detection gate); the runner scrapes it after
// each settle. Truncated up front so the first read starts empty. Defined before
// launch() so the launch environment can carry the path.
let invariantMarkerPath =
  NSTemporaryDirectory()
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
if env["REPROIT_INSPECT"] == "1" {
  moveOnscreen(appEl)
} else {
  moveOffscreen(appEl)
}
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
var valueCombos: [String: Set<String>] = [:]  // structuralSig -> set of V: sections
var cappedNodes = Set<String>()  // structuralSig that hit the cap

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
    let s = String(data: d, encoding: .utf8)
  {
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
    let rawItems = obj["items"] as? [[String: Any]]
  else { return nil }
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
    if items == nil {
      items = fallback
      fallback = nil
    }
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
    if fuzz.replay == nil {
      let scrollItems = axScrollRoundTrip(appEl, nsApp.isActive)
      if !scrollItems.isEmpty {
        emitJSON("EXPLORE:SCROLLROUNDTRIP", ["sig": snap.sig, "items": scrollItems])
      }
    }
    // CONTENT-BUG for this newly-seen state, keyed by the SAME sig. Only
    // emitted when a broken-content artifact is actually rendered.
    if !snap.contentBugs.isEmpty {
      let items = snap.contentBugs.map {
        ["key": $0.key, "reason": $0.reason, "text": $0.text] as [String: Any]
      }
      emitJSON("EXPLORE:CONTENTBUG", ["sig": snap.sig, "items": items])
    }
    // BROKEN-ASSET (tofu) for this newly-seen state, keyed by the SAME sig.
    // Only emitted when a U+FFFD replacement character actually rendered,
    // so a clean state stays silent (no marker, no finding).
    if !snap.brokenAssets.isEmpty {
      let items = snap.brokenAssets.map {
        ["key": $0.key, "reason": "tofu", "detail": $0.detail] as [String: Any]
      }
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
let budget =
  fuzz.replay?.count ?? ((mapMode && !fuzz.configured ? Int.max / 4 : fuzz.budget) + prefixLen)
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
let clipMov =
  clipArmed ? URL(fileURLWithPath: clipVideoDir).appendingPathComponent("clip.mov").path : ""
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
var inspectAutoContinue = false
while i < budget && stuck < 3 {
  // In replay/soak, sample the heap once per cycle (BEFORE acting, so cycle k's
  // sample reflects RSS after the previous action settled), matching the web
  // runner's per-action sampling that the soak slope is read from.
  if isSoak && i > 0 {
    sampleRSS(nsApp.processIdentifier, Int(Date().timeIntervalSince(soakStart) * 1000))
  }
  var act: String?
  if let r = fuzz.replay {
    act = i < r.count ? r[i] : nil
  } else if i < prefixLen {
    act = fuzz.prefix![i]
  } else if fuzz.seed != 0 {
    // Inverse-visit-count weighted pick, identical to the other runners.
    let taps = current.tappables.sorted()
    let ew = fuzz.edgeWeights[current.sig] ?? [:]
    var options = taps.map { "tap:\($0)" }
    options.append("back")
    let weights = options.map { 1.0 / (1.0 + Double(ew[$0] ?? 0)) }
    let total = weights.reduce(0, +)
    var r = rng.unit() * total
    act = options.last
    for k in 0..<options.count {
      r -= weights[k]
      if r <= 0 {
        act = options[k]
        break
      }
    }
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
  if fuzz.replay != nil && !inspectAutoContinue {
    do {
      inspectAutoContinue = try inspectPlatformStep(a, i + 1, fuzz.replay?.count ?? 0)
    } catch {
      crashBlock("inspection stopped", "\(error)")
      break
    }
  }
  emit("FUZZ:ACT \(a)")
  // Named screenshot point (from a replay/prefix script): capture the target
  // window to REPROIT_SHOTS_DIR and print SHOOT:<name>. Sanitize <name> to the
  // contract's [A-Za-z0-9_/-]; not a UI action, so it does not affect stuck.
  if a.hasPrefix("shoot:") {
    let raw = String(a.dropFirst("shoot:".count))
    let name = String(
      raw.unicodeScalars.filter {
        CharacterSet(
          charactersIn: "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_/-"
        ).contains($0)
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
      if AXUIElementPerformAction(el, kAXPressAction as CFString) == .success {
        didBack = true
        break
      }
    }
    if !didBack && ProcessInfo.processInfo.environment["REPROIT_ALLOW_KEYS"] == "1" {
      pressKey(53)
      didBack = true
    }
    if !didBack {
      stuck += 1
      i += 1
      continue
    }
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
      stuck = 0  // effective (value changed) but same node: keep exploring
    } else {
      stuck += 1
    }
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
  let focusArm = axFocusArm(current, pressedEl)
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
      emit("FUZZ:MISS \(a)")
      stuck += 1
      i += 1
      continue
    }
  } else {
    emit("FUZZ:MISS \(a)")
    stuck += 1
    i += 1
    continue
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
  let action = "tap:\(label)"
  if axFocusWasLost(focusArm, next, appEl, next.sig == current.sig) {
    emitJSON("EXPLORE:FOCUSLOSS", ["from": hangFrom, "action": action])
  }
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
  let elRect =
    clipRect
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
  emitJSON(
    "FINDING:BOXED",
    [
      "oracle": fuzz.clipOracle ?? "",
      "sel": fuzz.clipSel ?? "",
      "mov": clipMov,
      "drew": drew,
    ])
}

emit("JOURNEY[a] step: explored \(seen.count) states")
emit("JOURNEY DONE")
emit(failed ? "Some tests failed" : "All tests passed")
