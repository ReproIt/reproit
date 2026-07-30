// Production outbound-exchange capture for the iOS SDK.
//
// Port of sdk/reproit-backend-node/instrument.js (and its Rust twin) to
// Foundation. A recorded exchange is what deterministic local replay stubs,
// so responses are captured verbatim up to a fixed inline budget; an
// over-budget body keeps its byte count and sha256 and is marked truncated,
// which makes replay fail closed with a named reason instead of guessing.
//
// The shape is byte-compatible with the backend SDKs:
//
//   {"protocol":"http",
//    "request":{"method","url","headers","body"},
//    "response":{"status","headers","body"}}
//
// This is deliberately NOT the runner marker shape in Causal.swift. That
// marker is a separate, older contract consumed by the fuzz harness and is
// left byte-unchanged; this type is the production capsule contract.

import Foundation

#if canImport(CryptoKit)
  import CryptoKit
#endif

/// Inline body budget per exchange side. Beyond it the body is dropped and
/// only provable identity (byte count + sha256) remains. Matches
/// `MAX_EXCHANGE_BODY_BYTES` in instrument.js.
let reproitMaxExchangeBodyBytes = 8 * 1024

/// Recorded headers per side, capped to keep events bounded. Matches
/// `MAX_EXCHANGE_HEADERS` in instrument.js.
let reproitMaxExchangeHeaders = 32

/// Secret-shaped field names, redacted at source before anything leaves the
/// device. Byte-identical to `SECRET_PARTS` in the Node SDK's index.js.
private let reproitSecretParts = [
  "password", "passwd", "secret", "token", "authorization", "cookie", "email", "phone",
  "apikey", "publishablekey", "privatekey", "accesskey", "signingkey", "idempotencykey",
]

/// True when a field name looks secret. Non-alphanumerics are stripped and the
/// name lowercased first, so `private-key`, `Access Key`, and `signingKey` all
/// match, exactly like `secretField` in the Node SDK.
func reproitIsSecretField(_ name: String) -> Bool {
  let folded = name.lowercased().filter { $0.isLetter || $0.isNumber }
  return reproitSecretParts.contains { folded.contains($0) }
}

/// Full lowercase-hex sha256 of `data`, or nil when no crypto is available.
/// A nil result never becomes a silent omission: the caller marks the body
/// truncated regardless, and replay fails closed on truncated bodies.
func reproitSha256Hex(_ data: Data) -> String? {
  #if canImport(CryptoKit)
    return SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
  #else
    return nil
  #endif
}

/// The `$reproit` metadata stub that replaces a secret value. Byte-identical
/// to `metadata()` in the Node SDK: type plus length, never the value.
private func reproitRedactedStub(_ value: Any) -> [String: Any] {
  var kind = "null"
  var length: Any = NSNull()
  switch value {
  case let text as String:
    kind = "string"
    length = text.count
  case let number as NSNumber:
    // Bool bridges to NSNumber in Foundation JSON, so check it first.
    if CFGetTypeID(number) == CFBooleanGetTypeID() {
      kind = "boolean"
    } else {
      kind = CFNumberIsFloatType(number) ? "number" : "integer"
    }
  case let items as [Any]:
    kind = "array"
    length = items.count
  case is [String: Any]:
    kind = "object"
  default:
    kind = "null"
  }
  return ["$reproit": ["redacted": true, "type": kind, "length": length]]
}

/// Recursive structural redaction matching the backend SDKs: secret-named
/// fields become a `$reproit` stub, everything else recurses. Shapes and
/// non-secret values survive, so a redacted capsule still replays.
func reproitRedactExchangeValue(_ value: Any) -> Any {
  if let items = value as? [Any] { return items.map(reproitRedactExchangeValue) }
  if let map = value as? [String: Any] {
    var result: [String: Any] = [:]
    for (key, child) in map {
      result[key] = reproitIsSecretField(key) ? reproitRedactedStub(child) : reproitRedactExchangeValue(child)
    }
    return result
  }
  return value
}

/// Bound one exchange body into the wire fields. A JSON body is parsed so the
/// structural redaction above sees fields rather than text; a body past the
/// inline budget keeps only its provable identity.
func reproitBoundedExchangeBody(_ data: Data?, contentType: String?) -> [String: Any] {
  guard let data, !data.isEmpty else { return [:] }
  if data.count > reproitMaxExchangeBodyBytes {
    var bounded: [String: Any] = ["bodyBytes": data.count, "truncated": true]
    if let digest = reproitSha256Hex(data) { bounded["bodySha256"] = digest }
    return bounded
  }
  if (contentType ?? "").lowercased().contains("application/json"),
    let parsed = try? JSONSerialization.jsonObject(with: data, options: [.fragmentsAllowed])
  {
    return ["body": reproitRedactExchangeValue(parsed)]
  }
  guard let text = String(data: data, encoding: .utf8) else {
    // Non-UTF8 payload: identity only, never a lossy transcription.
    var bounded: [String: Any] = ["bodyBytes": data.count, "truncated": true]
    if let digest = reproitSha256Hex(data) { bounded["bodySha256"] = digest }
    return bounded
  }
  return ["body": text]
}

/// Lowercase, cap, and redact header values for one exchange side.
func reproitBoundedExchangeHeaders(_ headers: [String: String]) -> [String: Any] {
  // Sort before capping so the retained subset is deterministic run to run.
  let capped = headers.sorted { $0.key.lowercased() < $1.key.lowercased() }
    .prefix(reproitMaxExchangeHeaders)
  var result: [String: String] = [:]
  for (name, value) in capped {
    let key = name.lowercased()
    result[key] = reproitIsSecretField(key) ? "<reproit:secret>" : value
  }
  return result.isEmpty ? [:] : ["headers": result]
}

/// One captured production exchange, ready to nest in a capture batch.
struct ReproItExchange {
  let method: String
  let url: String
  let requestHeaders: [String: String]
  let requestBody: Data?
  let requestContentType: String?
  let status: Int
  let responseHeaders: [String: String]
  let responseBody: Data?
  let responseContentType: String?

  /// The wire object, field-for-field identical to the backend SDKs.
  func jsonObject() -> [String: Any] {
    var request: [String: Any] = ["method": method, "url": url]
    request.merge(reproitBoundedExchangeHeaders(requestHeaders)) { current, _ in current }
    request.merge(reproitBoundedExchangeBody(requestBody, contentType: requestContentType)) {
      current, _ in current
    }
    var response: [String: Any] = ["status": status]
    response.merge(reproitBoundedExchangeHeaders(responseHeaders)) { current, _ in current }
    response.merge(reproitBoundedExchangeBody(responseBody, contentType: responseContentType)) {
      current, _ in current
    }
    return ["protocol": "http", "request": request, "response": response]
  }
}

/// Process-wide store of production-captured exchanges.
///
/// Bounded and fail-open for the host app: recording never throws into the
/// caller, and overflow drops the OLDEST exchange rather than growing without
/// limit. The store is inert until ``enable()`` is called, which the SDK does
/// only when the app opts in through `ReproItConfig.captureExchanges`.
final class ReproItExchangeStore {
  static let shared = ReproItExchangeStore()

  /// Exchanges retained for the next failure occurrence. A failure ships the
  /// dependency calls that led to it, so this is a flight recorder, not a
  /// full session log.
  private static let maxRetained = 32

  private let lock = NSLock()
  private var enabled = false
  private var retained: [ReproItExchange] = []
  private(set) var droppedExchanges = 0

  /// Turn production recording on. Idempotent.
  func enable() {
    lock.lock()
    enabled = true
    lock.unlock()
  }

  var isEnabled: Bool {
    lock.lock()
    defer { lock.unlock() }
    return enabled
  }

  func record(_ exchange: ReproItExchange) {
    lock.lock()
    defer { lock.unlock() }
    guard enabled else { return }
    retained.append(exchange)
    if retained.count > Self.maxRetained {
      retained.removeFirst(retained.count - Self.maxRetained)
      droppedExchanges += 1
    }
  }

  /// The retained exchanges, oldest first.
  func snapshot() -> [ReproItExchange] {
    lock.lock()
    defer { lock.unlock() }
    return retained
  }

  /// Test and teardown hook.
  func reset() {
    lock.lock()
    enabled = false
    retained.removeAll()
    droppedExchanges = 0
    lock.unlock()
  }
}
