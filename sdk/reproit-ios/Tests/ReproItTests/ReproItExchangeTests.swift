// Production exchange capture: bounds, redaction, the opt-in gate, and the
// capture-batch-v1 shape. Mirrors sdk/reproit-backend-node/test/instrument.test.js
// so the two SDKs stay honest against the same contract.

import Foundation
import XCTest

@testable import ReproIt

final class ReproItExchangeTests: XCTestCase {
  override func tearDown() {
    ReproItExchangeStore.shared.reset()
    super.tearDown()
  }

  private func exchange(
    requestBody: Data? = nil,
    requestContentType: String? = nil,
    responseBody: Data?,
    responseContentType: String? = "application/json",
    requestHeaders: [String: String] = [:],
    responseHeaders: [String: String] = [:]
  ) -> ReproItExchange {
    ReproItExchange(
      method: "GET", url: "http://pricing.internal/prices?tier=gold",
      requestHeaders: requestHeaders, requestBody: requestBody,
      requestContentType: requestContentType,
      status: 200, responseHeaders: responseHeaders, responseBody: responseBody,
      responseContentType: responseContentType)
  }

  // MARK: shape and bounds

  func testExchangeShapeMatchesTheBackendContract() {
    let body = Data(#"{"prices":[1,2]}"#.utf8)
    let object = exchange(responseBody: body).jsonObject()
    XCTAssertEqual(object["protocol"] as? String, "http")
    let request = object["request"] as? [String: Any]
    XCTAssertEqual(request?["method"] as? String, "GET")
    XCTAssertEqual(request?["url"] as? String, "http://pricing.internal/prices?tier=gold")
    let response = object["response"] as? [String: Any]
    XCTAssertEqual(response?["status"] as? Int, 200)
    let decoded = response?["body"] as? [String: Any]
    XCTAssertEqual(decoded?["prices"] as? [Int], [1, 2])
  }

  func testOversizedBodyKeepsProvableIdentityOnly() {
    let big = Data(String(repeating: "x", count: reproitMaxExchangeBodyBytes + 1).utf8)
    let response = exchange(responseBody: big, responseContentType: "text/plain")
      .jsonObject()["response"] as? [String: Any]
    XCTAssertEqual(response?["truncated"] as? Bool, true)
    XCTAssertEqual(response?["bodyBytes"] as? Int, reproitMaxExchangeBodyBytes + 1)
    XCTAssertNil(response?["body"], "an over-budget body must not ship inline")
    #if canImport(CryptoKit)
      let digest = response?["bodySha256"] as? String
      XCTAssertEqual(digest?.count, 64, "identity stays provable via full sha256")
    #endif
  }

  func testHeadersAreLowercasedCappedAndSecretsMasked() {
    var headers: [String: String] = ["Content-Type": "application/json", "Authorization": "Bearer x"]
    for index in 0..<(reproitMaxExchangeHeaders + 10) { headers["x-pad-\(index)"] = "v" }
    let response = exchange(
      responseBody: Data("{}".utf8), responseHeaders: headers
    ).jsonObject()["response"] as? [String: Any]
    let recorded = response?["headers"] as? [String: String] ?? [:]
    XCTAssertLessThanOrEqual(recorded.count, reproitMaxExchangeHeaders)
    XCTAssertNil(recorded["Authorization"], "header names are lowercased")
    if let authorization = recorded["authorization"] {
      XCTAssertEqual(authorization, "<reproit:secret>")
    }
  }

  // MARK: redaction

  func testRedactionAppliesInsideExchangeBodies() {
    let body = Data(#"{"prices":[1],"apiKey":"sk-live-secret","nested":{"password":"hunter2"}}"#.utf8)
    let response = exchange(responseBody: body).jsonObject()["response"] as? [String: Any]
    let decoded = response?["body"] as? [String: Any]
    let apiKey = decoded?["apiKey"] as? [String: Any]
    let stub = apiKey?["$reproit"] as? [String: Any]
    XCTAssertEqual(stub?["redacted"] as? Bool, true)
    XCTAssertEqual(stub?["type"] as? String, "string")
    XCTAssertEqual(stub?["length"] as? Int, "sk-live-secret".count)
    let nested = decoded?["nested"] as? [String: Any]
    XCTAssertNotNil((nested?["password"] as? [String: Any])?["$reproit"], "redaction recurses")
    XCTAssertEqual(decoded?["prices"] as? [Int], [1], "non-secret values survive")
  }

  func testSecretFieldFoldingMatchesTheBackendKeywordList() {
    for name in ["api-key", "Access Key", "X-Authorization", "signingKey", "idempotencyKey"] {
      XCTAssertTrue(reproitIsSecretField(name), "\(name) must fold to a secret keyword")
    }
    for name in ["username", "quantity", "priceList"] {
      XCTAssertFalse(reproitIsSecretField(name), "\(name) is not secret")
    }
  }

  // MARK: the production gate

  func testProductionCaptureIsOffByDefault() {
    XCTAssertFalse(ReproItConfig(appId: "app").captureExchanges)
    XCTAssertFalse(ReproItExchangeStore.shared.isEnabled)
    ReproItExchangeStore.shared.record(exchange(responseBody: Data("{}".utf8)))
    XCTAssertTrue(
      ReproItExchangeStore.shared.snapshot().isEmpty,
      "a disabled store must retain nothing")
  }

  func testEnabledStoreRetainsAndBoundsExchanges() {
    ReproItExchangeStore.shared.enable()
    for _ in 0..<64 { ReproItExchangeStore.shared.record(exchange(responseBody: Data("{}".utf8))) }
    XCTAssertEqual(ReproItExchangeStore.shared.snapshot().count, 32)
    XCTAssertGreaterThan(ReproItExchangeStore.shared.droppedExchanges, 0)
  }

  func testProductionCaptureEnablesTheStoreOutsideTheRunner() throws {
    // Production capture must never turn on the runner's capsule replay or its
    // marker stream. Meaningful only outside a runner-driven run, so under
    // REPROIT_CAUSAL this skips rather than asserting a precondition it does
    // not control (the precedence case is pinned by the test below).
    try XCTSkipUnless(
      ProcessInfo.processInfo.environment["REPROIT_CAUSAL"] == nil,
      "runner-driven environment: precedence is covered by the companion test")
    ReproItCausalURLProtocol.install(excluding: nil, productionCapture: true)
    defer { ReproItCausalURLProtocol.uninstall() }
    XCTAssertTrue(ReproItExchangeStore.shared.isEnabled)
  }

  func testRunnerEnvironmentTakesPrecedenceOverProductionCapture() throws {
    // Under the runner the harness owns the URL adapter: the production store
    // stays off, so a fuzz session never doubles as a production capture.
    try XCTSkipUnless(
      ProcessInfo.processInfo.environment["REPROIT_CAUSAL"] == "1",
      "requires the runner-driven environment")
    ReproItCausalURLProtocol.install(excluding: nil, productionCapture: true)
    defer { ReproItCausalURLProtocol.uninstall() }
    XCTAssertFalse(
      ReproItExchangeStore.shared.isEnabled,
      "runner-driven install must not enable production capture")
  }

  // MARK: capture batch

  func testCaptureBatchCarriesEnvelopeExchangesAndFailure() {
    let batch = ReproItCaptureBatch.build(
      appId: "app-demo", sessionId: "ses-1", batchId: "cb-ios-1",
      operation: "A:home", triggerSubject: "tap:key:quote",
      triggerValue: ["symbol": "ACME"],
      exchanges: [exchange(responseBody: Data(#"{"prices":null}"#.utf8))],
      failureSummary: "TypeError: prices is null",
      failureSignature: "crash:A:home",
      buildVersion: "1.2.3", buildCommit: "abc123",
      observedAtMs: 1_753_747_200_000, replaySeed: "00ff00ff00ff00ff")
    let events = (batch?["events"] as? [[String: Any]]) ?? []
    let kinds = events.compactMap { ($0["event"] as? [String: Any])?["kind"] as? String }
    XCTAssertEqual(
      kinds, ["operation-start", "trigger", "checkpoint", "dependency", "operation-end", "observation"]
    )
    let checkpoint = events[2]["event"] as? [String: Any]
    XCTAssertEqual(checkpoint?["name"] as? String, "determinism-envelope")
    let attributes = checkpoint?["attributes"] as? [String: Any]
    XCTAssertEqual(attributes?["replaySeed"] as? String, "00ff00ff00ff00ff")
    XCTAssertEqual(attributes?["observedAtMs"] as? Int64, 1_753_747_200_000)
    XCTAssertNotNil(attributes?["tz"])
    let deployment = batch?["deployment"] as? [String: Any]
    XCTAssertEqual(deployment?["version"] as? String, "1.2.3")
    XCTAssertEqual(deployment?["commit"] as? String, "abc123")
    let capability = (batch?["capabilities"] as? [[String: Any]])?.first
    XCTAssertEqual(capability?["capability"] as? String, "network")
    XCTAssertEqual(capability?["completeness"] as? String, "complete")
  }

  func testCaptureBatchIsWithheldWithoutExchanges() {
    let batch = ReproItCaptureBatch.build(
      appId: "app-demo", sessionId: "ses-1", batchId: "cb-ios-1",
      operation: "A:home", triggerSubject: "tap", triggerValue: nil,
      exchanges: [],
      failureSummary: "boom", failureSignature: "crash:A:home",
      buildVersion: nil, buildCommit: nil)
    XCTAssertNil(batch, "a capsule with no exchanges could not be re-executed")
  }

  /// The emitted batch must satisfy the Rust semantic validator, which is the
  /// authority for capture-batch-v1. Skips (loudly) when the workspace binary
  /// is unavailable rather than passing on an unrun check.
  func testEmittedBatchPassesTheProtocolValidator() throws {
    let root = URL(fileURLWithPath: #filePath)
      .deletingLastPathComponent().deletingLastPathComponent()
      .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
    let validator = root.appendingPathComponent("target/debug/capture-validate")
    guard FileManager.default.isExecutableFile(atPath: validator.path) else {
      print("SKIP: capture-validate not built at \(validator.path)")
      return
    }
    let batch = ReproItCaptureBatch.build(
      appId: "app-demo", sessionId: "ses-1", batchId: "cb-ios-1",
      operation: "A:home", triggerSubject: "tap:key:quote", triggerValue: ["symbol": "ACME"],
      exchanges: [exchange(responseBody: Data(#"{"prices":null}"#.utf8))],
      failureSummary: "TypeError: prices is null", failureSignature: "crash:A:home",
      buildVersion: "1.2.3", buildCommit: "abc123")
    let body = try JSONSerialization.data(withJSONObject: try XCTUnwrap(batch))
    let process = Process()
    process.executableURL = validator
    let input = Pipe()
    let output = Pipe()
    process.standardInput = input
    process.standardOutput = output
    process.standardError = output
    try process.run()
    input.fileHandleForWriting.write(body)
    input.fileHandleForWriting.closeFile()
    let text = String(data: output.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? ""
    process.waitUntilExit()
    XCTAssertEqual(process.terminationStatus, 0, "validator rejected the batch: \(text)")
  }
}
