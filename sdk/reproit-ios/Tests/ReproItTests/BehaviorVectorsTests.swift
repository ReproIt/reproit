// Shared behavioral conformance vectors (sdk/capture-behavior-v1.json).
//
// This SDK is one of the two that independently shipped the trigger token
// `user-action`, which is not in the protocol vocabulary; the validator caught
// both. The triggerTokens group pins it so a third instance cannot ship.

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

  func testBoundsVectorsTruncateOverTheBudget() throws {
    let bounds = try XCTUnwrap(vectors()["bounds"] as? [String: Any])
    let cases = try XCTUnwrap(bounds["cases"] as? [[String: Any]])
    for kase in cases {
      guard let input = kase["input"] as? [String: Any],
        let repeatSpec = input["bodyRepeat"] as? [Any],
        let character = repeatSpec.first as? String,
        let count = repeatSpec.last as? Int,
        let expect = kase["expect"] as? [String: Any]
      else { continue }

      let body = String(repeating: character, count: count)
      let side = reproitBoundedExchangeBody(Data(body.utf8), contentType: "text/plain")
      if expect["truncated"] as? Bool == true {
        XCTAssertEqual(side["truncated"] as? Bool, true, "\(kase["name"] ?? "")")
        XCTAssertEqual(side["bodyBytes"] as? Int, expect["bodyBytes"] as? Int)
        XCTAssertEqual(side["bodySha256"] as? String, expect["bodySha256"] as? String)
      } else {
        XCTAssertEqual(side["body"] as? String, body, "\(kase["name"] ?? "")")
      }
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
