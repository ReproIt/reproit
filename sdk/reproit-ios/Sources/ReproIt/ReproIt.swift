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
    if engine != nil {
      lock.unlock()
      return engine
    }
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
    ReproItCausalURLProtocol.install(excluding: config.endpoint)
    e.startFlushTimer()
    // Fatal-signal capture (opt-in). Foundation/POSIX only, so it is wired
    // here (not in the UIKit/AppKit capture): drain any spooled crash from a
    // previous launch, restage the current state, then install the handlers.
    // This catches the SIGSEGV/SIGABRT class the NSException hook cannot see.
    if config.catchSignals {
      e.enableCrashSpool(ReproItCrashSpool.shared)
      reproitInstallSignalHandlers()
    }
    // Live capture: UIKit on iOS, AppKit on native macOS. Both walk the
    // platform a11y/view tree into the SAME ReproItNode model and feed the
    // engine; both install the NSException hook. Exactly one is compiled.
    #if canImport(UIKit)
      ReproItCapture.attach(to: e)
    #elseif canImport(AppKit)
      ReproItAppKitCapture.attach(to: e)
    #endif
    return e
  }

  /// Zero-config start: the one-line quickstart. Begins telemetry with sensible
  /// defaults and no required arguments, then delegates to ``start(_:)``.
  /// Active only in a `DEBUG` build; a no-op in release, so shipping this one
  /// line does nothing in a release build. The app id is derived from the main
  /// bundle identifier (falling back to "app"). To run in a release build, or
  /// to override any field, call ``start(_:)`` with an explicit
  /// ``ReproItConfig``.
  @discardableResult
  public static func start() -> ReproItEngine? {
    #if DEBUG
      let id = Bundle.main.bundleIdentifier ?? "app"
      return start(ReproItConfig(appId: id))
    #else
      return nil
    #endif
  }

  /// The active engine, if telemetry is running.
  public static var shared: ReproItEngine? {
    lock.lock()
    defer { lock.unlock() }
    return engine
  }

  /// Flush queued events immediately (e.g. before a known teardown).
  public static func flush() { shared?.flush() }

  /// Capture the current structural state as a tester-observed bug.
  @discardableResult
  public static func captureBug() -> Bool { shared?.captureBug() ?? false }

  /// Declare an indicator's semantic owner and container. The geometry closure
  /// must return screen-coordinate rectangles. ReproIt reports only after two
  /// identical settled samples; animation or unresolved transforms abstain.
  public static func indicator(
    _ id: String, dependentKey: String, ownerKey: String,
    containerKey: String, maxGap: CGFloat = 8,
    sample: @escaping () -> ReproItIndicatorGeometry?
  ) {
    ReproItIndicatorRelations.register(
      id, dependentKey: dependentKey,
      ownerKey: ownerKey, containerKey: containerKey, maxGap: maxGap,
      sample: sample)
  }
  public static func focusedInput(
    _ id: String, sample: @escaping () -> ReproItFocusObservation?, reveal: @escaping () -> Bool
  ) {
    ReproItFocusVisibility.register(id, sample: sample, reveal: reveal)
  }
  public static func preserveState(_ id: String, _ contract: ReproItStatePreservationContract) {
    ReproItStatePreservationContracts.register(id, contract)
  }
  @discardableResult public static func stateBoundary(
    _ kind: ReproItStateBoundary, _ phase: ReproItBoundaryPhase
  ) -> [ReproItContractResult] {
    let results = ReproItStatePreservationContracts.boundary(kind, phase)
    publish(results)
    return results
  }
  public static func actionEffect(_ id: String, _ contract: ReproItActionEffectContract) {
    ReproItActionEffectContracts.register(id, contract)
  }
  @discardableResult public static func actionBegin(_ id: String) -> [ReproItContractResult] {
    let r = ReproItActionEffectContracts.begin(id)
    publish(r)
    return r
  }
  @discardableResult public static func actionEnd(_ id: String) -> [ReproItContractResult] {
    let r = ReproItActionEffectContracts.end(id)
    publish(r)
    return r
  }
  private static func publish(_ results: [ReproItContractResult]) {
    guard let marker = reproitContractMarker(results) else { return }
    if ProcessInfo.processInfo.environment["REPROIT_FUZZ"] == "1" {
      NSLog("%@", marker)
    } else {
      for result in results where result.status == .proven {
        _ = shared?.captureContractBug(identity: result.id, message: result.message ?? result.id)
      }
    }
  }

  /// Register an app invariant: a predicate that must hold in EVERY visited
  /// state. `test` returns true when it holds; returning false or throwing marks
  /// it VIOLATED (a thrown error's text becomes the finding message).
  /// Registration is idempotent by id and INERT in production (evaluated only
  /// under the reproit fuzzer). Mirrors the web SDK's `ReproIt.invariant`. Call
  /// after `start`.
  public static func invariant(_ id: String, _ test: @escaping () throws -> Bool) {
    shared?.invariant(id, test)
  }

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
    #elseif canImport(AppKit)
      ReproItAppKitCapture.setScreenAnchor(name)
    #endif
  }

  /// Mark EXTRA value-bearing nodes (Layer 3 opt-in, docs/signature.md
  /// "Value-state"). Each selector uses the `value_nodes:` grammar: `key:<id>`
  /// (matches an accessibilityIdentifier) or `role:<role>#<idx>` (the idx-th
  /// element of that canonical role). A matched element's displayed value is
  /// folded into the signature as a bounded value-class even when its role is
  /// not in the value-role set (e.g. a score `UILabel`). Pass an empty list to
  /// clear the opt-in selectors.
  public static func valueNodes(_ selectors: [String]) {
    #if canImport(UIKit)
      ReproItCapture.setValueNodeSelectors(selectors)
    #elseif canImport(AppKit)
      ReproItAppKitCapture.setValueNodeSelectors(selectors)
    #endif
  }

  /// Tear down (mainly for tests).
  public static func reset() {
    lock.lock()
    let e = engine
    engine = nil
    lock.unlock()
    e?.stop()
    reproitUninstallSignalHandlers()
    #if canImport(UIKit)
      ReproItCapture.detach()
    #elseif canImport(AppKit)
      ReproItAppKitCapture.detach()
    #endif
    ReproItIndicatorRelations.clear()
    ReproItFocusVisibility.clear()
    ReproItStatePreservationContracts.clear()
    ReproItActionEffectContracts.clear()
  }
}
