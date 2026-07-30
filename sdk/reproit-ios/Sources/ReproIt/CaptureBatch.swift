// capture-batch-v1 emission for iOS production failure occurrences.
//
// Port of the batch builder in sdk/reproit-backend-node/capture.js. A failure
// that carries recorded dependency exchanges ships as a source-neutral causal
// capture batch (`POST <endpoint>/v1/capture-batches`) so the occurrence can
// be re-executed hermetically instead of only re-evaluated. The legacy event
// batch (`/v1/events`) is untouched and still carries every other event.
//
// Event order mirrors the backend SDKs exactly:
//   operation-start, trigger, determinism-envelope checkpoint,
//   dependency (one per exchange), operation-end, observation.

import Foundation

/// Protocol token charset (`validate_token` in reproit-protocol).
func reproitIsProtocolToken(_ value: String?) -> Bool {
  guard let value, !value.isEmpty, value.count <= 128 else { return false }
  return value.allSatisfy { character in
    character.isASCII
      && (character.isLetter || character.isNumber || "._:-".contains(character))
  }
}

/// Builds one capture-batch-v1 for a failure occurrence.
enum ReproItCaptureBatch {
  /// The emitter identity for this SDK, used in every event id.
  static let emitterId = "mobile-ios"

  /// Code identity, in priority order: explicit config, then the common CI and
  /// platform environment. Never shells out to git.
  static func resolveCommit(_ configured: String?) -> String? {
    let environment = ProcessInfo.processInfo.environment
    for candidate in [configured, environment["REPROIT_COMMIT"], environment["GITHUB_SHA"]] {
      if reproitIsProtocolToken(candidate) { return candidate }
    }
    return nil
  }

  /// The determinism envelope: where and when the capture happened, and a seed
  /// that makes REPLAY runs deterministic. Honesty note, carried from the
  /// backend SDKs: the seed does not reproduce the randomness the app drew in
  /// production; it pins the replay's.
  static func envelopeAttributes(observedAtMs: Int64, replaySeed: String? = nil) -> [String: Any] {
    var attributes: [String: Any] = [
      "observedAtMs": observedAtMs,
      "tz": TimeZone.current.identifier,
      "runtime": "swift",
      "os": reproitPlatformName,
      "arch": reproitDeviceArch,
      "replaySeed": replaySeed ?? reproitRandomSeedHex(),
    ]
    let environment = ProcessInfo.processInfo.environment
    if reproitIsProtocolToken(environment["REPROIT_IMAGE_DIGEST"]) {
      attributes["imageDigest"] = environment["REPROIT_IMAGE_DIGEST"]!
    }
    if let build = Bundle.main.infoDictionary?["CFBundleVersion"] as? String,
      reproitIsProtocolToken(build)
    {
      attributes["buildDigest"] = build
    }
    return attributes
  }

  /// Build the batch. Returns nil when the occurrence carries no exchanges:
  /// without them a capsule cannot be re-executed, so claiming the network
  /// capability would be an over-claim and the legacy path already covers the
  /// evidence-only case.
  ///
  /// - Parameters:
  ///   - appId: the cloud project id.
  ///   - sessionId: the SDK session identity (also the batch's trace id).
  ///   - operation: the failing operation name (the screen signature).
  ///   - triggerSubject: what the user did to reach it.
  ///   - triggerValue: the replayable trigger input, already redacted.
  ///   - exchanges: production-recorded dependency exchanges, oldest first.
  ///   - failureSummary: human-readable failure text.
  ///   - failureSignature: the grouping signature.
  static func build(
    appId: String,
    sessionId: String,
    batchId: String,
    operation: String,
    triggerSubject: String,
    triggerValue: Any?,
    exchanges: [ReproItExchange],
    failureSummary: String,
    failureSignature: String,
    buildVersion: String?,
    buildCommit: String?,
    observedAtMs: Int64 = reproitNowMs(),
    replaySeed: String? = nil
  ) -> [String: Any]? {
    guard !exchanges.isEmpty else { return nil }

    var events: [[String: Any]] = []
    var sequence = 0
    var previousId: String?

    func append(_ event: [String: Any]) {
      sequence += 1
      let id = "evt_\(emitterId)_\(sequence)"
      var captured: [String: Any] = [
        "id": id,
        "sequence": sequence,
        // Monotonic offsets are the event ordinal here: the mobile capture
        // path has no per-event high-resolution clock, and inventing one
        // would be a fabricated envelope.
        "monotonicNs": sequence,
        "causalParentIds": previousId.map { [$0] } ?? [],
        "traceId": sessionId,
        "event": event,
      ]
      captured["actor"] = nil
      captured.removeValue(forKey: "actor")
      events.append(captured)
      previousId = id
    }

    append(["kind": "operation-start", "name": operation])
    let value: [String: Any] =
      triggerValue == nil
      ? ["representation": "structural", "shape": ["type": "unknown"]]
      : [
        "representation": "replayable",
        "value": reproitRedactExchangeValue(triggerValue!),
        "redaction": "redacted-at-source",
      ]
    append([
      "kind": "trigger", "trigger": "ui-action", "subject": triggerSubject, "value": value,
    ])
    append([
      "kind": "checkpoint",
      "name": "determinism-envelope",
      "attributes": envelopeAttributes(observedAtMs: observedAtMs, replaySeed: replaySeed),
    ])
    for exchange in exchanges {
      append([
        "kind": "dependency",
        "system": "service",
        "operation": "call",
        "subject": exchange.url,
        "value": [
          "representation": "replayable",
          "value": exchange.jsonObject(),
          "redaction": "redacted-at-source",
        ],
      ])
    }
    append(["kind": "operation-end", "name": operation, "outcome": "failed"])
    append([
      "kind": "observation",
      "failure": [
        "observation": "exception",
        "authority": "runtime-diagnosis",
        "summary": failureSummary,
        "signature": failureSignature,
        "observationPoint": operation,
        "artifactIds": [] as [String],
      ],
    ])

    var batch: [String: Any] = [
      "version": 1,
      "batchId": batchId,
      "projectId": appId,
      "sessionId": sessionId,
      "emitter": [
        "id": emitterId,
        "kind": "runtime-sdk",
        "component": "mobile",
        "runtime": "swift",
      ],
      "observedAt": reproitIso8601(observedAtMs),
      "policy": ["consent": "application-telemetry", "retentionClass": "standard"],
      "capabilities": [
        [
          "capability": "network",
          "completeness": "complete",
          "detail": "outbound dependency exchanges recorded with responses",
        ]
      ],
      "events": events,
      "artifacts": [] as [Any],
    ]
    let commit = resolveCommit(buildCommit)
    var deployment: [String: Any] = [:]
    if reproitIsProtocolToken(buildVersion) { deployment["version"] = buildVersion! }
    if let commit { deployment["commit"] = commit }
    if !deployment.isEmpty { batch["deployment"] = deployment }
    return batch
  }
}

/// Device architecture reported in the envelope.
let reproitDeviceArch: String = {
  #if arch(arm64)
    return "arm64"
  #elseif arch(x86_64)
    return "x86_64"
  #else
    return "unknown"
  #endif
}()

/// 8 random bytes as lowercase hex, matching the backend SDKs' replay seed.
func reproitRandomSeedHex() -> String {
  (0..<8).map { _ in String(format: "%02x", UInt8.random(in: 0...255)) }.joined()
}

/// RFC 3339 timestamp for `observedAt`.
func reproitIso8601(_ millis: Int64) -> String {
  let formatter = DateFormatter()
  formatter.locale = Locale(identifier: "en_US_POSIX")
  formatter.timeZone = TimeZone(secondsFromGMT: 0)
  formatter.dateFormat = "yyyy-MM-dd'T'HH:mm:ss.SSS'Z'"
  return formatter.string(from: Date(timeIntervalSince1970: Double(millis) / 1000.0))
}
