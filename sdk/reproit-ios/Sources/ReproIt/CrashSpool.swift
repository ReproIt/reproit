// ReproIt, crash spool (Foundation-only, host-testable).
//
// The NSException hook in Capture.swift catches Obj-C / Swift exceptions and
// flushes synchronously over the network. It cannot see FATAL SIGNALS: a
// `fatalError`, a precondition failure, a wild pointer, a watchdog kill. Those
// are exactly the "production crash reproduced locally" headline cases, so the
// SDK must catch them too. A signal handler runs on a torn-down process, where
// almost nothing is safe (POSIX defines only a small set of async-signal-safe
// functions; Obj-C / Swift allocation, locking, and URLSession are NOT in it).
//
// The design here keeps the signal path allocation-free and lock-free:
//
//   1. OFF the signal path (in `stage`), the SDK serializes the current
//      best-effort crash record (the latest signature + repro path) to JSON and
//      writes it to a spool FILE on disk, plus pre-opens an fd to a tiny
//      "crash-confirmed" marker file. This does all the allocation, locking and
//      I/O buffering while the process is healthy.
//   2. IN the signal handler (`confirmCrashFromSignalHandler`), the ONLY work is
//      a single async-signal-safe `write(2)` of one byte to the pre-opened
//      marker fd, then `fsync`. No Swift objects are touched, nothing is
//      allocated, no lock is taken. The handler then re-raises the default
//      disposition so the process still dies and any paired crash reporter (e.g.
//      Crashlytics) still runs.
//   3. On the NEXT launch (`drainPending`), the SDK reads the spool file. If the
//      marker says the record was crash-confirmed it is resent as an `error`
//      event; either way the spool is cleared. Best-effort delivery: a crash
//      between `write` and `fsync`, or a spool the OS never flushed, is lost, and
//      that is an honest limitation, not a guarantee.
//
// This file is Foundation-only so it builds and is unit-tested on a macOS host
// under `swift test`; the actual signal-handler installation lives in
// Capture.swift (it needs the platform import, but the staging / draining logic
// is here and host-testable).

import Foundation

#if canImport(Darwin)
  import Darwin
#elseif canImport(Glibc)
  import Glibc
#endif

/// A pre-serialized crash record. Holds the JSON bytes that will be POSTed as an
/// `error` event if the staged session ends in a fatal signal.
public struct ReproItCrashRecord: Equatable {
  public let sig: String
  public let path: [ReproItStep]
  public init(sig: String, path: [ReproItStep]) {
    self.sig = sig
    self.path = path
  }
}

/// The fatal signals the SDK opts into catching (docs / README "Honest
/// limitations"). `SIGSEGV` (bad access), `SIGABRT` (a `fatalError` /
/// precondition / `abort`), `SIGILL`, `SIGBUS`, `SIGFPE`, `SIGTRAP`. These are
/// the signals an `NSException` handler never sees.
public let kReproItFatalSignals: [Int32] = [
  SIGSEGV, SIGABRT, SIGILL, SIGBUS, SIGFPE, SIGTRAP,
]

/// On-disk crash spool. Off-signal it stages a pre-serialized record; in the
/// signal handler it does a single allocation-free `write(2)` to confirm the
/// crash; on relaunch it drains any pending record.
///
/// Thread-safety: `stage` / `drainPending` / `clear` take a lock and run on a
/// healthy process. `confirmCrashFromSignalHandler` takes NO lock and does no
/// allocation; it touches only the pre-opened marker fd, which is the only state
/// the signal path reads or writes.
public final class ReproItCrashSpool {
  /// Process-wide spool. The signal handler reaches the marker fd through this
  /// shared instance (a C function pointer cannot capture context), so it is a
  /// singleton like the rest of the SDK's process-global hooks.
  public static let shared = ReproItCrashSpool()

  private let lock = NSLock()
  /// Pre-opened fd to the crash-confirm marker file. Written (one byte) ONLY
  /// from the signal handler. -1 when no session is staged. `Int32` so the
  /// signal path reads a plain word, never a Swift object.
  private var markerFd: Int32 = -1
  private let fs: FileManager
  private let recordURL: URL
  private let markerURL: URL

  /// Build a spool rooted at `directory` (defaults to the app's Caches dir, a
  /// per-app sandboxed location that survives a crash but not a reinstall).
  /// `directory` is injectable so host tests can use a temp dir.
  public init(directory: URL? = nil, fileManager: FileManager = .default) {
    self.fs = fileManager
    let base: URL
    if let directory = directory {
      base = directory
    } else {
      let caches =
        fileManager.urls(for: .cachesDirectory, in: .userDomainMask).first
        ?? URL(fileURLWithPath: NSTemporaryDirectory())
      base = caches.appendingPathComponent("reproit", isDirectory: true)
    }
    try? fileManager.createDirectory(at: base, withIntermediateDirectories: true)
    self.recordURL = base.appendingPathComponent("crash.json")
    self.markerURL = base.appendingPathComponent("crash.confirmed")
  }

  /// Stage `record` for this session (off the signal path). Serializes it to
  /// the spool file and (re)opens the marker fd so the signal handler has a
  /// ready, pre-opened descriptor to write into. Replaces any earlier staged
  /// record: the spool always reflects the latest known state, so the handler
  /// writes the most recent path even though it does no serialization itself.
  @discardableResult
  public func stage(_ record: ReproItCrashRecord, appId: String) -> Bool {
    let obj: [String: Any] = [
      "appId": appId,
      "sig": record.sig,
      "path": record.path.map {
        var step: [String: Any] = ["sig": $0.sig, "action": $0.action]
        if let label = $0.label { step["label"] = label }
        return step
      },
    ]
    guard let data = try? JSONSerialization.data(withJSONObject: obj) else { return false }
    lock.lock()
    defer { lock.unlock() }
    do {
      try data.write(to: recordURL, options: .atomic)
    } catch {
      return false
    }
    // (Re)open the marker fd. Truncate so a stale "confirmed" marker from a
    // previous session that was already drained does not falsely confirm.
    if markerFd >= 0 {
      close(markerFd)
      markerFd = -1
    }
    let fd = markerURL.withUnsafeFileSystemRepresentation { cpath -> Int32 in
      guard let cpath = cpath else { return -1 }
      return open(cpath, O_CREAT | O_WRONLY | O_TRUNC, 0o600)
    }
    guard fd >= 0 else { return false }
    markerFd = fd
    return true
  }

  /// THE signal-path entry point. Async-signal-safe: it takes NO lock, makes
  /// NO allocation, and touches ONLY the pre-opened `markerFd` word. It writes
  /// a single byte to confirm "the staged record ended in a fatal signal" and
  /// `fsync`s it so the marker survives the imminent process death. Every call
  /// here is a POSIX async-signal-safe function (`write`, `fsync` per
  /// signal-safety(7) / Apple's sigaction docs); no Swift runtime is entered.
  public func confirmCrashFromSignalHandler() {
    let fd = markerFd  // plain Int32 read; no object, no lock
    if fd < 0 { return }
    var byte: UInt8 = 1
    // `write` is async-signal-safe. Loop is unnecessary for one byte, but a
    // short partial-write guard costs nothing and stays allocation-free.
    _ = withUnsafePointer(to: &byte) { p in
      write(fd, p, 1)
    }
    fsync(fd)
  }

  /// Drain any spooled record from a previous launch. Returns the record ONLY
  /// when the crash was confirmed by the signal handler (the marker file holds
  /// a confirm byte); a staged-but-not-confirmed record (a clean exit) is
  /// discarded. Always clears the spool so a record is delivered at most once.
  public func drainPending() -> ReproItCrashRecord? {
    lock.lock()
    defer { lock.unlock() }
    defer { clearLocked() }
    let confirmed = (try? Data(contentsOf: markerURL))?.isEmpty == false
    guard confirmed else { return nil }
    guard let data = try? Data(contentsOf: recordURL),
      let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
      let sig = obj["sig"] as? String
    else { return nil }
    let steps =
      (obj["path"] as? [[String: Any]])?.compactMap { step -> ReproItStep? in
        guard let s = step["sig"] as? String, let a = step["action"] as? String else { return nil }
        return ReproItStep(sig: s, action: a, label: step["label"] as? String)
      } ?? []
    return ReproItCrashRecord(sig: sig, path: steps)
  }

  /// Remove the spool files and close the marker fd. Called after a successful
  /// drain and on detach.
  public func clear() {
    lock.lock()
    defer { lock.unlock() }
    clearLocked()
  }

  private func clearLocked() {
    if markerFd >= 0 {
      close(markerFd)
      markerFd = -1
    }
    try? fs.removeItem(at: recordURL)
    try? fs.removeItem(at: markerURL)
  }
}

// MARK: - Fatal-signal handler installation (Foundation-only, all platforms)

/// The C-convention signal handler. A signal handler cannot capture Swift
/// context, so it reaches the spool through the process-wide singleton and
/// re-raises through the saved prior dispositions kept in module globals. The
/// body is deliberately tiny and async-signal-safe: confirm the spooled crash
/// with one `write(2)`, restore the default disposition, and re-raise so the
/// process dies exactly as it would have (and any chained crash reporter runs).
private func reproitSignalHandler(_ sig: Int32) {
  // 1. Async-signal-safe confirm: one pre-opened-fd write, no alloc, no lock.
  ReproItCrashSpool.shared.confirmCrashFromSignalHandler()
  // 2. Chain to any handler installed before us (e.g. a crash reporter), if it
  //    was a real function-style handler (not SIG_DFL / SIG_IGN). We restore
  //    the default first so a re-raise cannot loop back into us. C function
  //    pointers are not Equatable, so SIG_DFL / SIG_IGN are detected by their
  //    bit pattern (they are tiny sentinel "addresses", 0 and 1).
  reproitRestoreDefault(sig)
  if let prior = reproitPriorHandlers[sig], let prior = prior {
    let bits = unsafeBitCast(prior, to: UInt.self)
    if bits > 1 { prior(sig) }  // > SIG_IGN(1): a genuine handler
  }
  // 3. Re-raise so the default disposition (terminate + core/report) runs.
  raise(sig)
}

/// Prior function-style dispositions, saved at install so the handler can chain.
/// Keyed by signal number. Module-global because a C handler has no context.
private var reproitPriorHandlers: [Int32: (@convention(c) (Int32) -> Void)?] = [:]
private var reproitSignalsInstalled = false
private let reproitSignalInstallLock = NSLock()

/// Restore a signal to its default disposition (used before re-raising).
private func reproitRestoreDefault(_ sig: Int32) {
  var action = sigaction()
  action.sa_flags = 0
  sigemptyset(&action.sa_mask)
  #if canImport(Darwin)
    action.__sigaction_u.__sa_handler = SIG_DFL
  #else
    action.sa_handler = SIG_DFL
  #endif
  sigaction(sig, &action, nil)
}

/// Install the fatal-signal handler (idempotent). Off-signal, healthy-process
/// work only: it sets up `sigaction` for each signal in
/// ``kReproItFatalSignals`` and remembers the prior handler so the new handler
/// can chain to it. The handler body (`reproitSignalHandler`) is the
/// async-signal-safe part. Returns true if the handlers were installed (false if
/// already installed).
@discardableResult
public func reproitInstallSignalHandlers() -> Bool {
  reproitSignalInstallLock.lock()
  defer { reproitSignalInstallLock.unlock() }
  if reproitSignalsInstalled { return false }
  reproitSignalsInstalled = true
  for sig in kReproItFatalSignals {
    var newAction = sigaction()
    sigemptyset(&newAction.sa_mask)
    // Plain handler (no SA_SIGINFO): we only need the signal number. We do
    // NOT set SA_RESETHAND; the default is restored manually inside the
    // handler (before re-raising) so we can also chain to a prior handler.
    newAction.sa_flags = 0
    #if canImport(Darwin)
      newAction.__sigaction_u.__sa_handler = reproitSignalHandler
    #else
      newAction.sa_handler = reproitSignalHandler
    #endif
    var old = sigaction()
    if sigaction(sig, &newAction, &old) == 0 {
      #if canImport(Darwin)
        let prior = old.__sigaction_u.__sa_handler
      #else
        let prior = old.sa_handler
      #endif
      reproitPriorHandlers[sig] = prior
    }
  }
  return true
}

/// Restore the default disposition for every fatal signal (mainly for tests /
/// detach so a later test does not inherit our handler).
public func reproitUninstallSignalHandlers() {
  reproitSignalInstallLock.lock()
  defer { reproitSignalInstallLock.unlock() }
  guard reproitSignalsInstalled else { return }
  for sig in kReproItFatalSignals { reproitRestoreDefault(sig) }
  reproitPriorHandlers.removeAll()
  reproitSignalsInstalled = false
}
