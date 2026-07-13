// ReproIt iOS, live view-hierarchy capture (UIKit).
//
// Everything here is guarded by `#if canImport(UIKit)`, so the package still
// builds and tests on a macOS host (where UIKit is absent). On iOS this is what
// makes the SDK "one call": it walks the live view tree for accessible names,
// debounces snapshots, hit-tests taps for structural `tap:<selector>` edges, and installs an
// uncaught-exception handler that ships an error event (with the repro path)
// before the process dies.
//
// HONEST LIMITATIONS (see README for the full list):
//   • Snapshotting and tap hit-testing run on the main thread and must touch
//     UIKit objects there; the recognizer uses cancelsTouchesInView=false and
//     allows simultaneous recognition so it never eats the app's own gestures.
//   • The accessibility surface read here is UIKit's (accessibilityLabel/title
//     + traits). SwiftUI views bridge to UIKit accessibility, so they are
//     covered, but custom drawing with no a11y label is invisible by design
//     (same as a screen reader, same as the runner).
//   • The uncaught-exception handler catches Obj-C/Swift NSExceptions and does a
//     best-effort synchronous flush. It does NOT catch fatal signals (SIGSEGV,
//     SIGABRT from `fatalError`) by default; a signal handler is provided but
//     left opt-in because running non-async-signal-safe code (URLSession, JSON)
//     in a signal handler is undefined behaviour. Treat crash-path delivery as
//     best-effort, not guaranteed.

#if canImport(UIKit)
import UIKit
import ObjectiveC

enum ReproItCapture {
    private static var engine: ReproItEngine?
    private static var debounceWorkItem: DispatchWorkItem?
    private static var tapRecognizer: UITapGestureRecognizer?
    private static var recognizerDelegate: TapDelegate?
    private static weak var observedWindow: UIWindow?
    private static var notifTokens: [NSObjectProtocol] = []
    private static var priorExceptionHandler: (@convention(c) (NSException) -> Void)?

    // MARK: attach / detach

    static func attach(to engine: ReproItEngine) {
        self.engine = engine
        // Wire up on the main thread: UIKit + run-loop work belong there.
        if Thread.isMainThread {
            wire()
        } else {
            DispatchQueue.main.async { wire() }
        }
        installExceptionHandler()
    }

    static func detach() {
        if Thread.isMainThread {
            teardown()
        } else {
            DispatchQueue.main.sync { teardown() }
        }
        restoreExceptionHandler()
        engine = nil
    }

    private static func wire() {
        installTapRecognizer()
        installNavObservers()
        // First snapshot once the UI is up.
        scheduleSnapshot()
    }

    private static func teardown() {
        debounceWorkItem?.cancel()
        debounceWorkItem = nil
        if let r = tapRecognizer, let w = observedWindow {
            w.removeGestureRecognizer(r)
        }
        tapRecognizer = nil
        recognizerDelegate = nil
        observedWindow = nil
        for t in notifTokens { NotificationCenter.default.removeObserver(t) }
        notifTokens.removeAll()
        valueNodeSelectors = []
    }

    // MARK: window / tap recognizer

    /// Best-effort key window across UIKit scene configurations.
    static func keyWindow() -> UIWindow? {
        let scenes = UIApplication.shared.connectedScenes
            .compactMap { $0 as? UIWindowScene }
        for scene in scenes where scene.activationState == .foregroundActive {
            if let w = scene.windows.first(where: { $0.isKeyWindow }) ?? scene.windows.first {
                return w
            }
        }
        // Fallback: any window from any scene.
        for scene in scenes {
            if let w = scene.windows.first(where: { $0.isKeyWindow }) ?? scene.windows.first {
                return w
            }
        }
        return nil
    }

    private static func installTapRecognizer() {
        guard tapRecognizer == nil, let window = keyWindow() else { return }
        let delegate = TapDelegate()
        let recognizer = UITapGestureRecognizer(target: Target.shared, action: #selector(Target.onTap(_:)))
        recognizer.cancelsTouchesInView = false   // never swallow the app's taps
        recognizer.delaysTouchesBegan = false
        recognizer.delaysTouchesEnded = false
        recognizer.delegate = delegate            // allow simultaneous recognition
        window.addGestureRecognizer(recognizer)
        tapRecognizer = recognizer
        recognizerDelegate = delegate
        observedWindow = window
    }

    /// Forwarding target (UITapGestureRecognizer needs an NSObject target).
    final class Target: NSObject {
        static let shared = Target()
        @objc func onTap(_ gr: UITapGestureRecognizer) {
            guard let window = ReproItCapture.observedWindow ?? ReproItCapture.keyWindow() else { return }
            let point = gr.location(in: window)
            let target = ReproItCapture.tapTargetAt(point: point, in: window)
            ReproItCapture.engine?.setPendingAction(
                target.map { "tap:\($0.selector)" } ?? "tap:?",
                label: target?.label
            )
            ReproItCapture.scheduleSnapshot()
        }
    }

    final class TapDelegate: NSObject, UIGestureRecognizerDelegate {
        func gestureRecognizer(
            _ gestureRecognizer: UIGestureRecognizer,
            shouldRecognizeSimultaneouslyWith other: UIGestureRecognizer
        ) -> Bool { true }
    }

    // MARK: navigation / layout observers

    private static func installNavObservers() {
        let nc = NotificationCenter.default
        // A new window becoming visible, or device rotation, likely changes the
        // visible surface; re-snapshot after the debounce settles.
        let names: [Notification.Name] = [
            UIWindow.didBecomeVisibleNotification,
            UIWindow.didBecomeKeyNotification,
            UIDevice.orientationDidChangeNotification,
        ]
        for name in names {
            let token = nc.addObserver(forName: name, object: nil, queue: .main) { _ in
                // Re-bind the recognizer if the key window changed.
                if observedWindow == nil || observedWindow?.window == nil {
                    installTapRecognizer()
                }
                scheduleSnapshot()
            }
            notifTokens.append(token)
        }
    }

    // MARK: debounced snapshot

    static func scheduleSnapshot() {
        guard let engine = engine else { return }
        let interval = engine.config.debounce
        let work = DispatchWorkItem { takeSnapshot() }
        // Replace any pending snapshot so we only fire after the UI settles.
        debounceWorkItem?.cancel()
        debounceWorkItem = work
        DispatchQueue.main.asyncAfter(deadline: .now() + interval, execute: work)
    }

    private static func takeSnapshot() {
        guard let engine = engine, let window = keyWindow() else { return }
        var labels: [(name: String?, tappable: Bool)] = []
        let tree = captureTree(in: window, labels: &labels)
        let snap = ReproItSnapshot.build(
            anchor: currentAnchor(),
            tree: tree,
            labels: labels,
            maxLabels: engine.config.maxLabels,
            maxLabelLen: engine.config.maxLabelLen)
        engine.observe(snap)
    }

    // MARK: screen anchor

    /// Developer-annotated screen anchor (`ReproItScreen("name")`), if set. The
    /// route/path is not always reachable from UIKit, so the explicit annotation
    /// is the supported anchor source; nil means a purely structural identity.
    private static var screenAnchor: String?

    /// Set the current screen anchor (route / screen-key / explicit name). Used
    /// as the `A:` prefix of the descriptor; clears with nil.
    static func setScreenAnchor(_ anchor: String?) {
        screenAnchor = anchor
        scheduleSnapshot()
    }

    static func currentAnchor() -> String? { screenAnchor }

    // MARK: canonical accessibility-tree capture

    /// Walk the live view tree under `window` into a canonical ``ReproItNode``
    /// tree (the input to the structural signature) AND, in the same pass,
    /// collect (rawName, tappable) display labels. The root node is forced to
    /// `screen`. Visible = not hidden, non-zero frame, alpha > 0 (mirrors the
    /// runner's AX walk). Localized names never enter the node tree, only the
    /// parallel `labels` list (display-only).
    static func captureTree(
        in window: UIWindow,
        labels: inout [(name: String?, tappable: Bool)]
    ) -> ReproItNode {
        func isVisible(_ v: UIView) -> Bool {
            if v.isHidden || v.alpha <= 0.01 { return false }
            if v.bounds.width <= 0 || v.bounds.height <= 0 { return false }
            return true
        }
        // Build a node for `v` (its role/id/type/icon) and recurse into visible
        // subviews. `isRoot` forces the screen role at the top.
        func build(_ v: UIView, isRoot: Bool) -> ReproItNode? {
            guard isVisible(v) else { return nil } // hidden subtree dropped entirely
            let name = rawName(of: v)
            let tappable = isTappable(v)
            if name != nil || tappable {
                labels.append((name: name, tappable: tappable))
            }
            let role = isRoot ? "screen" : roleOf(v)
            // Layer 2/3 value detection (docs/signature.md "Value-state"): a
            // value-bearing element (text field / live-region status / a Layer-3
            // opt-in node) carries its displayed value + the value_node flag so
            // the oracle folds a bounded value-class into the V: section. A
            // value-bearing node WINS over the transient heuristic, so a
            // role=status live region the transient heuristic would drop is kept.
            let optIn = !isRoot && matchesValueNode(v)
            let valueBearing = !isRoot && (isValueBearingView(v) || optIn)
            let value: String? = valueBearing ? (valueOf(v) ?? "") : nil
            let transient = !isRoot && !valueBearing && isTransient(v)
            var children: [ReproItNode] = []
            for sub in v.subviews {
                if let c = build(sub, isRoot: false) { children.append(c) }
            }
            return ReproItNode(
                role: role,
                id: identifierOf(v),
                type: typeOf(v, role: role),
                icon: iconOf(v),
                transient: transient,
                value: value,
                // The flag makes the canonical is-value-bearing test accept the
                // node even when roleOf normalized its raw value-role (a live
                // region whose structural role is node/text) to a non-value-role.
                valueNode: valueBearing,
                children: children
            )
        }
        return build(window, isRoot: true)
            ?? ReproItNode(role: "screen")
    }

    // MARK: UIKit-trait -> canonical role mapping

    /// Map a UIKit view to a canonical role (docs/signature.md "Roles"). Derived
    /// from accessibility traits + the concrete UIKit class, never from the
    /// visible label. Unknown shapes fall through to `group` (a container) or
    /// `text`/`node`; anything outside the vocabulary normalizes to `node` in
    /// the signature core anyway.
    static func roleOf(_ v: UIView) -> String {
        let traits = v.accessibilityTraits

        // Concrete UIKit classes give the most reliable role.
        switch v {
        case is UISwitch: return "switch"
        case is UISlider: return "slider"
        case is UITextField, is UITextView, is UISearchBar: return "textfield"
        case is UIImageView: return "image"
        case let b as UIButton:
            // A button whose only content is an image reads as an icon button;
            // otherwise a plain button.
            if b.currentTitle?.isEmpty != false, b.currentImage != nil { return "button" }
            return "button"
        case is UILabel: return "text"
        default:
            break
        }

        // Accessibility traits next (covers SwiftUI bridging + custom views).
        if traits.contains(.header) { return "header" }
        if traits.contains(.link) { return "link" }
        if traits.contains(.searchField) { return "textfield" }
        if traits.contains(.image) { return "image" }
        if traits.contains(.button) { return "button" }
        if traits.contains(.adjustable) { return "slider" }
        if traits.contains(.tabBar) { return "tab" }
        if traits.contains(.staticText) { return "text" }

        // Container-ish UIKit classes -> structural containers.
        switch v {
        case is UITableView, is UICollectionView: return "list"
        case is UITableViewCell, is UICollectionViewCell: return "listitem"
        case is UINavigationBar: return "header"
        case is UITabBar: return "tab"
        case is UIControl: return "button" // generic interactive control
        default:
            return "group"
        }
    }

    /// Stable developer identifier: accessibilityIdentifier (the iOS analogue of
    /// a test-id / resource-id). Empty -> nil so it is omitted from the token.
    static func identifierOf(_ v: UIView) -> String? {
        if let id = v.accessibilityIdentifier, !id.isEmpty { return id }
        return nil
    }

    /// Optional input-type refinement, only for textfields (docs/signature.md
    /// `type`). Derived from UITextField keyboard/secure traits, never the text.
    static func typeOf(_ v: UIView, role: String) -> String? {
        guard role == "textfield" else { return nil }
        if let tf = v as? UITextField {
            if tf.isSecureTextEntry { return "password" }
            switch tf.keyboardType {
            case .emailAddress: return "email"
            case .numberPad, .decimalPad, .numbersAndPunctuation, .phonePad: return "number"
            default: break
            }
            return "text"
        }
        if v is UISearchBar { return "search" }
        if v is UITextView { return "text" }
        return "text"
    }

    /// Language-independent icon identity, if the view exposes one. UIKit images
    /// can carry an SF Symbol name (`UIImage.symbolName`-ish via the image's
    /// accessibilityIdentifier or the system symbol name on newer OSes). We use
    /// the image's own accessibilityIdentifier when set, which is the only stable,
    /// language-independent handle available without private API.
    static func iconOf(_ v: UIView) -> String? {
        if let iv = v as? UIImageView, let img = iv.image {
            if #available(iOS 13.0, *) {
                // SF Symbol images expose a stable, language-independent name.
                if let symbol = symbolName(of: img) { return symbol }
            }
        }
        if let b = v as? UIButton, let img = b.currentImage {
            if #available(iOS 13.0, *) {
                if let symbol = symbolName(of: img) { return symbol }
            }
        }
        return nil
    }

    /// Best-effort SF Symbol name for an image. `UIImage` does not publicly
    /// surface the symbol name it was created with on all OS versions; where it
    /// is unavailable this returns nil and the icon is simply omitted (the same
    /// element then hashes without an icon discriminator, matching a plain image).
    @available(iOS 13.0, *)
    static func symbolName(of image: UIImage) -> String? {
        // `isSymbolImage` tells us it is an SF Symbol; the name itself is not
        // public API, but the description may carry it. Kept conservative: only
        // return a value we can read deterministically.
        guard image.isSymbolImage else { return nil }
        // image.value(forKey:) on a private "symbolName" is fragile across OS
        // versions; prefer the accessibilityIdentifier developers can set.
        if let id = image.accessibilityIdentifier, !id.isEmpty { return id }
        return nil
    }

    /// Heuristic: is this view a transient node (toast / snackbar / spinner /
    /// progress / tooltip / badge) that flickers in and out and must be dropped
    /// from the hash (docs/signature.md rule 2)?
    static func isTransient(_ v: UIView) -> Bool {
        if v is UIActivityIndicatorView || v is UIProgressView { return true }
        // accessibilityIdentifier or class name hint (developer-marked).
        let id = (v.accessibilityIdentifier ?? "").lowercased()
        for hint in ["toast", "snackbar", "spinner", "progress", "tooltip", "badge"] {
            if id.contains(hint) { return true }
        }
        let cls = String(describing: type(of: v)).lowercased()
        for hint in ["toast", "snackbar", "spinner", "tooltip", "badge", "hud"] {
            if cls.contains(hint) { return true }
        }
        return false
    }

    // MARK: value-state capture (docs/signature.md "Value-state", Layer 2/3)

    /// True when `v` is a value-bearing element whose displayed value must enter
    /// the canonical `V:` section (docs/signature.md "Value-state", Layer 2).
    /// Detected from UIKit class / accessibility traits only, NEVER from chrome
    /// text, so rule 1's chrome-text exclusion is preserved:
    ///
    ///   * a text field / text view (`UITextField` / `UITextView`)   -> its text,
    ///   * a slider (`UISlider` / `.adjustable` trait)               -> its value,
    ///   * a status / live region: a `.updatesFrequently` element, or any element
    ///     exposing an `accessibilityValue` on a non-chrome (static/none) role
    ///     (the iOS analogue of an aria-live status node).
    ///
    /// Buttons, headers, links and plain labels are chrome and return false even
    /// when they carry an accessibilityValue.
    static func isValueBearingView(_ v: UIView) -> Bool {
        if v is UITextField || v is UITextView || v is UISearchBar { return true }
        if v is UISlider { return true }
        let traits = v.accessibilityTraits
        if traits.contains(.adjustable) { return true }
        // A live region marked as frequently-updating (the iOS aria-live analogue).
        if traits.contains(.updatesFrequently) { return true }
        // A status element: it announces an accessibilityValue but is not chrome.
        // Chrome traits (button/link/header) are excluded so labels/buttons that
        // happen to expose a value never enter the V: section.
        let chrome: UIAccessibilityTraits = [.button, .link, .header]
        if let val = v.accessibilityValue, !val.isEmpty,
           traits.intersection(chrome).isEmpty,
           !(v is UILabel) {
            return true
        }
        return false
    }

    /// The displayed VALUE of a value-bearing view (docs/signature.md
    /// "Value-state"): the entered text for inputs, the slider/adjustable value,
    /// else the accessibilityValue of a status / live region. Returns the raw
    /// string (possibly empty, which classifies to EMPTY). The raw value is
    /// reduced to a bounded value-class by the oracle; it never enters the hash
    /// verbatim.
    static func valueOf(_ v: UIView) -> String? {
        switch v {
        case let tf as UITextField:
            if tf.isSecureTextEntry { return "" } // never read password text
            return tf.text ?? ""
        case let tv as UITextView:
            return tv.text ?? ""
        case let sb as UISearchBar:
            return sb.text ?? ""
        case let s as UISlider:
            return String(s.value)
        default:
            return v.accessibilityValue ?? ""
        }
    }

    /// Layer-3 opt-in value-node selectors (docs/signature.md "Value-state"),
    /// the iOS analogue of `reproit.yaml`'s `value_nodes:` list. Set via
    /// ``setValueNodeSelectors(_:)``; matched against each element so a developer
    /// can mark EXTRA value-bearing nodes (e.g. a score label) whose role is not
    /// in the value-role set. Empty by default.
    static var valueNodeSelectors: [String] = []

    /// Replace the Layer-3 value-node selector list and re-snapshot. Each
    /// selector uses the same grammar as `value_nodes:`: `key:<id>` (matches an
    /// accessibilityIdentifier) or `role:<role>#<idx>` (the idx-th element of
    /// that canonical role, document order).
    static func setValueNodeSelectors(_ selectors: [String]) {
        valueNodeSelectors = selectors
        scheduleSnapshot()
    }

    /// True if `v` matches one of the Layer-3 ``valueNodeSelectors``. `key:<id>`
    /// matches the element's accessibilityIdentifier; `role:<role>#<idx>` matches
    /// the idx-th element of that canonical role in the key window (document
    /// order), mirroring the selector grammar the runners resolve.
    static func matchesValueNode(_ v: UIView) -> Bool {
        if valueNodeSelectors.isEmpty { return false }
        for sel in valueNodeSelectors {
            if sel.isEmpty { continue }
            if sel.hasPrefix("key:") {
                let id = String(sel.dropFirst(4))
                if !id.isEmpty, identifierOf(v) == id { return true }
            } else if sel.hasPrefix("role:"), let hash = sel.firstIndex(of: "#") {
                let role = String(sel[sel.index(sel.startIndex, offsetBy: 5)..<hash])
                guard let idx = Int(sel[sel.index(after: hash)...]), idx >= 0 else { continue }
                if elementAtRoleIndex(role: role, index: idx) === v { return true }
            }
        }
        return false
    }

    /// The idx-th view of canonical `role` under the key window in document
    /// order (the resolution target of a `role:<role>#<idx>` value-node
    /// selector). Walks the visible tree exactly as the snapshot does so the
    /// index namespace matches.
    static func elementAtRoleIndex(role: String, index: Int) -> UIView? {
        guard let window = keyWindow() else { return nil }
        var seen = -1
        var found: UIView?
        func visit(_ v: UIView, isRoot: Bool) {
            if found != nil { return }
            if v.isHidden || v.alpha <= 0.01 { return }
            if v.bounds.width <= 0 || v.bounds.height <= 0 { return }
            if !isRoot {
                let r = reproitNormalizeRole(roleOf(v))
                if r == role {
                    seen += 1
                    if seen == index { found = v; return }
                }
            }
            for sub in v.subviews { visit(sub, isRoot: false) }
        }
        visit(window, isRoot: true)
        return found
    }

    /// Raw accessible name before normalization: accessibilityLabel, then the
    /// element's own title/text (button title, label/textfield text, nav item).
    static func rawName(of v: UIView) -> String? {
        if let a = v.accessibilityLabel, !a.isEmpty { return a }
        switch v {
        case let b as UIButton:
            if let t = b.currentTitle, !t.isEmpty { return t }
            return b.titleLabel?.text
        case let l as UILabel:
            return l.text
        case let f as UITextField:
            return f.text?.isEmpty == false ? f.text : f.placeholder
        case let s as UISearchBar:
            return s.text?.isEmpty == false ? s.text : s.placeholder
        default:
            // Generic accessibility value (e.g. switches) as a last resort.
            return v.accessibilityValue
        }
    }

    /// Tappable = responds to tap: a UIControl, has a tap gesture recognizer, or
    /// carries the `.button`/`.link` accessibility trait.
    static func isTappable(_ v: UIView) -> Bool {
        if let c = v as? UIControl, c.isEnabled { return true }
        let traits = v.accessibilityTraits
        if traits.contains(.button) || traits.contains(.link) { return true }
        if let grs = v.gestureRecognizers,
           grs.contains(where: { $0 is UITapGestureRecognizer && $0.isEnabled }) {
            return true
        }
        return false
    }

    /// The structural selector + accessible name of the deepest tappable view
    /// containing `point` (in `window` coords). Hit-tests by walking the tree and
    /// keeping the deepest match, mirroring the Flutter SDK's `_tapTargetAt`.
    static func tapTargetAt(point: CGPoint, in window: UIWindow) -> (selector: String, label: String?)? {
        var best: (selector: String, label: String?)?
        var perRole: [String: Int] = [:]
        func visit(_ v: UIView) {
            if v.isHidden || v.alpha <= 0.01 { return }
            let inView = window.convert(point, to: v)
            let tappable = isTappable(v)
            let role = roleOf(v)
            let idx = perRole[role] ?? 0
            if tappable { perRole[role] = idx + 1 }
            guard v.point(inside: inView, with: nil) else { return }
            if tappable {
                let selector = identifierOf(v).map { "key:\($0)" } ?? "role:\(reproitNormalizeRole(role))#\(idx)"
                let label = rawName(of: v).flatMap { ReproItName.normalize($0, maxLabelLen: 40) }
                best = (selector, label) // children visited after parents => deepest wins
            }
            for sub in v.subviews { visit(sub) }
        }
        visit(window)
        return best
    }

    // MARK: input fingerprinting (PII-safe tier-3 on-error context)

    /// Collect PII-safe fingerprints of on-screen text fields for the on-error
    /// context. Walks the key window for `UITextField`/`UITextView`, fingerprints
    /// each value to FEATURES, then discards the value. Raw text never escapes.
    ///
    /// HONEST LIMITATION (see README): `isSecureTextEntry` fields (passwords) are
    /// skipped entirely; their values are never read. Fields with no text report
    /// `isEmpty:true`.
    static func collectFieldFingerprints(in window: UIWindow) -> [[String: Any]] {
        var fields: [(field: String, value: String)] = []
        var index = 0
        func labelOf(_ v: UIView, placeholder: String?) -> String {
            if let a = v.accessibilityLabel, !a.isEmpty { return a }
            if let p = placeholder, !p.isEmpty { return p }
            defer { index += 1 }
            return "#\(index)"
        }
        func visit(_ v: UIView) {
            if v.isHidden || v.alpha <= 0.01 { return }
            switch v {
            case let tf as UITextField:
                if tf.isSecureTextEntry { break } // never read password fields
                fields.append((field: labelOf(tf, placeholder: tf.placeholder),
                               value: tf.text ?? ""))
            case let tv as UITextView:
                if tv.isSecureTextEntry { break }
                fields.append((field: labelOf(tv, placeholder: nil),
                               value: tv.text ?? ""))
            default:
                break
            }
            for sub in v.subviews { visit(sub) }
        }
        visit(window)
        return ReproItFingerprint.fingerprintFields(fields)
    }

    /// Build the on-error context (`{fingerprint: [...]}`) on the main thread.
    /// Best-effort: returns nil if nothing is reachable.
    static func errorContext() -> [String: Any]? {
        guard let window = keyWindow() else { return nil }
        let build: () -> [String: Any]? = {
            let fp = collectFieldFingerprints(in: window)
            return fp.isEmpty ? nil : ["fingerprint": fp, "fpVersion": ReproItFingerprint.fpVersion]
        }
        if Thread.isMainThread { return build() }
        return DispatchQueue.main.sync(execute: build)
    }

    // MARK: error hooks

    private static func installExceptionHandler() {
        priorExceptionHandler = NSGetUncaughtExceptionHandler()
        NSSetUncaughtExceptionHandler { exception in
            let stack = exception.callStackSymbols
            // Tier-3 on-error context: PII-safe input fingerprints. The handler
            // runs on the crashing thread (often main); errorContext() reads
            // UIKit safely on the main thread.
            let context = ReproItCapture.errorContext()
            ReproItCapture.engine?.recordError(
                message: "\(exception.name.rawValue): \(exception.reason ?? "")",
                stack: stack,
                source: nil,
                line: nil,
                context: context)
            // Chain to any previously installed handler (e.g. Crashlytics).
            ReproItCapture.priorExceptionHandler?(exception)
        }
    }

    private static func restoreExceptionHandler() {
        NSSetUncaughtExceptionHandler(priorExceptionHandler)
        priorExceptionHandler = nil
    }
}
#endif
