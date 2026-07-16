// ReproIt iOS, the telemetry engine (Foundation-only).
//
// Holds the cross-platform state machine: current signature, repro path,
// pending action, event buffer, flush timer, and the URLSession transport.
// Capture.swift (UIKit) feeds it snapshots, taps and errors; the engine decides
// what edges/errors to emit. Kept UIKit-free so it is exercised by host tests.

import Foundation

/// Thread-safe telemetry engine. All mutable state is guarded by `lock`; the
/// UIKit layer may call in from the main thread (snapshots/taps) and from an
/// uncaught-exception handler (errors), so the engine never assumes a queue.
public final class ReproItEngine {
    private let cfg: ReproItConfig
    private let lock = NSLock()
    private let session: URLSession

    private var buffer: [ReproItEvent] = []
    private var path: [ReproItStep] = []
    // PII-safe context dimensions sent with each batch as `ctx` (the "which
    // users" answer the cloud turns into a cohort discriminator). Populated with
    // tier-1 auto dimensions at start and extended via identify/setContext.
    private var context: [String: Any] = [:]
    private var currentSig: String?
    private var pendingAction: String?   // set at tap/nav time, consumed by the next edge
    private var pendingLabel: String?    // display-only label for pendingAction
    // App-declared invariants (see `invariant`). A plain SDK-owned store, idempotent
    // by id; INERT in production (never evaluated) and consulted only when the SDK
    // detects it is running under the fuzzer, so registration is zero-overhead.
    private var invariants: [(id: String, test: () throws -> Bool)] = []
    private var flushTimer: Timer?
    private var stopped = false
    // Crash spool: when enabled (cfg.catchSignals), the engine restages the
    // latest signature + repro path here on every state change so a fatal-signal
    // handler has a ready, pre-serialized record to confirm (see ReproItCrashSpool).
    private var spool: ReproItCrashSpool?

    public init(config: ReproItConfig, session: URLSession = .shared) {
        self.cfg = config
        self.session = session
        var build: [String: String] = [:]
        if let version = config.buildVersion, !version.isEmpty { build["version"] = version }
        if let commit = config.buildCommit, !commit.isEmpty { build["commit"] = commit }
        if !build.isEmpty { context["build"] = build }
    }

    /// The configuration this engine was started with.
    public var config: ReproItConfig { cfg }

    // MARK: lifecycle

    /// Enable fatal-signal crash spooling (cfg.catchSignals). The engine will
    /// restage the latest signature + repro path into `spool` on every state
    /// change so a signal handler can confirm it with one allocation-free write.
    /// First it drains any record left by a previous launch's fatal signal and
    /// re-emits it as an `error` event (best-effort delivery across launches).
    /// Returns the drained record, if any (for tests / introspection).
    @discardableResult
    public func enableCrashSpool(_ spool: ReproItCrashSpool) -> ReproItCrashRecord? {
        lock.lock()
        self.spool = spool
        lock.unlock()
        let pending = spool.drainPending()
        if let pending = pending {
            // Re-emit a crash from a previous launch. It carries the spooled
            // signature and the full repro path that led to the fatal signal.
            let ev = ReproItEvent.error(
                sig: pending.sig, path: pending.path,
                message: "fatal signal (spooled from previous launch)",
                stack: [], source: nil, line: nil, context: nil, t: reproitNowMs())
            emit(ev)
            flush()
        }
        // Stage the initial (empty-path) record so even a crash before the first
        // state change leaves something to confirm.
        restageSpool()
        return pending
    }

    /// Restage the current signature + repro path into the crash spool (no-op if
    /// spooling is disabled). Called off the signal path on every state change so
    /// the spooled record always reflects the latest known state; the signal
    /// handler itself never serializes anything.
    private func restageSpool() {
        lock.lock()
        guard let spool = spool else { lock.unlock(); return }
        let record = ReproItCrashRecord(sig: currentSig ?? "", path: path)
        let appId = cfg.appId
        lock.unlock()
        spool.stage(record, appId: appId)
    }

    /// Populate the tier-1 auto context dimensions (platform/os/locale/tz). Called
    /// once at start; existing keys are preserved (an earlier identify wins).
    public func seedAutoContext() {
        let auto = ReproItContext.autoDimensions()
        lock.lock()
        for (k, v) in auto where context[k] == nil { context[k] = v }
        lock.unlock()
    }

    // MARK: context (PII-safe cohort dimensions; mirrors the Flutter SDK)

    /// Attach a hashed user id (`uid`) so the cloud can group "these N users hit
    /// it" without storing identity, plus optional context dimensions.
    public func identify(_ userId: String, context extra: [String: Any]? = nil) {
        let uid = ReproItContext.hashUserId(userId)
        lock.lock()
        context["uid"] = uid
        if let extra { for (k, v) in extra { context[k] = v } }
        lock.unlock()
    }

    /// Set a single PII-safe context dimension (e.g. role, plan, a count bucket).
    public func setContext(_ key: String, _ value: Any) {
        lock.lock(); context[key] = value; lock.unlock()
    }

    /// Merge several context dimensions at once.
    public func setContexts(_ values: [String: Any]) {
        lock.lock(); for (k, v) in values { context[k] = v }; lock.unlock()
    }

    /// A read-only copy of the current context (for tests/introspection).
    public var currentContext: [String: Any] {
        lock.lock(); defer { lock.unlock() }; return context
    }

    // MARK: app invariants (self-triggered oracle)

    /// Register an app invariant: a predicate the app declares that must hold in
    /// EVERY visited state (a running total never negative, the selected tab
    /// always highlighted). `test` returns true when it holds; returning false or
    /// throwing marks it VIOLATED (a thrown error's `localizedDescription` becomes
    /// the finding message). Registration is idempotent by id. It is INERT in
    /// production: the predicate is stored but only evaluated when the SDK detects
    /// it is running under the reproit fuzzer (see `underFuzzer`), so this is
    /// zero-overhead until a run reproduces it. Mirrors the web SDK's
    /// `ReproIt.invariant`.
    public func invariant(_ id: String, _ test: @escaping () throws -> Bool) {
        lock.lock(); defer { lock.unlock() }
        if let i = invariants.firstIndex(where: { $0.id == id }) {
            invariants[i].test = test
        } else {
            invariants.append((id: id, test: test))
        }
    }

    /// Whether this app is running under the reproit fuzzer. iOS has no
    /// `navigator.webdriver` equivalent, so the runner launches the app with
    /// `REPROIT_FUZZ=1` in its environment (Appium XCUITest `processArguments.env`)
    /// and the engine gates all invariant evaluation on it. Off => the registry is
    /// never evaluated (production is inert).
    func underFuzzer() -> Bool {
        ProcessInfo.processInfo.environment["REPROIT_FUZZ"] == "1"
    }

    /// Evaluate every registered invariant; return one `{id,message}` entry per
    /// VIOLATED invariant (held ones are omitted). Each predicate is isolated so
    /// one throwing predicate cannot suppress the others. Does NOT apply the fuzzer
    /// gate (that is `invariantMarkerIfUnderFuzzer`); exposed for host tests.
    func evaluateInvariants() -> [[String: String]] {
        lock.lock(); let regs = invariants; lock.unlock()
        var items: [[String: String]] = []
        for reg in regs {
            do {
                let ok = try reg.test()
                if !ok { items.append(["id": reg.id, "message": ""]) }
            } catch {
                items.append(["id": reg.id, "message": error.localizedDescription])
            }
        }
        return items
    }

    /// The `REPROIT_INVARIANT` marker line the engine logs when under the fuzzer
    /// and one or more invariants are violated, else nil (silent). The emitted sig
    /// is left empty ("") so the mobile runner substitutes the sig it is currently
    /// on. `sig` is accepted for symmetry with the other SDKs but not embedded.
    func invariantMarkerIfUnderFuzzer(sig: String) -> String? {
        guard underFuzzer() else { return nil }
        let items = evaluateInvariants()
        if items.isEmpty { return nil }
        let payload: [String: Any] = ["sig": "", "items": items]
        guard let data = try? JSONSerialization.data(withJSONObject: payload),
              let json = String(data: data, encoding: .utf8) else { return nil }
        return "REPROIT_INVARIANT \(json)"
    }

    /// Start the periodic flush timer. Idempotent.
    public func startFlushTimer() {
        lock.lock(); defer { lock.unlock() }
        guard flushTimer == nil, !stopped else { return }
        let timer = Timer(timeInterval: cfg.flushInterval, repeats: true) { [weak self] _ in
            self?.flush()
        }
        // Common run-loop mode so the timer still fires while scrolling.
        RunLoop.main.add(timer, forMode: .common)
        flushTimer = timer
    }

    public func stop() {
        lock.lock()
        stopped = true
        flushTimer?.invalidate()
        flushTimer = nil
        let spool = self.spool
        self.spool = nil
        lock.unlock()
        flush()
        // A clean stop is not a crash: clear the spool so no record lingers.
        spool?.clear()
    }

    // MARK: capture inputs (called by the UIKit layer or tests)

    /// Remember the action that caused the next state change. The UIKit layer
    /// calls this from the tap recognizer (`tap:<selector>`) and navigation hooks
    /// (`nav`); the next differing snapshot consumes it as the edge action.
    public func setPendingAction(_ action: String, label: String? = nil) {
        ReproItCausalURLProtocol.advanceAction()
        lock.lock()
        pendingAction = action
        pendingLabel = label
        lock.unlock()
    }

    /// Feed a fresh snapshot of the current screen. If its signature differs
    /// from the current one, an edge is recorded (initial state uses `load`).
    /// Returns true if an edge was emitted.
    @discardableResult
    public func observe(_ snap: ReproItSnapshot) -> Bool {
        // Self-triggered oracle: the native fuzzer drives this app and cannot call
        // the app's predicates, so the engine evaluates its OWN registered
        // invariants on each settled state and logs a REPROIT_INVARIANT marker for
        // the violations (which the runner scrapes from syslog). Runs only under
        // the fuzzer; a no-op in production. Done outside the lock, before the edge
        // logic, since user predicates run arbitrary app code.
        if let marker = invariantMarkerIfUnderFuzzer(sig: snap.sig) {
            NSLog("%@", marker)
        }
        lock.lock()
        if stopped { lock.unlock(); return false }
        if currentSig == nil {
            currentSig = snap.sig
            appendPathLocked(sig: snap.sig, action: "load")
            let ev = ReproItEvent.edge(
                from: nil, action: "load", to: snap.sig,
                labels: cfg.redactLabels ? nil : snap.labels, t: reproitNowMs())
            lock.unlock()
            emit(ev)
            restageSpool()
            return true
        }
        if snap.sig == currentSig { lock.unlock(); return false }
        let from = currentSig
        let action = pendingAction ?? "auto"
        let label = cfg.redactLabels ? nil : pendingLabel
        pendingAction = nil
        pendingLabel = nil
        currentSig = snap.sig
        appendPathLocked(sig: snap.sig, action: action, label: label)
        let ev = ReproItEvent.edge(
            from: from, action: action, to: snap.sig,
            labels: cfg.redactLabels ? nil : snap.labels, t: reproitNowMs())
        lock.unlock()
        emit(ev)
        restageSpool()
        return true
    }

    /// Record an error carrying the current signature and the full repro path.
    /// Flushes synchronously so the event ships before a crashing process dies.
    /// `context` is the PII-safe tier-3 on-error context (input fingerprints
    /// under `context.fingerprint`); omitted from the wire when nil/empty.
    public func recordError(message: String, stack: [String], source: String?, line: Int?,
                            context: [String: Any]? = nil) {
        lock.lock()
        if stopped { lock.unlock(); return }
        let sig = currentSig ?? ""
        // Include the in-flight action: a tap whose handler throws synchronously
        // sets pendingAction but crashes before the next observe records it, so
        // the bare path stops one step short of the crashing tap.
        var pathCopy = path
        if let pending = pendingAction {
            pathCopy.append(ReproItStep(sig: sig, action: pending, label: cfg.redactLabels ? nil : pendingLabel))
        }
        let trimmed = Array(stack.prefix(8))
        let ev = ReproItEvent.error(
            sig: sig, path: pathCopy, message: message,
            stack: trimmed, source: source, line: line, context: context, t: reproitNowMs())
        lock.unlock()
        emit(ev)
        flushSync()
    }

    // MARK: path / buffer

    private func appendPathLocked(sig: String, action: String, label: String? = nil) {
        path.append(ReproItStep(sig: sig, action: action, label: label))
        if path.count > cfg.pathCap { path.removeFirst(path.count - cfg.pathCap) }
    }

    private func emit(_ ev: ReproItEvent) {
        cfg.onEvent?(ev)
        // No endpoint => onEvent / debug only, never buffer for network.
        if cfg.endpoint == nil {
            if cfg.onEvent == nil {
                let obj = ev.jsonObject(redactLabels: cfg.redactLabels)
                if let d = try? JSONSerialization.data(withJSONObject: obj),
                   let s = String(data: d, encoding: .utf8) {
                    print("[reproit] \(s)")
                }
            }
            return
        }
        lock.lock()
        buffer.append(ev)
        let over = buffer.count >= 50
        lock.unlock()
        if over { flush() }
    }

    // MARK: transport

    /// Drain the buffer and POST it (async). Best-effort; on failure the batch
    /// is re-queued ahead of newer events for one retry (mirrors the Flutter SDK).
    public func flush() {
        guard let request = makeFlushRequest() else { return }
        session.dataTask(with: request.req) { [weak self] _, _, err in
            guard let self, err != nil else { return }
            self.lock.lock()
            self.buffer.insert(contentsOf: request.batch, at: 0)
            self.lock.unlock()
        }.resume()
    }

    /// Synchronous flush for the crash path: blocks briefly so the POST leaves
    /// the device before an uncaught exception tears the process down.
    public func flushSync() {
        guard let request = makeFlushRequest() else { return }
        let sem = DispatchSemaphore(value: 0)
        session.dataTask(with: request.req) { _, _, _ in sem.signal() }.resume()
        _ = sem.wait(timeout: .now() + 2.0)
    }

    private struct FlushRequest { let req: URLRequest; let batch: [ReproItEvent] }

    private func makeFlushRequest() -> FlushRequest? {
        lock.lock()
        guard let endpoint = cfg.endpoint, !buffer.isEmpty else { lock.unlock(); return nil }
        let batch = buffer
        buffer.removeAll(keepingCapacity: true)
        let ctx = context
        lock.unlock()

        guard let url = URL(string: "\(endpoint)/v1/events"),
              let body = ReproItBatch.encode(
                appId: cfg.appId, sentAt: reproitNowMs(),
                ctx: ctx, events: batch, redactLabels: cfg.redactLabels)
        else {
            // Couldn't build a request; put events back so they aren't lost.
            lock.lock(); buffer.insert(contentsOf: batch, at: 0); lock.unlock()
            return nil
        }
        var req = URLRequest(url: url)
        req.httpMethod = "POST"
        req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        if let apiKey = cfg.apiKey {
            req.setValue("Bearer \(apiKey)", forHTTPHeaderField: "Authorization")
        }
        req.httpBody = body
        return FlushRequest(req: req, batch: batch)
    }

    // MARK: test/introspection helpers

    /// Current state signature (nil before the first snapshot).
    public var currentSignature: String? {
        lock.lock(); defer { lock.unlock() }; return currentSig
    }

    /// Snapshot of the repro path (for tests).
    public var currentPath: [ReproItStep] {
        lock.lock(); defer { lock.unlock() }; return path
    }
}
