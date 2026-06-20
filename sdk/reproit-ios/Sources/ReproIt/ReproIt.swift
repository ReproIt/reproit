// ReproIt iOS, public facade.
//
// One-call entry point: `ReproIt.start(config)` from app launch. The sampling
// decision and the engine live here (Foundation-only). When UIKit is available,
// `start` also wires the live-hierarchy capture in Capture.swift; on a non-UIKit
// host (e.g. `swift test` on macOS) the engine still works and can be driven
// directly with snapshots/taps/errors, which is what the parity test does.

import Foundation

/// The ReproIt telemetry singleton.
public enum ReproIt {
    private static let lock = NSLock()
    private static var engine: ReproItEngine?

    /// Start production telemetry. Safe to call once; later calls are ignored.
    /// Returns the engine if telemetry is active (nil if sampled out).
    @discardableResult
    public static func start(_ config: ReproItConfig) -> ReproItEngine? {
        lock.lock()
        if engine != nil { lock.unlock(); return engine }
        // Sampling decision, made once per session (matches the other SDKs).
        if config.sampleRate < 1.0, Double.random(in: 0..<1) >= config.sampleRate {
            lock.unlock()
            return nil
        }
        let e = ReproItEngine(config: config)
        engine = e
        lock.unlock()

        // Tier-1 auto context (platform/os/locale/tz), PII-safe, Foundation-only.
        e.seedAutoContext()
        e.startFlushTimer()
        #if canImport(UIKit)
        ReproItCapture.attach(to: e)
        #endif
        return e
    }

    /// The active engine, if telemetry is running.
    public static var shared: ReproItEngine? {
        lock.lock(); defer { lock.unlock() }; return engine
    }

    /// Flush queued events immediately (e.g. before a known teardown).
    public static func flush() { shared?.flush() }

    // MARK: context API (mirrors reproit_flutter)

    /// Attach a hashed user id (so the cloud can group "these N users hit it"
    /// without storing identity) plus optional PII-safe context dimensions. The
    /// raw `userId` is hashed to a 16-char `uid`; it is never sent or stored.
    public static func identify(_ userId: String, context: [String: Any]? = nil) {
        shared?.identify(userId, context: context)
    }

    /// Set a single PII-safe context dimension (e.g. role, plan, a count bucket).
    public static func setContext(_ key: String, _ value: Any) {
        shared?.setContext(key, value)
    }

    /// Merge several PII-safe context dimensions at once.
    public static func setContexts(_ values: [String: Any]) {
        shared?.setContexts(values)
    }

    /// The current context dimensions sent with each batch (read-only copy).
    public static var context: [String: Any] { shared?.currentContext ?? [:] }

    /// Annotate the current screen with an explicit anchor (route / screen key /
    /// human name). The anchor becomes the `A:` prefix of the structural
    /// signature, so two screens with the same structure but different anchors
    /// hash to distinct nodes (and the same anchor + same structure merges). Call
    /// from a screen's `viewDidAppear` / `onAppear`. Pass nil to clear.
    public static func screen(_ name: String?) {
        #if canImport(UIKit)
        ReproItCapture.setScreenAnchor(name)
        #endif
    }

    /// Mark EXTRA value-bearing nodes (Layer 3 opt-in, docs/signature.md
    /// "Value-state"). Each selector uses the `value_nodes:` grammar: `key:<id>`
    /// (matches an accessibilityIdentifier) or `role:<role>#<idx>` (the idx-th
    /// element of that canonical role). A matched element's displayed value is
    /// folded into the signature as a bounded value-class even when its role is
    /// not in the value-role set (e.g. a score `UILabel`). Pass an empty list to
    /// clear (the default; fully backward-compatible).
    public static func valueNodes(_ selectors: [String]) {
        #if canImport(UIKit)
        ReproItCapture.setValueNodeSelectors(selectors)
        #endif
    }

    /// Tear down (mainly for tests).
    public static func reset() {
        lock.lock()
        let e = engine
        engine = nil
        lock.unlock()
        e?.stop()
        #if canImport(UIKit)
        ReproItCapture.detach()
        #endif
    }
}
