// Shared behavioral conformance vectors (sdk/capture-behavior-v1.json).
//
// This SDK is one of the two that independently shipped the trigger token
// `user-action`, which is not in the protocol vocabulary; the validator caught
// both. The triggerTokens group pins it so a third instance cannot ship.
//
// The remaining groups are harvested from defects, not invented. Each names
// the one it pins:
//
//   bounds            a budget measured in String.count rather than encoded
//                     bytes records 4096 characters of "€" inline, 12288
//                     bytes, past a budget the replayer trusts.
//   headers           the 32 header cap applied in map order recorded a
//                     different subset each run (Go's defect, repeated by
//                     Android). The cap is defined over NAME SORTED order, so
//                     the generated case is fed scrambled on purpose.
//   redaction.type    the $reproit stub must report the ORIGINAL type and
//                     length; a stub claiming "string" for everything makes
//                     the recorded shape unreplayable.
//   redaction.folding secret detection folds case and separators and matches
//                     substrings, so `X-Authorization` and `tokenizer` are
//                     secret and `username` is not.
//   redaction.nesting redaction recurses through objects AND arrays; a
//                     top-level-only scrub shipped nested keys in plaintext.
//   redaction.structure  redaction preserves shape: no key dropped, no array
//                     shortened, an explicit null stays a null VALUE. Swift
//                     drops nils out of a dictionary almost by accident, and
//                     an encoder that did made a capsule say
//                     {"symbol":"ACME"} where production sent
//                     {"prices":null}: replay reproduced a DIFFERENT bug.

import XCTest

@testable import ReproIt

final class BehaviorVectorsTests: XCTestCase {

  private func vectors() throws -> [String: Any] {
    let path = URL(fileURLWithPath: #filePath)
      .deletingLastPathComponent()  // ReproItTests
      .deletingLastPathComponent()  // Tests
      .deletingLastPathComponent()  // reproit-ios
      .deletingLastPathComponent()  // sdk
      .appendingPathComponent("capture-behavior-v1.json")
    let data = try Data(contentsOf: path)
    guard let parsed = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
      throw NSError(domain: "vectors", code: 1)
    }
    return parsed
  }

  func testConstantsMatchTheSharedVectors() throws {
    let constants = try XCTUnwrap(vectors()["constants"] as? [String: Any])
    XCTAssertEqual(constants["maxExchangeBodyBytes"] as? Int, reproitMaxExchangeBodyBytes)
    XCTAssertEqual(constants["maxExchangeHeaders"] as? Int, reproitMaxExchangeHeaders)
  }

  func testRedactionKeyFoldingVectors() throws {
    let redaction = try XCTUnwrap(vectors()["redaction"] as? [String: Any])
    let cases = try XCTUnwrap(redaction["foldingCases"] as? [[String: Any]])
    for kase in cases {
      let field = try XCTUnwrap(kase["field"] as? String)
      let expected = try XCTUnwrap(kase["secret"] as? Bool)
      let out = reproitRedactExchangeValue([field: "value"]) as? [String: Any]
      let value = out?[field] as? [String: Any]
      let redacted = value?["$reproit"] != nil
      XCTAssertEqual(redacted, expected, "field \(field)")
    }
  }

  /// Canonical JSON for one recorded value. Deep equality across `Any` is what
  /// the vectors need, and key order is not part of the contract, so both
  /// sides are serialized with sorted keys and compared as text. Serializing
  /// also keeps `true` distinct from `1` and a null distinct from an absent
  /// key, which an NSDictionary comparison would not.
  private func canonical(_ value: Any) throws -> String {
    let data = try JSONSerialization.data(
      withJSONObject: value, options: [.sortedKeys, .fragmentsAllowed])
    return String(decoding: data, as: UTF8.self)
  }

  /// The canonical form of one case, with the case name LEADING it. XCTest
  /// truncates a long assertion message, and an 8 KiB body is long enough to
  /// swallow a trailing label, so the name goes first or it is not there.
  private func labelled(_ name: String, _ value: Any) throws -> String {
    return "\(name) => \(try canonical(value))"
  }

  /// `{"repeat": ["a", 8192]}` in an expectation stands for the expanded
  /// string, so a vector can pin an 8 KiB body without carrying it.
  private func expanded(_ expect: [String: Any]) -> [String: Any] {
    guard let body = expect["body"] as? [String: Any],
      let spec = body["repeat"] as? [Any],
      let character = spec.first as? String,
      let count = spec.last as? Int
    else { return expect }
    var out = expect
    out["body"] = String(repeating: character, count: count)
    return out
  }

  func testBoundsVectors() throws {
    let bounds = try XCTUnwrap(vectors()["bounds"] as? [String: Any])
    for kase in try XCTUnwrap(bounds["cases"] as? [[String: Any]]) {
      let name = kase["name"] as? String ?? ""
      let input = try XCTUnwrap(kase["input"] as? [String: Any])
      var data: Data?
      if let spec = input["bodyRepeat"] as? [Any],
        let character = spec.first as? String, let count = spec.last as? Int
      {
        data = Data(String(repeating: character, count: count).utf8)
      } else if let text = input["body"] as? String {
        data = Data(text.utf8)
      }
      let contentType = input["contentType"] as? String
      let actual = reproitBoundedExchangeBody(data, contentType: contentType)
      let expect = expanded(try XCTUnwrap(kase["expect"] as? [String: Any]))
      let label = "bounds case \(name)"
      XCTAssertEqual(try labelled(label, actual), try labelled(label, expect))
    }
  }

  /// The generated header table in an order that is neither ascending nor
  /// descending: 17 is coprime with 40, so `index * 17 % count` is a
  /// permutation. A cap applied before sorting therefore keeps a visibly wrong
  /// subset instead of accidentally passing on already-sorted input.
  private func scrambledHeaders(_ spec: [String: Any]) throws -> [String: String] {
    let count = try XCTUnwrap(spec["headerCount"] as? Int)
    let pattern = try XCTUnwrap(spec["namePattern"] as? String)
    let value = try XCTUnwrap(spec["value"] as? String)
    var headers: [String: String] = [:]
    for step in 0..<count {
      headers[String(format: pattern, (step * 17) % count)] = value
    }
    return headers
  }

  func testHeaderVectors() throws {
    let headers = try XCTUnwrap(vectors()["headers"] as? [String: Any])
    for kase in try XCTUnwrap(headers["cases"] as? [[String: Any]]) {
      let name = kase["name"] as? String ?? ""
      let expect = try XCTUnwrap(kase["expect"] as? [String: Any])
      if let input = kase["input"] as? [String: Any] {
        let given = try XCTUnwrap(input["headers"] as? [String: String])
        let actual = reproitBoundedExchangeHeaders(given)
        XCTAssertEqual(
          try labelled("headers case \(name)", actual),
          try labelled("headers case \(name)", expect))
        continue
      }
      let spec = try XCTUnwrap(kase["inputGenerated"] as? [String: Any])
      let actual = reproitBoundedExchangeHeaders(try scrambledHeaders(spec))
      let kept = try XCTUnwrap(actual["headers"] as? [String: String]).keys.sorted()
      XCTAssertEqual(kept.count, expect["headerCount"] as? Int, "headers case \(name)")
      // The cap must be over sorted names, not the order the headers arrived.
      XCTAssertEqual(kept.first, expect["firstName"] as? String)
      XCTAssertEqual(kept.last, expect["lastName"] as? String)
    }
  }

  private func redactionGroup(_ group: String) throws -> [[String: Any]] {
    let redaction = try XCTUnwrap(vectors()["redaction"] as? [String: Any])
    return try XCTUnwrap(redaction[group] as? [[String: Any]])
  }

  func testRedactionTypeVectors() throws {
    for kase in try redactionGroup("typeCases") {
      let input = try XCTUnwrap(kase["input"])
      let expect = try XCTUnwrap(kase["expect"])
      let label = try canonical(input)
      XCTAssertEqual(
        try labelled(label, reproitRedactExchangeValue(input)), try labelled(label, expect))
    }
  }

  func testRedactionNestingVectors() throws {
    for kase in try redactionGroup("nestingCases") {
      let input = try XCTUnwrap(kase["input"])
      let expect = try XCTUnwrap(kase["expect"])
      let label = try canonical(input)
      XCTAssertEqual(
        try labelled(label, reproitRedactExchangeValue(input)), try labelled(label, expect))
    }
  }

  // A dropped key, a shortened array, or a null collapsed to nothing all change
  // the shape the replay matcher walks, so the capsule stops matching the call
  // it was recorded from.
  func testRedactionStructureVectors() throws {
    for kase in try redactionGroup("structureCases") {
      let name = kase["name"] as? String ?? ""
      let input = try XCTUnwrap(kase["input"])
      let expect = try XCTUnwrap(kase["expect"])
      let label = "structure case \(name)"
      XCTAssertEqual(
        try labelled(label, reproitRedactExchangeValue(input)), try labelled(label, expect))
    }
  }

  // The defect this SDK shipped: `user-action` is not in the vocabulary.
  func testTriggerTokenIsInTheProtocolVocabulary() throws {
    let tokens = try XCTUnwrap(vectors()["triggerTokens"] as? [String: Any])
    let bySdkKind = try XCTUnwrap(tokens["bySdkKind"] as? [String: String])
    let token = try XCTUnwrap(bySdkKind["mobile"])
    let allowed = try XCTUnwrap(tokens["allowed"] as? [String])
    XCTAssertTrue(allowed.contains(token))

    let source = URL(fileURLWithPath: #filePath)
      .deletingLastPathComponent()
      .deletingLastPathComponent()
      .deletingLastPathComponent()
      .appendingPathComponent("Sources/ReproIt/CaptureBatch.swift")
    let text = try String(contentsOf: source, encoding: .utf8)
    XCTAssertTrue(text.contains("\"\(token)\""), "CaptureBatch.swift must emit \(token)")
    for bad in try XCTUnwrap(tokens["rejected"] as? [String]) {
      XCTAssertFalse(text.contains("\"\(bad)\""), "must not emit \(bad)")
    }
  }
}
