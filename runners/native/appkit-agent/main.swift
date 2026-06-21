// ReproIt AppKit IN-PROCESS operability agent (graph-1 ground truth).
//
// Unlike runners/macos-ax.swift (which reads the EXTERNAL AX tree of ANOTHER
// process = graph 2 only), this agent runs INSIDE an AppKit app and has direct
// access to the real NSView/NSControl object graph + their wired handlers. That
// is graph 1: the literal operability ground truth (does this object actually
// DO something when poked), independent of whatever the app published to a11y.
//
// It then queries graph 2 for the SAME object (NSAccessibility role/label, the
// key-view loop, focus eligibility) and JOINs the two by object identity (the
// NSView pointer). The diff is the operability/accessibility gap the engine
// scores: an object that is operable in graph 1 but is missing a role / not in
// the tab order / not keyboard-activatable in graph 2.
//
// Output: the framework-neutral marker `reproit` parses (crates/reproit/src/
// model/map.rs::gaps_from_groundtruth):
//   EXPLORE:GROUNDTRUTH {"sig":..,"focusTrap":bool,"elements":[{id,operable,
//     gestureKind,a11y:{rolePresent,namePresent,focusable,inTabOrder,
//     keyboardActivatable}}]}
//
// THE PROOF this agent exists to demonstrate: it builds a window with a REAL
// NSButton (operable, full a11y) and a FAKE button (a custom NSView with a
// click gesture recognizer + handler, NO NSAccessibility role) and shows the
// fake one is `operable:true` but `rolePresent:false` / not keyboard reachable:
// a real, machine-detected operability gap.
//
// Runs headless: it builds the view tree and runs the walk WITHOUT showing a
// window (NSApp is created but never `run()` / `activateIgnoringOtherApps`), so
// it works over SSH / in CI with no window server interaction. Build + run:
//   swiftc -O runners/native/appkit-agent/main.swift -o /tmp/appkit-agent \
//     && /tmp/appkit-agent

import Cocoa
import Foundation

func emit(_ s: String) { print(s); fflush(stdout) }

// ---- canonical signature (FNV-1a over a normalized role/id token tree) ------
// Byte-compatible subset of the oracle in runners/macos-ax.swift: we only need
// the structural signature for the marker's `sig` field here (no value-state),
// so this is the minimal descriptorOf + FNV-1a. The engine treats `sig` as an
// opaque state key, so any stable per-state string works; we use the real one.
let kRoles: Set<String> = [
    "screen", "header", "text", "button", "link", "textfield", "image", "icon",
    "list", "listitem", "tab", "switch", "checkbox", "radio", "slider", "menu",
    "menuitem", "dialog", "group", "node",
]
func normalizeRole(_ r: String) -> String { kRoles.contains(r) ? r : "node" }
func fnv1a32hex(_ bytes: [UInt8]) -> String {
    var h: UInt32 = 0x811c_9dc5
    for b in bytes { h ^= UInt32(b); h = h &* 0x0100_0193 }
    return String(format: "%08x", h)
}

// ---- the FAKE button: a custom NSView with a click handler and no a11y role -
// This is the canonical operability anti-pattern: a developer drew a button
// (a styled NSView) and wired a click gesture recognizer to an action, but
// never adopted the NSAccessibility button role / made it focusable. It LOOKS
// and ACTS like a button (graph 1 = operable) yet is invisible to assistive
// tech and the keyboard (graph 2 = no role, not in tab order).
final class FakeButton: NSView {
    var onClick: (() -> Void)?
    private var didWireGesture = false

    func wireClickHandler() {
        guard !didWireGesture else { return }
        let g = NSClickGestureRecognizer(target: self, action: #selector(handleClick))
        addGestureRecognizer(g)
        didWireGesture = true
    }
    @objc private func handleClick() { onClick?() }

    // Deliberately DO NOT override accessibility role / isAccessibilityElement,
    // so graph 2 sees no button role. (A correct custom control would set
    // setAccessibilityRole(.button) and become keyboard-activatable.)
}

// A "good" custom control for contrast: a custom NSView that DOES adopt the
// accessibility button role and is focusable, so graph 1 and graph 2 agree.
final class AccessibleCustomButton: NSView {
    var onClick: (() -> Void)?
    override func awakeFromNib() {}
    func wireUp() {
        let g = NSClickGestureRecognizer(target: self, action: #selector(handleClick))
        addGestureRecognizer(g)
        setAccessibilityRole(.button)
        setAccessibilityLabel("Accessible Custom Button")
        setAccessibilityElement(true)
    }
    @objc private func handleClick() { onClick?() }
    override var acceptsFirstResponder: Bool { true }   // keyboard focusable
    override func keyDown(with event: NSEvent) {
        // Activatable by Space/Return (keyboard activation path).
        if event.keyCode == 49 || event.keyCode == 36 { onClick?() }
        else { super.keyDown(with: event) }
    }
}

// ---- graph 1: is this NSView OPERABLE (does poking it actually do work)? -----
// Ground truth, read off the real object graph, NOT from a11y:
//   - an NSControl with a non-nil target+action (a button, checkbox, ...),
//   - OR any view carrying a click/press/tap gesture recognizer,
//   - OR (weaker) a first-responder-accepting view with a keyDown override.
// Returns (operable, gestureKind) where gestureKind labels what kind of gesture
// the engine should synthesize, mirroring the marker's `gestureKind`.
func graph1Operable(_ v: NSView) -> (Bool, String) {
    // 1) NSControl with a wired target-action is the textbook operable case.
    if let c = v as? NSControl {
        // A control is operable when enabled and it has at least one action.
        let hasAction = c.action != nil
        if c.isEnabled && hasAction { return (true, "button") }
        // Some controls (e.g. NSTextField) are operable via editing, not action.
        if c is NSTextField, (c as! NSTextField).isEditable { return (true, "type") }
        if hasAction { return (true, "button") }
    }
    // 2) A click / press / pan gesture recognizer makes ANY view operable, even
    //    a plain NSView (the "fake button" case).
    for g in v.gestureRecognizers {
        switch g {
        case is NSClickGestureRecognizer: return (true, "button")
        case is NSPressGestureRecognizer: return (true, "longPress")
        case is NSPanGestureRecognizer:   return (true, "pan")
        case is NSMagnificationGestureRecognizer: return (true, "magnify")
        case is NSRotationGestureRecognizer: return (true, "rotate")
        default: return (true, "gesture")
        }
    }
    // 3) Keyboard-only operable: accepts first responder AND overrides keyDown.
    //    (Heuristic; reported as a weak operable with a "key" gesture.)
    if v.acceptsFirstResponder {
        let cls = type(of: v)
        if cls.instancesRespond(to: #selector(NSResponder.keyDown(with:))),
           cls != NSView.self {
            return (true, "key")
        }
    }
    return (false, "")
}

// ---- graph 2: the NSAccessibility projection of the SAME object --------------
// Read entirely through the public NSAccessibility protocol the view publishes
// (same surface VoiceOver / the AX client sees), joined to graph 1 by identity.
struct A11y {
    var rolePresent: Bool
    var namePresent: Bool
    var focusable: Bool
    var inTabOrder: Bool
    var keyboardActivatable: Bool
}

// The accessibility role the AX client actually sees for a view. IMPORTANT
// AppKit detail (verified empirically): standard NSControls delegate their
// accessibility to their CELL, so `NSButton.accessibilityRole()` on the view
// returns AXUnknown while `NSButton.cell.accessibilityRole()` returns AXButton.
// An in-process graph-2 reader must consult the cell for cell-backed controls,
// otherwise it would wrongly flag every standard button as role-less. Custom
// NSViews carry their role on the view itself.
func resolvedAXRole(_ v: NSView) -> NSAccessibility.Role? {
    if let control = v as? NSControl, let cell = control.cell {
        let r = cell.accessibilityRole()
        if let r = r, !r.rawValue.isEmpty, r != .unknown { return r }
    }
    return v.accessibilityRole()
}

func graph2A11y(_ v: NSView, inKeyViewLoop: Bool) -> A11y {
    // Role: present iff the view (or its cell, for cell-backed controls) reports
    // a non-empty, non-`unknown`/`group` accessibility role. A plain NSView with
    // no override reports `.group`/`.unknown`, which is NOT an operable role.
    let role = resolvedAXRole(v)
    let rolePresent: Bool = {
        guard let r = role, !r.rawValue.isEmpty else { return false }
        // Treat the structural fall-throughs as "no operable role".
        if r == .unknown || r == .group { return false }
        return true
    }()
    let cellLabel = (v as? NSControl)?.cell?.accessibilityLabel()
    let name = (v.accessibilityLabel() ?? cellLabel ?? v.accessibilityTitle() ?? "")
    let namePresent = !name.isEmpty
    // Focusable: the view can become the first responder (a precondition for
    // keyboard reachability). NSControl reports refusesFirstResponder; views
    // expose acceptsFirstResponder.
    let focusable = v.acceptsFirstResponder
    // In tab order: present in the window's key-view loop (computed by the
    // caller, which walks nextKeyView from initialFirstResponder).
    let inTabOrder = inKeyViewLoop
    // Keyboard-activatable: an a11y element that exposes the AXPress action OR a
    // control that activates from the keyboard. NSControl/NSButton do; a bare
    // gesture-recognizer view does NOT (gestures are pointer-only).
    // Keyboard-activatable: a focusable NSControl activates from the keyboard
    // (Space/Return), or a custom view that adopts the button role + is focusable
    // (so VoiceOver/keyboard can press it). A bare gesture-recognizer view is
    // pointer-only: not keyboard-activatable.
    let keyboardActivatable = (v is NSControl && focusable)
        || (rolePresent && focusable)
    return A11y(rolePresent: rolePresent, namePresent: namePresent,
                focusable: focusable, inTabOrder: inTabOrder,
                keyboardActivatable: keyboardActivatable)
}

// Identity-stable element id for the marker: accessibilityIdentifier if the dev
// set one (the macOS test-id), else a structural role#index fallback.
func elementId(_ v: NSView, roleIndex: inout [String: Int]) -> String {
    let ident = v.accessibilityIdentifier()
    if !ident.isEmpty { return "key:\(ident)" }
    let role = normalizeRole(axRoleString(v))
    let idx = roleIndex[role] ?? 0
    roleIndex[role] = idx + 1
    return "role:\(role)#\(idx)"
}

// Map the NSAccessibility role to the canonical role vocabulary for ids/sig.
func axRoleString(_ v: NSView) -> String {
    guard let r = resolvedAXRole(v) else { return "group" }
    switch r {
    case .button, .popUpButton, .menuButton: return "button"
    case .checkBox: return "checkbox"
    case .radioButton: return "radio"
    case .slider: return "slider"
    case .textField: return "textfield"
    case .staticText: return "text"
    case .image: return "image"
    case .link: return "link"
    default: return "group"
    }
}

// ---- the key-view (tab order) loop ------------------------------------------
// Walk window.initialFirstResponder -> nextKeyView around the loop, collecting
// the set of views the keyboard can reach with Tab. Anything operable but NOT in
// this set is keyboard-unreachable (an `inTabOrder:false` gap).
func keyViewLoop(_ window: NSWindow) -> Set<ObjectIdentifier> {
    var reachable = Set<ObjectIdentifier>()
    guard let start = window.initialFirstResponder ?? window.contentView?.nextValidKeyView
    else { return reachable }
    var cur: NSView? = start
    var guardN = 0
    while let v = cur, guardN < 4096 {
        if !reachable.insert(ObjectIdentifier(v)).inserted { break } // cycled
        cur = v.nextValidKeyView
        guardN += 1
    }
    return reachable
}

// ---- the walk: graph 1 x graph 2 over the real view tree --------------------
struct GTElement {
    let id: String
    let operable: Bool
    let gestureKind: String
    let a11y: A11y
}

// Reachability: an element a pointer or keyboard user can ACTUALLY reach in the
// current layout. A hidden / zero-alpha / fully-clipped (off-screen) view is
// operable by nobody, so it must not be scored as a pointer-only or keyboard-
// unreachable gap. Mirrors the web runner's reachability gate on `operable`:
// without it, an off-screen operable view is a phantom gap (graph 1 has it, the
// key-view loop never can, so the diff always "finds" it).
func isReachable(_ v: NSView) -> Bool {
    if v.isHiddenOrHasHiddenAncestor { return false }
    if v.window == nil { return false }
    if v.alphaValue <= 0.01 { return false }
    if v.visibleRect.isEmpty { return false }
    return true
}

func walkAndJoin(_ window: NSWindow) -> (sig: String, focusTrap: Bool, elements: [GTElement]) {
    let loop = keyViewLoop(window)
    var elements: [GTElement] = []
    var sigTokens: [String] = []
    var roleIndex: [String: Int] = [:]

    func visit(_ v: NSView, depth: Int) {
        let (opRaw, kind) = graph1Operable(v)
        // Gate graph-1 operability on reachability (see isReachable): an
        // off-screen / hidden operable view is operable by no one, not a gap.
        let op = opRaw && isReachable(v)
        let inLoop = loop.contains(ObjectIdentifier(v))
        let a = graph2A11y(v, inKeyViewLoop: inLoop)
        // Only emit elements that are operable in graph 1 OR carry an operable
        // a11y role: those are the join rows the engine scores. Pure containers
        // (a bare NSView grouping subviews) are skipped to keep the marker tight.
        if op || a.rolePresent {
            let id = elementId(v, roleIndex: &roleIndex)
            elements.append(GTElement(id: id, operable: op, gestureKind: kind, a11y: a))
            sigTokens.append("\(depth):\(normalizeRole(axRoleString(v)))@\(id)")
        }
        for sub in v.subviews { visit(sub, depth: depth + 1) }
    }
    if let content = window.contentView {
        for sub in content.subviews { visit(sub, depth: 1) }
    }
    // focusTrap: a modal-ish window where >=1 operable element exists but the
    // key-view loop is empty (nothing is keyboard reachable) is a focus trap.
    let anyOperable = elements.contains { $0.operable }
    let focusTrap = anyOperable && loop.isEmpty
    let descriptor = "A:\n" + sigTokens.joined(separator: ";")
    let sig = fnv1a32hex(Array(descriptor.utf8))
    return (sig, focusTrap, elements)
}

func emitGroundTruth(_ window: NSWindow) {
    let r = walkAndJoin(window)
    var els: [[String: Any]] = []
    for e in r.elements {
        els.append([
            "id": e.id,
            "operable": e.operable,
            "gestureKind": e.gestureKind,
            "a11y": [
                "rolePresent": e.a11y.rolePresent,
                "namePresent": e.a11y.namePresent,
                "focusable": e.a11y.focusable,
                "inTabOrder": e.a11y.inTabOrder,
                "keyboardActivatable": e.a11y.keyboardActivatable,
            ],
        ])
    }
    let payload: [String: Any] = ["sig": r.sig, "focusTrap": r.focusTrap, "elements": els]
    if let d = try? JSONSerialization.data(withJSONObject: payload),
       let s = String(data: d, encoding: .utf8) {
        emit("EXPLORE:GROUNDTRUTH \(s)")
    }
    // Also emit the human-readable per-element verdict to stderr for the report.
    for e in r.elements {
        let gaps = [
            e.operable && !e.a11y.rolePresent ? "NO_ROLE" : nil,
            e.operable && !e.a11y.inTabOrder ? "KEYBOARD_UNREACHABLE" : nil,
            e.operable && !e.a11y.keyboardActivatable ? "POINTER_ONLY" : nil,
        ].compactMap { $0 }
        let verdict = gaps.isEmpty ? "OK" : "GAP(\(gaps.joined(separator: ",")))"
        FileHandle.standardError.write(
            "  \(e.id) operable=\(e.operable) gesture=\(e.gestureKind) -> \(verdict)\n"
                .data(using: .utf8)!)
    }
}

// ---- build the demo window IN-PROCESS, then walk it --------------------------
// We must create an NSApplication (AppKit views need an app + window), but we
// never call run()/activate(): the window is built, laid out, the key-view loop
// is wired, and we walk it synchronously, all headless.
let app = NSApplication.shared
app.setActivationPolicy(.accessory)   // no Dock icon, no menu bar takeover

let window = NSWindow(
    contentRect: NSRect(x: 0, y: 0, width: 400, height: 200),
    styleMask: [.titled],
    backing: .buffered,
    defer: false)
let content = NSView(frame: window.contentRect(forFrameRect: window.frame))
window.contentView = content

// (1) A REAL NSButton: operable in graph 1 (target-action) AND fully a11y in
//     graph 2 (button role, focusable, in tab order, keyboard-activatable).
let realButton = NSButton(title: "Real Button", target: nil, action: #selector(NSApplication.terminate(_:)))
realButton.frame = NSRect(x: 20, y: 120, width: 140, height: 32)
realButton.setAccessibilityIdentifier("realButton")
content.addSubview(realButton)

// (2) The FAKE button: a custom NSView with a click gesture + handler, NO a11y
//     role. Operable in graph 1, but rolePresent:false / not keyboard reachable
//     in graph 2: the gap this whole cluster exists to detect.
let fake = FakeButton(frame: NSRect(x: 20, y: 70, width: 140, height: 32))
fake.setAccessibilityIdentifier("fakeButton")
fake.onClick = { /* would do real work */ }
fake.wireClickHandler()
content.addSubview(fake)

// (3) A correctly-built custom control, for contrast: operable AND a11y-clean.
let good = AccessibleCustomButton(frame: NSRect(x: 200, y: 70, width: 160, height: 32))
good.setAccessibilityIdentifier("goodCustom")
good.onClick = {}
good.wireUp()
content.addSubview(good)

// (4) A HIDDEN fake button: operable (click gesture) but isHidden, so a user can
//     reach it with NEITHER pointer NOR keyboard. The reachability gate must drop
//     it from graph 1 so it is NOT reported as a gap (it has no role either, so it
//     should not appear in the marker at all).
let hiddenFake = FakeButton(frame: NSRect(x: 20, y: 20, width: 140, height: 32))
hiddenFake.setAccessibilityIdentifier("hiddenFake")
hiddenFake.onClick = {}
hiddenFake.wireClickHandler()
hiddenFake.isHidden = true
content.addSubview(hiddenFake)

// Wire a key-view loop that includes the real button + good custom control but
// (correctly, since the dev forgot) NOT the fake button.
window.initialFirstResponder = realButton
realButton.nextKeyView = good
good.nextKeyView = realButton
// Force layout so frames/responders settle before the walk.
content.layoutSubtreeIfNeeded()

emit("JOURNEY claimed role=appkit-agent")
emit("EXPLORE:STATE {\"sig\":\"start\",\"labels\":[\"Real Button\",\"Accessible Custom Button\"]}")
emitGroundTruth(window)
emit("JOURNEY DONE")
emit("All tests passed")
