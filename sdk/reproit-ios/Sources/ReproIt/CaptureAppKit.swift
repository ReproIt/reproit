// ReproIt macOS, live view-hierarchy capture (AppKit).
//
// This is the native-macOS counterpart of Capture.swift (UIKit). It walks the
// live NSWindow / NSView (+ NSAccessibility) tree into the SAME canonical
// ``ReproItNode`` model the UIKit capture produces, so Signature.swift is reused
// UNCHANGED and macOS telemetry hashes byte-for-byte identically to iOS, web,
// Flutter, and the Rust oracle. Localized text never enters the node tree (rule
// 1); only structure (roles + ids + types + icons + value-classes) is hashed.
//
// Compiled only when AppKit is available AND UIKit is not (Mac Catalyst imports
// both; there UIKit's Capture.swift owns capture, so this stays out of the way).
// On a plain macOS host this is what `swift test` compiles, which lets the
// descriptor-mapping unit tests below run on the host.
//
// HONEST LIMITATIONS (mirror the UIKit notes):
//   • Snapshotting and click hit-testing read live AppKit objects and so run on
//     the main thread; the engine itself is thread-safe and queue-agnostic.
//   • The accessibility surface read here is AppKit's (NSAccessibility role +
//     accessibilityIdentifier + concrete NSControl classes). Custom-drawn views
//     with no a11y role are invisible by design, same as a screen reader.
//   • The NSException handler does a best-effort synchronous flush; fatal
//     signals are caught only when `catchSignals` is enabled (see CrashSpool).

#if canImport(AppKit) && !canImport(UIKit)
import AppKit
import ObjectiveC

enum ReproItAppKitCapture {
    private static var engine: ReproItEngine?
    private static var debounceWorkItem: DispatchWorkItem?
    private static var clickRecognizer: NSClickGestureRecognizer?
    private static weak var observedView: NSView?
    private static var notifTokens: [NSObjectProtocol] = []
    private static var priorExceptionHandler: (@convention(c) (NSException) -> Void)?
    private static var screenAnchor: String?
    static var valueNodeSelectors: [String] = []

    // MARK: attach / detach

    static func attach(to engine: ReproItEngine) {
        self.engine = engine
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
        installClickRecognizer()
        installObservers()
        scheduleSnapshot()
    }

    private static func teardown() {
        debounceWorkItem?.cancel()
        debounceWorkItem = nil
        if let r = clickRecognizer, let v = observedView {
            v.removeGestureRecognizer(r)
        }
        clickRecognizer = nil
        observedView = nil
        for t in notifTokens { NotificationCenter.default.removeObserver(t) }
        notifTokens.removeAll()
        valueNodeSelectors = []
        screenAnchor = nil
    }

    // MARK: key window / click recognizer

    /// Best-effort key window: the app's keyWindow, then mainWindow, then any.
    static func keyWindow() -> NSWindow? {
        if let w = NSApplication.shared.keyWindow { return w }
        if let w = NSApplication.shared.mainWindow { return w }
        return NSApplication.shared.windows.first { $0.isVisible }
            ?? NSApplication.shared.windows.first
    }

    /// The content view we walk and attach the click recognizer to.
    static func rootView() -> NSView? { keyWindow()?.contentView }

    private static func installClickRecognizer() {
        guard clickRecognizer == nil, let view = rootView() else { return }
        let recognizer = NSClickGestureRecognizer(target: Target.shared, action: #selector(Target.onClick(_:)))
        // delaysPrimaryMouseButtonEvents = false so we never swallow the app's
        // own clicks (the AppKit analogue of cancelsTouchesInView = false).
        recognizer.delaysPrimaryMouseButtonEvents = false
        view.addGestureRecognizer(recognizer)
        clickRecognizer = recognizer
        observedView = view
    }

    /// Forwarding target (NSClickGestureRecognizer needs an NSObject target).
    final class Target: NSObject {
        static let shared = Target()
        @objc func onClick(_ gr: NSClickGestureRecognizer) {
            guard let view = ReproItAppKitCapture.observedView ?? ReproItAppKitCapture.rootView() else { return }
            let point = gr.location(in: view)
            let label = ReproItAppKitCapture.accessibleLabelAt(point: point, in: view)
            ReproItAppKitCapture.engine?.setPendingAction(label.map { "tap:\($0)" } ?? "tap:?")
            ReproItAppKitCapture.scheduleSnapshot()
        }
    }

    // MARK: observers

    private static func installObservers() {
        let nc = NotificationCenter.default
        let names: [Notification.Name] = [
            NSWindow.didBecomeKeyNotification,
            NSWindow.didBecomeMainNotification,
            NSWindow.didResizeNotification,
        ]
        for name in names {
            let token = nc.addObserver(forName: name, object: nil, queue: .main) { _ in
                if observedView == nil || observedView?.window == nil {
                    installClickRecognizer()
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
        debounceWorkItem?.cancel()
        debounceWorkItem = work
        DispatchQueue.main.asyncAfter(deadline: .now() + interval, execute: work)
    }

    private static func takeSnapshot() {
        guard let engine = engine, let view = rootView() else { return }
        var labels: [(name: String?, tappable: Bool)] = []
        let tree = captureTree(in: view, labels: &labels)
        let snap = ReproItSnapshot.build(
            anchor: currentAnchor(),
            tree: tree,
            labels: labels,
            maxLabels: engine.config.maxLabels,
            maxLabelLen: engine.config.maxLabelLen)
        engine.observe(snap)
    }

    // MARK: screen anchor

    static func setScreenAnchor(_ anchor: String?) {
        screenAnchor = anchor
        scheduleSnapshot()
    }

    static func currentAnchor() -> String? { screenAnchor }

    // MARK: canonical accessibility-tree capture

    /// Walk the live NSView tree under `root` into a canonical ``ReproItNode``
    /// tree (the input to the structural signature) AND, in the same pass,
    /// collect (rawName, tappable) display labels. The root is forced to
    /// `screen`. Visible = not hidden, non-zero frame, alphaValue > 0 (the AppKit
    /// analogue of the UIKit walk). Localized names never enter the node tree,
    /// only the parallel `labels` list (display-only).
    static func captureTree(
        in root: NSView,
        labels: inout [(name: String?, tappable: Bool)]
    ) -> ReproItNode {
        func isVisible(_ v: NSView) -> Bool {
            if v.isHidden || v.alphaValue <= 0.01 { return false }
            if v.bounds.width <= 0 || v.bounds.height <= 0 { return false }
            return true
        }
        func build(_ v: NSView, isRoot: Bool) -> ReproItNode? {
            guard isVisible(v) else { return nil } // hidden subtree dropped
            let name = rawName(of: v)
            let tappable = isTappable(v)
            if name != nil || tappable {
                labels.append((name: name, tappable: tappable))
            }
            let role = isRoot ? "screen" : roleOf(v)
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
                valueNode: valueBearing,
                children: children
            )
        }
        return build(root, isRoot: true) ?? ReproItNode(role: "screen")
    }

    // MARK: AppKit -> canonical role mapping

    /// Map an NSView to a canonical role (docs/signature.md "Roles"). Derived
    /// from the concrete NSControl class and the NSAccessibility role, NEVER the
    /// visible label. Unknown shapes fall through to `group`; anything outside
    /// the vocabulary normalizes to `node` in the signature core anyway.
    static func roleOf(_ v: NSView) -> String {
        // Concrete AppKit classes give the most reliable role.
        switch v {
        case is NSSwitch: return "switch"
        case is NSSlider: return "slider"
        case let b as NSButton:
            // Checkbox / radio buttons report a distinct a11y role; a plain
            // push button falls through to `button`.
            switch b.accessibilityRole() {
            case .some(.checkBox): return "checkbox"
            case .some(.radioButton): return "radio"
            default: break
            }
            return "button"
        case let tf as NSTextField:
            // An editable / selectable text field is an input; a static label
            // (the default NSTextField used as a caption) is chrome `text`.
            if tf.isEditable || (tf.isSelectable && tf.isEnabled) { return "textfield" }
            return "text"
        case is NSTextView: return "textfield"
        case is NSSearchField: return "textfield"
        case is NSImageView: return "image"
        case is NSTableView, is NSOutlineView, is NSCollectionView: return "list"
        case is NSTabView: return "tab"
        case is NSProgressIndicator: return "progress"
        default:
            break
        }

        // NSAccessibility role next (covers custom views + AppKit cells).
        // `accessibilityRole()` returns an Optional NSAccessibility.Role.
        switch v.accessibilityRole() {
        case .some(.button): return "button"
        case .some(.checkBox): return "checkbox"
        case .some(.radioButton): return "radio"
        case .some(.slider): return "slider"
        case .some(.image): return "image"
        case .some(.textField): return "textfield"
        case .some(.staticText): return "text"
        case .some(.link): return "link"
        case .some(.list), .some(.table), .some(.outline): return "list"
        case .some(.row), .some(.cell): return "listitem"
        case .some(.tabGroup): return "tab"
        case .some(.menu): return "menu"
        case .some(.menuItem): return "menuitem"
        case .some(.menuBar): return "menu"
        case .some(.group): return "group"
        default:
            break
        }

        return "group"
    }

    /// Stable developer identifier (the macOS analogue of a test-id /
    /// resource-id). Prefers `accessibilityIdentifier()` (what the AX runner
    /// reads); falls back to `NSView.identifier` (the Interface Builder /
    /// restoration id developers commonly set in code or a storyboard). Empty ->
    /// nil so it is omitted from the token.
    static func identifierOf(_ v: NSView) -> String? {
        let axid = v.accessibilityIdentifier()
        if !axid.isEmpty { return axid }
        if let raw = v.identifier?.rawValue, !raw.isEmpty { return raw }
        return nil
    }

    /// Optional input-type refinement, only for textfields (docs/signature.md
    /// `type`). Derived from the field's secure/search kind, never the text.
    static func typeOf(_ v: NSView, role: String) -> String? {
        guard role == "textfield" else { return nil }
        if v is NSSecureTextField { return "password" }
        if v is NSSearchField { return "search" }
        if v is NSTextView { return "text" }
        return "text"
    }

    /// Language-independent icon identity, if the view exposes one. AppKit
    /// `NSImage` may carry an SF Symbol name on recent macOS; the only stable,
    /// non-private handle is the image's `accessibilityDescription`/name when the
    /// developer set one. Returns nil otherwise (the element then hashes without
    /// an icon discriminator, matching a plain image).
    static func iconOf(_ v: NSView) -> String? {
        let image: NSImage?
        switch v {
        case let iv as NSImageView: image = iv.image
        case let b as NSButton: image = b.image
        default: image = nil
        }
        guard let img = image else { return nil }
        if #available(macOS 11.0, *) {
            if img.isTemplate, let name = img.name(), !name.isEmpty { return name }
        } else if let name = img.name(), !name.isEmpty {
            return name
        }
        return nil
    }

    /// Heuristic: is this view a transient node (toast / snackbar / spinner /
    /// progress / tooltip / badge) that flickers in and out and must be dropped
    /// from the hash (docs/signature.md rule 2)?
    static func isTransient(_ v: NSView) -> Bool {
        if v is NSProgressIndicator { return true }
        let id = v.accessibilityIdentifier().lowercased()
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
    /// the canonical `V:` section. Detected from AppKit class / accessibility
    /// role only, NEVER from chrome text, so rule 1's chrome-text exclusion holds:
    ///   * an editable text field / text view / search field -> its text,
    ///   * a slider                                          -> its value,
    ///   * an NSAccessibility staticText/textField with a non-chrome value that
    ///     is not a plain caption (the macOS analogue of an aria-live status).
    static func isValueBearingView(_ v: NSView) -> Bool {
        if v is NSTextView || v is NSSearchField { return true }
        if let tf = v as? NSTextField, tf.isEditable || tf.isSelectable, tf.isEnabled {
            return true
        }
        if v is NSSlider { return true }
        // A status / live region: announces an accessibilityValue but is not a
        // button/link and is not a plain static label.
        if let val = v.accessibilityValue() as? String, !val.isEmpty {
            let role = v.accessibilityRole()
            let chrome: Set<NSAccessibility.Role?> = [.button, .link, .staticText]
            if !chrome.contains(role), !(v is NSTextField) {
                return true
            }
        }
        return false
    }

    /// The displayed VALUE of a value-bearing view (docs/signature.md
    /// "Value-state"): entered text for inputs, the slider value, else the
    /// accessibilityValue of a status / live region. Returns the raw string
    /// (possibly empty, which classifies to EMPTY). The oracle reduces it to a
    /// bounded value-class; it never enters the hash verbatim.
    static func valueOf(_ v: NSView) -> String? {
        switch v {
        case let secure as NSSecureTextField:
            _ = secure
            return "" // never read secure-field text
        case let tf as NSTextField:
            return tf.stringValue
        case let tv as NSTextView:
            return tv.string
        case let sb as NSSearchField:
            return sb.stringValue
        case let s as NSSlider:
            return String(s.doubleValue)
        default:
            return (v.accessibilityValue() as? String) ?? ""
        }
    }

    /// Layer-3 opt-in value-node selectors (the macOS analogue of `reproit.yaml`
    /// `value_nodes:`). Empty by default.
    static func setValueNodeSelectors(_ selectors: [String]) {
        valueNodeSelectors = selectors
        scheduleSnapshot()
    }

    /// True if `v` matches one of the Layer-3 ``valueNodeSelectors``. `key:<id>`
    /// matches the accessibilityIdentifier; `role:<role>#<idx>` matches the
    /// idx-th element of that canonical role (document order), mirroring the
    /// selector grammar the runners resolve.
    static func matchesValueNode(_ v: NSView) -> Bool {
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

    /// The idx-th view of canonical `role` under the root view in document order
    /// (the resolution target of a `role:<role>#<idx>` selector). Walks the
    /// visible tree exactly as the snapshot does so the index namespace matches.
    static func elementAtRoleIndex(role: String, index: Int) -> NSView? {
        guard let root = rootView() else { return nil }
        var seen = -1
        var found: NSView?
        func visit(_ v: NSView, isRoot: Bool) {
            if found != nil { return }
            if v.isHidden || v.alphaValue <= 0.01 { return }
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
        visit(root, isRoot: true)
        return found
    }

    /// Raw accessible name before normalization: accessibilityLabel, then the
    /// element's own title/text (button title, label/field text). Display-only.
    static func rawName(of v: NSView) -> String? {
        if let a = v.accessibilityLabel(), !a.isEmpty { return a }
        switch v {
        case let b as NSButton:
            return b.title.isEmpty ? nil : b.title
        case let tf as NSTextField:
            if !tf.stringValue.isEmpty { return tf.stringValue }
            return tf.placeholderString
        case let sb as NSSearchField:
            return sb.stringValue.isEmpty ? sb.placeholderString : sb.stringValue
        default:
            return v.accessibilityValue() as? String
        }
    }

    /// Tappable = responds to a click: an enabled NSControl, a view carrying the
    /// button/link a11y role, or a view with a click gesture recognizer.
    static func isTappable(_ v: NSView) -> Bool {
        if let c = v as? NSControl, c.isEnabled { return true }
        let role = v.accessibilityRole()
        if role == .button || role == .link || role == .checkBox || role == .radioButton { return true }
        if v.gestureRecognizers.contains(where: { $0 is NSClickGestureRecognizer && $0.isEnabled }) {
            return true
        }
        return false
    }

    /// The accessible name of the deepest tappable, named view containing
    /// `point` (in `root` coords). Walks the tree keeping the deepest match,
    /// mirroring the UIKit / Flutter `accessibleLabelAt`.
    static func accessibleLabelAt(point: CGPoint, in root: NSView) -> String? {
        var best: String?
        func visit(_ v: NSView) {
            if v.isHidden || v.alphaValue <= 0.01 { return }
            let inView = root.convert(point, to: v)
            guard v.bounds.contains(inView) else { return }
            if isTappable(v), let name = rawName(of: v),
               let n = ReproItName.normalize(name, maxLabelLen: 40) {
                best = n // children after parents => deepest wins
            }
            for sub in v.subviews { visit(sub) }
        }
        visit(root)
        return best
    }

    // MARK: error hooks

    private static func installExceptionHandler() {
        priorExceptionHandler = NSGetUncaughtExceptionHandler()
        NSSetUncaughtExceptionHandler { exception in
            let stack = exception.callStackSymbols
            ReproItAppKitCapture.engine?.recordError(
                message: "\(exception.name.rawValue): \(exception.reason ?? "")",
                stack: stack,
                source: nil,
                line: nil,
                context: nil)
            ReproItAppKitCapture.priorExceptionHandler?(exception)
        }
    }

    private static func restoreExceptionHandler() {
        NSSetUncaughtExceptionHandler(priorExceptionHandler)
        priorExceptionHandler = nil
    }
}
#endif
