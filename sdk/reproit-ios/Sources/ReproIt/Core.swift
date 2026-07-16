// ReproIt iOS, canonical contract (Foundation-only).
//
// This file is deliberately free of UIKit so it compiles and runs on a macOS
// host under `swift test`. It owns the parts that MUST match the reproit
// runners and the other SDKs byte-for-byte:
//
//   • the FNV-1a state signature over sorted, '|'-joined accessible names
//   • the accessible-name normalization rule
//   • the edge / error event shapes and the {appId, sentAt, events} batch
//   • the POST /v1/events transport (URLSession), batching, flush, sampling
//
// The live view-hierarchy capture (snapshotting, tap hit-testing, error hooks)
// lives in Capture.swift, guarded by `#if canImport(UIKit)`.

import Foundation

// MARK: - Config (mirrors sdk/reproit-web.js DEFAULTS and the Flutter SDK)

/// Configuration for ``ReproIt/start(_:)``. Field names and defaults mirror the
/// web SDK (`sdk/reproit-web.js`) and `reproit_flutter` so behaviour is
/// consistent across platforms and the cloud graph merges 1:1.
public struct ReproItConfig {
    /// Identifies the app in the cloud (the `appId` in every batch). Required.
    public var appId: String

    /// `POST <endpoint>/v1/events`. If nil, events go only to ``onEvent`` (or,
    /// when that is also nil, an `os_log`/print debug line).
    public var endpoint: String?

    /// Bearer token sent as `Authorization: Bearer <apiKey>` when set.
    public var apiKey: String?

    /// User-visible application version stamped into `ctx.build.version`.
    public var buildVersion: String?

    /// Source revision stamped into `ctx.build.commit`.
    public var buildCommit: String?

    /// Dev hook / custom transport; called for every event in addition to (or
    /// instead of, when ``endpoint`` is nil) the HTTP sink.
    public var onEvent: ((ReproItEvent) -> Void)?

    /// Fraction of sessions that report (0..1). Decided once at ``start(_:)``.
    public var sampleRate: Double

    /// Max distinct labels captured per state (matches the runners).
    public var maxLabels: Int

    /// Labels longer than this are ignored (matches the runners).
    public var maxLabelLen: Int

    /// Max length of the action trail kept for repro paths.
    public var pathCap: Int

    /// How often batched events are flushed.
    public var flushInterval: TimeInterval

    /// When true, only signatures/actions are sent (no human-readable labels).
    public var redactLabels: Bool

    /// Settle window: snapshot once the UI has been quiet this long.
    public var debounce: TimeInterval

    /// Opt-in fatal-signal capture. When true the SDK installs a handler for the
    /// fatal signals (`SIGSEGV`, `SIGABRT`, `SIGILL`, `SIGBUS`, `SIGFPE`,
    /// `SIGTRAP`) that the `NSException` hook can never see (a `fatalError`, a
    /// precondition failure, a memory fault). On a fatal signal the handler does
    /// an async-signal-safe `write(2)` of a pre-serialized crash record to an
    /// on-disk spool, then re-raises the default handler so the process still
    /// dies (and any paired crash reporter still runs). The spooled record is
    /// resent on the next launch. Off by default because a signal handler runs
    /// on a torn-down process: only a narrow, allocation-free write is safe
    /// there, so this trades a small amount of in-handler work for catching the
    /// most severe crashes (see ``ReproItCrashSpool`` for the safety rationale).
    public var catchSignals: Bool

    public init(
        appId: String,
        endpoint: String? = nil,
        apiKey: String? = nil,
        buildVersion: String? = nil,
        buildCommit: String? = nil,
        onEvent: ((ReproItEvent) -> Void)? = nil,
        sampleRate: Double = 1.0,
        maxLabels: Int = 24,
        maxLabelLen: Int = 40,
        pathCap: Int = 60,
        flushInterval: TimeInterval = 5.0,
        redactLabels: Bool = false,
        debounce: TimeInterval = 0.350,
        catchSignals: Bool = false
    ) {
        self.appId = appId
        self.endpoint = endpoint
        self.apiKey = apiKey
        self.buildVersion = buildVersion
        self.buildCommit = buildCommit
        self.onEvent = onEvent
        self.sampleRate = sampleRate
        self.maxLabels = maxLabels
        self.maxLabelLen = maxLabelLen
        self.pathCap = pathCap
        self.flushInterval = flushInterval
        self.redactLabels = redactLabels
        self.debounce = debounce
        self.catchSignals = catchSignals
    }
}

// MARK: - State signature
//
// The canonical STRUCTURAL signature now lives in Signature.swift
// (`ReproItSignature.of(anchor:tree:)`), a byte-for-byte Swift port of the Rust
// oracle. It hashes the normalized accessibility-node tree (roles + ids + types
// + icons + shape), NOT localized names, so it matches the runners and the
// other SDKs and the cloud graph merges 1:1.

// MARK: - PII-safe input fingerprinting (tier-3 on-error context)

/// PII-safe input fingerprinting.
///
/// Some bugs only reproduce with a specific INPUT property: a 312-char name, an
/// emoji, a Turkish dotless "i", an empty field, an RTL string. To reproduce
/// those without storing PII we capture DERIVED FEATURES of on-screen text-field
/// values at error time, never the values themselves; the cloud turns these into
/// a property-matched replay fixture.
///
/// `fingerprintValue` is the load-bearing pure function: Foundation-only,
/// host-testable, identical shape and rules across all five SDKs. It returns
/// FEATURES only and NEVER includes the raw string.
public enum ReproItFingerprint {
    /// Fingerprint schema version. Bumped to 2 when the v2 feature keys (bytes /
    /// scripts / hasCombiningMarks / hasZeroWidth / hasNewline /
    /// leadingTrailingWhitespace) were added. Stamped into the on-error context as
    /// `fpVersion` alongside the fingerprint array. Matches `FP_VERSION` in the
    /// web/Flutter SDKs.
    public static let fpVersion = 2

    /// Derived, PII-safe features of a single text value:
    ///   len: Unicode scalar count (so "José🎉" -> 5)
    ///   bytes: UTF-8 byte length
    ///   charset: "numeric" (all ASCII digits) | "ascii" | "unicode"
    ///   scripts: sorted unique Unicode script buckets present (mixed-script bidi)
    ///   hasEmoji / isEmpty / isRtl: Bool flags
    ///   hasCombiningMarks / hasZeroWidth / hasNewline / leadingTrailingWhitespace
    public static func fingerprintValue(_ value: String) -> [String: Any] {
        let scalars = Array(value.unicodeScalars)
        let len = scalars.count
        let isEmpty = value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        var hasUnicode = false
        var allDigits = !isEmpty
        for s in scalars {
            let v = s.value
            if v > 0x7f { hasUnicode = true }
            if v < 0x30 || v > 0x39 { allDigits = false }
        }
        let charset = hasUnicode ? "unicode" : (allDigits ? "numeric" : "ascii")
        // v2 range checks iterate UTF-16 code units to match the web reference
        // (which iterates JS string charCodeAt units) byte-for-byte.
        let units = Array(value.utf16)
        var hasNewline = false
        for u in units where u == 0x0a || u == 0x0d { hasNewline = true }
        func isWs(_ u: UInt16) -> Bool {
            return u == 0x09 || u == 0x0a || u == 0x0b || u == 0x0c ||
                   u == 0x0d || u == 0x20 || u == 0xa0
        }
        let edgeWs = !units.isEmpty && (isWs(units[0]) || isWs(units[units.count - 1]))
        return [
            "len": len,
            "bytes": value.utf8.count,
            "graphemes": graphemeCount(scalars),
            "charset": charset,
            "scripts": scripts(units),
            "hasEmoji": hasEmoji(scalars),
            "isEmpty": isEmpty,
            "isRtl": isRtl(scalars),
            "hasCombiningMarks": hasCombining(units),
            "hasZeroWidth": hasZeroWidth(units),
            "hasNewline": hasNewline,
            "leadingTrailingWhitespace": edgeWs,
        ]
    }

    /// Zero-width / invisible code points (injection + normalization breakers).
    static func hasZeroWidth(_ units: [UInt16]) -> Bool {
        for u in units {
            if u == 0x200b || u == 0x200c || u == 0x200d || u == 0x2060 || u == 0xfeff {
                return true
            }
        }
        return false
    }

    /// Combining marks (a base char + combining accent renders differently than a
    /// precomposed one; a classic normalization/layout breaker).
    static func hasCombining(_ units: [UInt16]) -> Bool {
        for u in units {
            if (u >= 0x0300 && u <= 0x036f) ||
               (u >= 0x1ab0 && u <= 0x1aff) ||
               (u >= 0x1dc0 && u <= 0x1dff) ||
               (u >= 0x20d0 && u <= 0x20ff) ||
               (u >= 0xfe20 && u <= 0xfe2f) {
                return true
            }
        }
        return false
    }

    static func isCombiningCp(_ c: UInt32) -> Bool {
        return (c >= 0x0300 && c <= 0x036f) ||
               (c >= 0x1ab0 && c <= 0x1aff) ||
               (c >= 0x1dc0 && c <= 0x1dff) ||
               (c >= 0x20d0 && c <= 0x20ff) ||
               (c >= 0xfe20 && c <= 0xfe2f)
    }

    static func graphemeCount(_ scalars: [Unicode.Scalar]) -> Int {
        var n = 0
        var joined = false
        for s in scalars {
            let c = s.value
            if c == 0x200d {
                joined = true
                continue
            }
            if isCombiningCp(c) || (c >= 0xfe00 && c <= 0xfe0f) { continue }
            if joined {
                joined = false
                continue
            }
            n += 1
        }
        return n
    }

    /// The Unicode SCRIPTS present, as a sorted unique list of coarse bucket
    /// names. Ranges are fixed and shared verbatim across all SDKs.
    static func scripts(_ units: [UInt16]) -> [String] {
        var found = Set<String>()
        for u in units {
            if (u >= 0x41 && u <= 0x5a) || (u >= 0x61 && u <= 0x7a) ||
               (u >= 0xc0 && u <= 0x24f) || (u >= 0x1e00 && u <= 0x1eff) {
                found.insert("Latin")
            } else if u >= 0x370 && u <= 0x3ff {
                found.insert("Greek")
            } else if u >= 0x400 && u <= 0x4ff {
                found.insert("Cyrillic")
            } else if u >= 0x590 && u <= 0x5ff {
                found.insert("Hebrew")
            } else if (u >= 0x600 && u <= 0x6ff) || (u >= 0x750 && u <= 0x77f) ||
                      (u >= 0x8a0 && u <= 0x8ff) {
                found.insert("Arabic")
            } else if u >= 0x900 && u <= 0x97f {
                found.insert("Devanagari")
            } else if u >= 0xe00 && u <= 0xe7f {
                found.insert("Thai")
            } else if (u >= 0x3040 && u <= 0x30ff) || (u >= 0x3400 && u <= 0x9fff) ||
                      (u >= 0xac00 && u <= 0xd7a3) || (u >= 0xf900 && u <= 0xfaff) {
                found.insert("CJK")
            }
        }
        return found.sorted()
    }

    /// Any scalar in a strong RTL Unicode block (Arabic / Hebrew / ...).
    static func isRtl(_ scalars: [Unicode.Scalar]) -> Bool {
        for s in scalars {
            let c = s.value
            if (c >= 0x0590 && c <= 0x05ff) || // Hebrew
               (c >= 0x0600 && c <= 0x06ff) || // Arabic
               (c >= 0x0700 && c <= 0x074f) || // Syriac
               (c >= 0x0780 && c <= 0x07bf) || // Thaana
               (c >= 0x07c0 && c <= 0x07ff) || // N'Ko
               (c >= 0x08a0 && c <= 0x08ff) || // Arabic Extended-A
               (c >= 0xfb1d && c <= 0xfb4f) || // Hebrew presentation forms
               (c >= 0xfb50 && c <= 0xfdff) || // Arabic presentation forms-A
               (c >= 0xfe70 && c <= 0xfeff) {   // Arabic presentation forms-B
                return true
            }
        }
        return false
    }

    /// Common emoji / pictographic blocks + regional indicators (flags).
    static func hasEmoji(_ scalars: [Unicode.Scalar]) -> Bool {
        for s in scalars {
            let c = s.value
            if (c >= 0x1f000 && c <= 0x1faff) || // pictographs, emoji, symbols
               (c >= 0x1f1e6 && c <= 0x1f1ff) || // regional indicators (flags)
               (c >= 0x2600 && c <= 0x27bf) ||   // misc symbols + dingbats
               c == 0x2764 ||                      // heavy black heart
               c == 0xfe0f ||                      // variation selector-16
               c == 0x200d {                       // zero-width joiner
                return true
            }
        }
        return false
    }

    /// Fingerprint a list of (field, value) pairs, discarding each value. The
    /// caller (UIKit walk) supplies labels + values; raw values never escape.
    public static func fingerprintFields(
        _ fields: [(field: String, value: String)]
    ) -> [[String: Any]] {
        fields.map { f in
            var obj = fingerprintValue(f.value)
            obj["field"] = f.field
            return obj
        }
    }
}

// MARK: - Accessible-name normalization

public enum ReproItName {
    /// Normalize a raw accessible name: trim, take the first line, then enforce
    /// the length cap. Returns nil if empty or longer than `maxLabelLen`.
    /// (Matches `nameOf` in the web SDK and `_labelOf` in the Flutter SDK.)
    public static func normalize(_ raw: String?, maxLabelLen: Int) -> String? {
        guard let raw else { return nil }
        let firstLine = raw
            .trimmingCharacters(in: .whitespacesAndNewlines)
            .split(separator: "\n", maxSplits: 1, omittingEmptySubsequences: false)
            .first
            .map(String.init) ?? ""
        let name = firstLine.trimmingCharacters(in: .whitespacesAndNewlines)
        if name.isEmpty || name.count > maxLabelLen { return nil }
        return name
    }
}

// MARK: - Snapshot model

/// The result of walking the visible accessibility surface of a screen.
///
/// `sig` is the canonical STRUCTURAL signature of the captured accessibility
/// node tree (see Signature.swift); it never depends on localized text. `labels`
/// is a display-only set of accessible names, kept for `map --show` and edge
/// previews, deduped and capped; it does NOT enter the signature.
public struct ReproItSnapshot {
    public let sig: String
    public let labels: [String]

    public init(sig: String, labels: [String]) {
        self.sig = sig
        self.labels = labels
    }

    /// Build a snapshot from a captured canonical node tree plus the (display-
    /// only) accessible names gathered during the same walk. The UIKit capture
    /// in Capture.swift produces both; this centralizes signature + label
    /// dedupe/cap so the walk and host tests share one code path.
    ///
    /// Rules:
    ///   • signature = canonical structural signature of `tree` under `anchor`
    ///     (roles + ids + types + icons + shape; localized text excluded).
    ///   • labels    = each rawName normalized (first line, trimmed), empties /
    ///     overlong dropped, deduped first-seen, capped at maxLabels. Display
    ///     only; never hashed.
    public static func build(
        anchor: String?,
        tree: ReproItNode,
        labels rawLabels: [(name: String?, tappable: Bool)],
        maxLabels: Int,
        maxLabelLen: Int
    ) -> ReproItSnapshot {
        var ordered: [String] = []
        var seen = Set<String>()
        for el in rawLabels {
            let name = ReproItName.normalize(el.name, maxLabelLen: maxLabelLen)
            if name == nil { continue }
            let n = name!
            if seen.insert(n).inserted { ordered.append(n) }
        }
        let sig = ReproItSignature.of(anchor: anchor, tree: tree)
        return ReproItSnapshot(
            sig: sig,
            labels: Array(ordered.prefix(maxLabels))
        )
    }
}

// MARK: - Events (shapes match the cloud's POST /v1/events contract)

/// A single graph step retained for repro paths.
public struct ReproItStep: Equatable {
    public let sig: String
    public let action: String
    public let label: String?
    public init(sig: String, action: String, label: String? = nil) {
        self.sig = sig
        self.action = action
        self.label = label
    }
}

/// An event the SDK emits. JSON shapes are byte-identical with the web/Flutter
/// SDKs and the cloud's `POST /v1/events` contract.
public enum ReproItEvent {
    /// `{kind:"edge", from?, action, to, labels?, t}`
    case edge(from: String?, action: String, to: String, labels: [String]?, t: Int64)
    /// `{kind:"error", oracle:"crash", sig, path, message, stack, source, line, context?, t}`.
    /// `context` carries the PII-safe tier-3 on-error context (input fingerprints
    /// under `context.fingerprint`); omitted when nil/empty.
    case error(sig: String, path: [ReproItStep], message: String,
               stack: [String], source: String?, line: Int?,
               context: [String: Any]? = nil, t: Int64)

    /// Encode to the JSON object the cloud expects. `JSONSerialization` is used
    /// (not Codable) so key presence/omission exactly matches the JS/Dart SDKs.
    public func jsonObject(redactLabels: Bool) -> [String: Any] {
        switch self {
        case let .edge(from, action, to, labels, t):
            var obj: [String: Any] = ["kind": "edge", "action": action, "to": to, "t": t]
            if let from { obj["from"] = from }
            if !redactLabels, let labels { obj["labels"] = labels }
            return obj
        case let .error(sig, path, message, stack, source, line, context, t):
            var obj: [String: Any] = [
                "kind": "error",
                // A genuine uncaught error IS the `crash` oracle firing; tag it
                // so the cloud can gate ingest on oracle-grade findings.
                "oracle": "crash",
                "sig": sig,
                "path": path.map {
                    var step: [String: Any] = ["sig": $0.sig, "action": $0.action]
                    if !redactLabels, let label = $0.label { step["label"] = label }
                    return step
                },
                "message": message,
                "stack": stack,
                "t": t,
            ]
            if let source { obj["source"] = source }
            if let line { obj["line"] = line }
            if let context, !context.isEmpty { obj["context"] = context }
            return obj
        }
    }
}

// MARK: - Batch encoding

public enum ReproItBatch {
    /// Encode `{appId, sentAt, ctx?, events:[...]}` exactly like the other SDKs.
    /// `ctx` is the PII-safe context map (the "which users" answer the cloud uses
    /// to compute a cohort discriminator); it is included only when non-empty so
    /// key presence matches the web/Flutter batch (`if (_context.isNotEmpty)`).
    public static func encode(
        appId: String,
        sentAt: Int64,
        ctx: [String: Any] = [:],
        events: [ReproItEvent],
        redactLabels: Bool
    ) -> Data? {
        var batch: [String: Any] = [
            "appId": appId,
            "sentAt": sentAt,
            "events": events.map { $0.jsonObject(redactLabels: redactLabels) },
        ]
        if !ctx.isEmpty { batch["ctx"] = ctx }
        return try? JSONSerialization.data(withJSONObject: batch)
    }
}

// MARK: - Context (PII-safe cohort dimensions, mirrors the Flutter `_context`)

public enum ReproItContext {
    /// Tier-1 auto dimensions: zero-PII, Foundation-available, high-signal for
    /// "works for me but not for them" bugs. Mirrors the Flutter SDK's `_start`
    /// auto-context (platform / locale / tz), restricted to what Foundation can
    /// read on any host (no UIKit) so this is exercised by the host test.
    ///   • platform, "ios" on iOS / Catalyst, "macos" on native macOS (matches the
    ///     Dart `defaultTargetPlatform.name`); selected at compile time so each
    ///     build reports the surface it actually captures
    ///   • os      , clean "major.minor" OS version
    ///   • locale  , `Locale.current.identifier` (e.g. "en_US")
    ///   • tz      , `TimeZone.current.identifier` (e.g. "America/New_York")
    public static func autoDimensions(
        processInfo: ProcessInfo = .processInfo,
        locale: Locale = .current,
        timeZone: TimeZone = .current
    ) -> [String: Any] {
        let v = processInfo.operatingSystemVersion
        let os = "\(v.majorVersion).\(v.minorVersion)"
        return [
            "platform": reproitPlatformName,
            "os": os,
            "locale": locale.identifier,
            "tz": timeZone.identifier,
        ]
    }

    /// Hash a user id to a stable, non-reversible 16-char `uid` so the cloud can
    /// group "these N users hit it" without storing identity. Uses CryptoKit
    /// SHA-256 when available; otherwise a documented Foundation FNV-1a-64 fallback
    /// over the UTF-8 bytes (stable across runs, not cryptographic but adequate as
    /// a non-PII grouping key). Mirrors the Flutter SDK's
    /// `sha256(userId).substring(0,16)`.
    public static func hashUserId(_ userId: String) -> String {
        return reproitHashUserId(userId)
    }
}

func reproitNowMs() -> Int64 {
    Int64(Date().timeIntervalSince1970 * 1000.0)
}

/// The platform name reported in the `ctx` map, chosen at compile time to match
/// the capture surface this build actually walks: UIKit -> "ios" (iOS / iPadOS /
/// Mac Catalyst), AppKit native macOS -> "macos". The plain-Foundation host build
/// (no UIKit, AppKit present) reports "macos", which is correct for the native
/// macOS SDK the host build represents.
let reproitPlatformName: String = {
    #if canImport(UIKit)
    return "ios"
    #elseif canImport(AppKit)
    return "macos"
    #else
    return "ios"
    #endif
}()

#if canImport(CryptoKit)
import CryptoKit

/// SHA-256 of the UTF-8 bytes, lowercase hex, truncated to 16 chars. This is the
/// preferred path and is byte-identical to the Flutter SDK's
/// `sha256.convert(utf8.encode(userId)).toString().substring(0, 16)`.
func reproitHashUserId(_ userId: String) -> String {
    let digest = SHA256.hash(data: Data(userId.utf8))
    return digest.map { String(format: "%02x", $0) }.joined().prefix(16).description
}
#else

/// Foundation-only fallback when CryptoKit is unavailable: FNV-1a over the UTF-8
/// bytes, run as two independent 64-bit FNV-1a passes (forward + reversed bytes)
/// and concatenated to 16 hex chars. NOT cryptographic, but deterministic across
/// runs and one-way enough to serve purely as a non-PII grouping key. Documented
/// here so consumers know the `uid` differs from the CryptoKit path on platforms
/// without CryptoKit.
func reproitHashUserId(_ userId: String) -> String {
    func fnv1a64<S: Sequence>(_ bytes: S) -> UInt64 where S.Element == UInt8 {
        var h: UInt64 = 0xcbf2_9ce4_8422_2325
        for b in bytes {
            h ^= UInt64(b)
            h = h &* 0x0000_0100_0000_01b3
        }
        return h
    }
    let utf8 = Array(userId.utf8)
    let a = fnv1a64(utf8)
    let b = fnv1a64(utf8.reversed())
    return String(format: "%016llx", a).prefix(8).description
        + String(format: "%016llx", b).prefix(8).description
}
#endif
