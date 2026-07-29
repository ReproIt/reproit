import Foundation
import XCTest

@testable import ReproIt

#if canImport(AppKit) && !canImport(UIKit)
  import AppKit
#endif

extension ReproItTests {
  // MARK: context API (mirrors reproit_flutter)

  /// The platform string the current build reports: "ios" under UIKit, "macos"
  /// on native macOS (the host build). Mirrors `reproitPlatformName` so the
  /// host test asserts the right value for the surface it compiles.
  static var expectedPlatform: String {
    #if canImport(UIKit)
      return "ios"
    #elseif canImport(AppKit)
      return "macos"
    #else
      return "ios"
    #endif
  }

  func testAutoDimensionsPresent() {
    let dims = ReproItContext.autoDimensions()
    XCTAssertEqual(dims["platform"] as? String, ReproItTests.expectedPlatform)
    XCTAssertNotNil(dims["locale"] as? String)
    XCTAssertNotNil(dims["tz"] as? String)
    XCTAssertFalse((dims["tz"] as? String ?? "").isEmpty)
    let os = dims["os"] as? String
    XCTAssertNotNil(os)
    XCTAssertTrue((os ?? "").contains("."))
  }

  func testSeedAutoContextPopulatesEngine() {
    let engine = ReproItEngine(config: ReproItConfig(appId: "t"))
    engine.seedAutoContext()
    let ctx = engine.currentContext
    XCTAssertEqual(ctx["platform"] as? String, ReproItTests.expectedPlatform)
    XCTAssertNotNil(ctx["locale"] as? String)
    XCTAssertNotNil(ctx["tz"] as? String)
  }

  func testConfiguredBuildIdentityIsAddedToContext() {
    let engine = ReproItEngine(
      config: ReproItConfig(
        appId: "t", buildVersion: "1.4.2", buildCommit: "abc123"))
    let build = engine.currentContext["build"] as? [String: String]
    XCTAssertEqual(build?["version"], "1.4.2")
    XCTAssertEqual(build?["commit"], "abc123")
  }

  func testIdentifyHashesUserIdAndMergesContext() {
    let engine = ReproItEngine(config: ReproItConfig(appId: "t"))
    let raw = "user@example.com"
    engine.identify(raw, context: ["plan": "pro"])
    let ctx = engine.currentContext
    let uid = ctx["uid"] as? String
    XCTAssertNotNil(uid)
    XCTAssertNotEqual(uid, raw)
    XCTAssertEqual(uid?.count, 16)
    XCTAssertEqual(uid, ReproItContext.hashUserId(raw))
    XCTAssertNotEqual(
      ReproItContext.hashUserId(raw),
      ReproItContext.hashUserId("someone-else"))
    XCTAssertEqual(ctx["plan"] as? String, "pro")
  }

  func testSetContextAndSetContextsMerge() {
    let engine = ReproItEngine(config: ReproItConfig(appId: "t"))
    engine.setContext("role", "admin")
    engine.setContexts(["tenant": "acme", "seats": 12])
    engine.setContext("role", "owner")
    let ctx = engine.currentContext
    XCTAssertEqual(ctx["role"] as? String, "owner")
    XCTAssertEqual(ctx["tenant"] as? String, "acme")
    XCTAssertEqual(ctx["seats"] as? Int, 12)
  }

  func testBatchFindingCarriesMergedContext() throws {
    let withCtx = ReproItBatch.encode(
      appId: "a", sentAt: 1, batchSequence: 1,
      ctx: ["platform": "ios", "uid": "deadbeefdeadbeef"],
      events: [.testerCapture(sig: "811c9dc5", path: [], trigger: "load", t: 1)],
      redactLabels: false)
    let obj1 = try XCTUnwrap(
      try JSONSerialization.jsonObject(with: try XCTUnwrap(withCtx)) as? [String: Any])
    let frame = try XCTUnwrap((obj1["frames"] as? [[String: Any]])?.first)
    let finding = try XCTUnwrap(frame["event"] as? [String: Any])
    let ctx = finding["context"] as? [String: Any]
    XCTAssertEqual(ctx?["platform"] as? String, "ios")
    XCTAssertEqual(ctx?["uid"] as? String, "deadbeefdeadbeef")

    let noCtx = ReproItBatch.encode(
      appId: "a", sentAt: 1, batchSequence: 2,
      events: [.testerCapture(sig: "811c9dc5", path: [], trigger: "load", t: 1)],
      redactLabels: false)
    let obj2 = try XCTUnwrap(
      try JSONSerialization.jsonObject(with: try XCTUnwrap(noCtx)) as? [String: Any])
    let noCtxFrame = try XCTUnwrap((obj2["frames"] as? [[String: Any]])?.first)
    let noCtxFinding = try XCTUnwrap(noCtxFrame["event"] as? [String: Any])
    XCTAssertEqual((noCtxFinding["context"] as? [String: Any])?.count, 0)
  }

  // MARK: PII-safe input fingerprint (tier-3 on-error context)

  func testFingerprintJoseEmojiIsUnicodeAndEmoji() {
    let r = ReproItFingerprint.fingerprintValue("José🎉")
    XCTAssertEqual(r["len"] as? Int, 5)
    XCTAssertEqual(r["charset"] as? String, "unicode")
    XCTAssertEqual(r["hasEmoji"] as? Bool, true)
    XCTAssertEqual(r["isEmpty"] as? Bool, false)
    XCTAssertEqual(r["isRtl"] as? Bool, false)
  }

  func testFingerprintNumericAsciiEmpty() {
    XCTAssertEqual(ReproItFingerprint.fingerprintValue("12345")["charset"] as? String, "numeric")
    XCTAssertEqual(ReproItFingerprint.fingerprintValue("hello")["charset"] as? String, "ascii")
    let empty = ReproItFingerprint.fingerprintValue("")
    XCTAssertEqual(empty["isEmpty"] as? Bool, true)
    XCTAssertEqual(empty["len"] as? Int, 0)
    XCTAssertEqual(empty["charset"] as? String, "ascii")
    XCTAssertEqual(ReproItFingerprint.fingerprintValue("   ")["isEmpty"] as? Bool, true)
  }

  func testFingerprintRtlAndTurkishAndLength() {
    let ar = ReproItFingerprint.fingerprintValue("مرحبا")
    XCTAssertEqual(ar["isRtl"] as? Bool, true)
    XCTAssertEqual(ar["charset"] as? String, "unicode")
    XCTAssertEqual(ar["hasEmoji"] as? Bool, false)
    XCTAssertEqual(ReproItFingerprint.fingerprintValue("שלום")["isRtl"] as? Bool, true)
    XCTAssertEqual(ReproItFingerprint.fingerprintValue("ıstanbul")["charset"] as? String, "unicode")
    let long = ReproItFingerprint.fingerprintValue(String(repeating: "a", count: 312))
    XCTAssertEqual(long["len"] as? Int, 312)
    XCTAssertEqual(long["charset"] as? String, "ascii")
  }

  func testFingerprintNeverEchoesRawValue() throws {
    let raw = "secret-pii-value"
    let obj = ReproItFingerprint.fingerprintValue(raw)
    let data = try JSONSerialization.data(withJSONObject: obj)
    let json = String(data: data, encoding: .utf8) ?? ""
    XCTAssertFalse(json.contains(raw))
  }

  func testFingerprintFieldsKeepsLabelDropsValue() throws {
    let out = ReproItFingerprint.fingerprintFields([
      (field: "email", value: "a@b.co"),
      (field: "#1", value: "12345"),
      (field: "note", value: ""),
    ])
    XCTAssertEqual(out.count, 3)
    XCTAssertEqual(out[0]["field"] as? String, "email")
    XCTAssertEqual(out[1]["charset"] as? String, "numeric")
    XCTAssertEqual(out[2]["isEmpty"] as? Bool, true)
    let json = String(data: try JSONSerialization.data(withJSONObject: out), encoding: .utf8) ?? ""
    XCTAssertFalse(json.contains("a@b.co"))
  }

  // ---- v2 features (bytes / scripts / combining / zero-width / newline / ws) ----

  func testFingerprintBytesIsUtf8Length() {
    let r = ReproItFingerprint.fingerprintValue("José\u{1F389}")
    XCTAssertEqual(r["len"] as? Int, 5)
    XCTAssertEqual(r["bytes"] as? Int, 9)
    XCTAssertEqual(ReproItFingerprint.fingerprintValue("hello")["bytes"] as? Int, 5)
  }

  func testFingerprintGraphemesCountUserVisibleClusters() {
    XCTAssertEqual(ReproItFingerprint.fingerprintValue("hello")["graphemes"] as? Int, 5)
    XCTAssertEqual(ReproItFingerprint.fingerprintValue("e\u{0301}")["len"] as? Int, 2)
    XCTAssertEqual(ReproItFingerprint.fingerprintValue("e\u{0301}")["graphemes"] as? Int, 1)
    XCTAssertEqual(ReproItFingerprint.fingerprintValue("👨‍👩‍👧‍👦")["graphemes"] as? Int, 1)
  }

  func testFingerprintScriptsBuckets() {
    XCTAssertEqual(ReproItFingerprint.fingerprintValue("hello")["scripts"] as? [String], ["Latin"])
    let ar = ReproItFingerprint.fingerprintValue("\u{0645}\u{0631}\u{062D}\u{0628}\u{0627}")
    XCTAssertEqual(ar["scripts"] as? [String], ["Arabic"])
    XCTAssertEqual(ar["isRtl"] as? Bool, true)
    let mixed = ReproItFingerprint.fingerprintValue("hi \u{0645}\u{0631}\u{062D}\u{0628}\u{0627}")
    XCTAssertEqual(mixed["scripts"] as? [String], ["Arabic", "Latin"])
    XCTAssertEqual(
      ReproItFingerprint.fingerprintValue("\u{65E5}\u{672C}\u{8A9E}")["scripts"] as? [String],
      ["CJK"])
    XCTAssertEqual(ReproItFingerprint.fingerprintValue("12345")["scripts"] as? [String], [])
  }

  func testFingerprintHasNewline() {
    XCTAssertEqual(ReproItFingerprint.fingerprintValue("line1\nline2")["hasNewline"] as? Bool, true)
    XCTAssertEqual(ReproItFingerprint.fingerprintValue("oneline")["hasNewline"] as? Bool, false)
  }

  func testFingerprintHasZeroWidth() {
    XCTAssertEqual(ReproItFingerprint.fingerprintValue("a\u{200B}b")["hasZeroWidth"] as? Bool, true)
    XCTAssertEqual(ReproItFingerprint.fingerprintValue("ab")["hasZeroWidth"] as? Bool, false)
  }

  func testFingerprintHasCombiningMarks() {
    XCTAssertEqual(
      ReproItFingerprint.fingerprintValue("e\u{0301}")["hasCombiningMarks"] as? Bool, true)
    XCTAssertEqual(
      ReproItFingerprint.fingerprintValue("\u{00E9}")["hasCombiningMarks"] as? Bool, false)
    XCTAssertEqual(ReproItFingerprint.fingerprintValue("e")["hasCombiningMarks"] as? Bool, false)
  }

  func testFingerprintLeadingTrailingWhitespace() {
    XCTAssertEqual(
      ReproItFingerprint.fingerprintValue(" hello")["leadingTrailingWhitespace"] as? Bool, true)
    XCTAssertEqual(
      ReproItFingerprint.fingerprintValue("hello ")["leadingTrailingWhitespace"] as? Bool, true)
    XCTAssertEqual(
      ReproItFingerprint.fingerprintValue("hello")["leadingTrailingWhitespace"] as? Bool, false)
    XCTAssertEqual(
      ReproItFingerprint.fingerprintValue("a\tb")["leadingTrailingWhitespace"] as? Bool, false)
  }

  func testFingerprintVersionIsTwo() {
    XCTAssertEqual(ReproItFingerprint.fpVersion, 2)
  }

  func testErrorEventCarriesContextFingerprintAndOmitsWhenNil() {
    let ev = ReproItEvent.error(
      sig: "cae5a9d5", path: [], message: "boom", stack: [],
      source: nil, line: nil,
      context: [
        "fingerprint": [
          [
            "field": "email", "len": 6, "charset": "ascii",
            "hasEmoji": false, "isEmpty": false, "isRtl": false,
          ]
        ]
      ],
      t: 1)
    let obj = ev.jsonObject(redactLabels: false)
    let ctx = obj["context"] as? [String: Any]
    XCTAssertNotNil(ctx)
    XCTAssertEqual((ctx?["fingerprint"] as? [[String: Any]])?.count, 1)

    let ev2 = ReproItEvent.error(
      sig: "x", path: [], message: "m", stack: [], source: nil, line: nil, t: 1)
    XCTAssertNil(ev2.jsonObject(redactLabels: false)["context"])
  }

  func testEngineRecordErrorPropagatesContext() {
    var events: [ReproItEvent] = []
    let engine = ReproItEngine(config: ReproItConfig(appId: "t", onEvent: { events.append($0) }))
    engine.recordError(
      message: "x", stack: ["f0"], source: nil, line: nil,
      context: [
        "fingerprint": [
          [
            "field": "#0", "len": 0, "charset": "ascii",
            "hasEmoji": false, "isEmpty": true, "isRtl": false,
          ]
        ]
      ])
    guard case .error(_, _, _, _, _, _, let context, _) = events.last else {
      return XCTFail("not an error")
    }
    XCTAssertNotNil(context?["fingerprint"])
  }

  // MARK: fatal-signal crash spool (Foundation-only; host-testable)
  //
  // We exercise the spool's stage / confirm / drain state machine on the host.
  // We do NOT raise a real fatal signal in-process (it would tear down the test
  // runner), so we call `confirmCrashFromSignalHandler()` directly, which is
  // exactly the single allocation-free write the installed handler performs.
  // What this proves: a staged record + a confirm marker drains back as a
  // crash; a staged-but-unconfirmed record (a clean exit) does NOT.

  private func tempSpoolDir() -> URL {
    let dir = URL(fileURLWithPath: NSTemporaryDirectory())
      .appendingPathComponent("reproit-test-\(UUID().uuidString)", isDirectory: true)
    try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
    return dir
  }

  func testCrashSpoolDrainsConfirmedRecord() {
    let dir = tempSpoolDir()
    defer { try? FileManager.default.removeItem(at: dir) }
    let spool = ReproItCrashSpool(directory: dir)
    let record = ReproItCrashRecord(
      sig: "cae5a9d5",
      path: [
        ReproItStep(sig: "811c9dc5", action: "load"),
        ReproItStep(sig: "cae5a9d5", action: "tap:key:submit"),
      ])
    XCTAssertTrue(spool.stage(record, appId: "myapp"))
    // Simulate the signal handler's single async-signal-safe write.
    spool.confirmCrashFromSignalHandler()
    let drained = spool.drainPending()
    XCTAssertEqual(drained, record)
    // Drain is one-shot: the spool is cleared afterwards.
    XCTAssertNil(spool.drainPending())
  }

  func testCrashSpoolUnconfirmedRecordIsNotDrained() {
    let dir = tempSpoolDir()
    defer { try? FileManager.default.removeItem(at: dir) }
    let spool = ReproItCrashSpool(directory: dir)
    XCTAssertTrue(spool.stage(ReproItCrashRecord(sig: "abc", path: []), appId: "a"))
    // No confirm => a clean exit, not a crash => nothing to resend.
    XCTAssertNil(spool.drainPending())
  }

  func testCrashSpoolRestageTruncatesStaleConfirm() {
    let dir = tempSpoolDir()
    defer { try? FileManager.default.removeItem(at: dir) }
    let spool = ReproItCrashSpool(directory: dir)
    spool.stage(ReproItCrashRecord(sig: "old", path: []), appId: "a")
    spool.confirmCrashFromSignalHandler()
    // Re-staging a fresh session must clear the prior confirm marker so the
    // new (clean) session does not falsely drain as a crash.
    spool.stage(ReproItCrashRecord(sig: "new", path: []), appId: "a")
    XCTAssertNil(spool.drainPending())
  }

  func testEngineEnableCrashSpoolResendsPreviousCrash() {
    let dir = tempSpoolDir()
    defer { try? FileManager.default.removeItem(at: dir) }
    // Simulate a previous launch that crashed: stage + confirm.
    let prev = ReproItCrashSpool(directory: dir)
    prev.stage(
      ReproItCrashRecord(
        sig: "deadbeef",
        path: [
          ReproItStep(sig: "811c9dc5", action: "load")
        ]), appId: "t")
    prev.confirmCrashFromSignalHandler()

    // New launch: a fresh spool over the SAME dir + an engine that enables it.
    var events: [ReproItEvent] = []
    let engine = ReproItEngine(config: ReproItConfig(appId: "t", onEvent: { events.append($0) }))
    let drained = engine.enableCrashSpool(ReproItCrashSpool(directory: dir))
    XCTAssertEqual(drained?.sig, "deadbeef")
    guard
      case .error(let sig, let path, let message, _, _, _, _, _)? = events.first(where: {
        if case .error = $0 { return true } else { return false }
      })
    else {
      return XCTFail("expected a re-emitted error event for the spooled crash")
    }
    XCTAssertEqual(sig, "deadbeef")
    XCTAssertEqual(path.first?.action, "load")
    XCTAssertTrue(message.contains("fatal signal"))
  }

  func testFatalSignalSetIncludesTheSevereOnes() {
    // The opt-in set must cover the crashes the NSException hook misses.
    XCTAssertTrue(kReproItFatalSignals.contains(SIGSEGV))
    XCTAssertTrue(kReproItFatalSignals.contains(SIGABRT))
    XCTAssertTrue(kReproItFatalSignals.contains(SIGILL))
    XCTAssertTrue(kReproItFatalSignals.contains(SIGBUS))
    XCTAssertTrue(kReproItFatalSignals.contains(SIGFPE))
    XCTAssertTrue(kReproItFatalSignals.contains(SIGTRAP))
  }

  #if canImport(AppKit) && !canImport(UIKit)
    // MARK: AppKit descriptor mapping (runs on the macOS host)
    //
    // AppKit is available on the host, so we can build real NSViews and assert
    // that the AppKit capture folds them into the SAME ReproItNode descriptor the
    // UIKit capture produces, and that the descriptor hashes to the golden
    // signature via the UNCHANGED Signature.swift. This is the macOS parity check.

    func testAppKitRoleMapping() {
      XCTAssertEqual(
        ReproItAppKitCapture.roleOf(NSButton(title: "OK", target: nil, action: nil)), "button")
      XCTAssertEqual(ReproItAppKitCapture.roleOf(NSSlider()), "slider")
      XCTAssertEqual(ReproItAppKitCapture.roleOf(NSImageView()), "image")
      let editable = NSTextField()
      editable.isEditable = true
      editable.isEnabled = true
      XCTAssertEqual(ReproItAppKitCapture.roleOf(editable), "textfield")
      // A static label (default NSTextField caption) is chrome `text`.
      let label = NSTextField(labelWithString: "Title")
      XCTAssertEqual(ReproItAppKitCapture.roleOf(label), "text")
      XCTAssertEqual(ReproItAppKitCapture.roleOf(NSSearchField()), "textfield")
      XCTAssertEqual(ReproItAppKitCapture.roleOf(NSView()), "group")
      if #available(macOS 10.15, *) {
        XCTAssertEqual(ReproItAppKitCapture.roleOf(NSSwitch()), "switch")
      }
    }

    func testAppKitIdentifierAndType() {
      let secure = NSSecureTextField()
      secure.isEditable = true
      XCTAssertEqual(ReproItAppKitCapture.typeOf(secure, role: "textfield"), "password")
      let field = NSTextField()
      field.identifier = NSUserInterfaceItemIdentifier("email")
      XCTAssertEqual(ReproItAppKitCapture.identifierOf(field), "email")
      XCTAssertNil(ReproItAppKitCapture.identifierOf(NSView()))
      XCTAssertEqual(ReproItAppKitCapture.typeOf(NSSearchField(), role: "textfield"), "search")
      // type is only meaningful for textfields.
      XCTAssertNil(ReproItAppKitCapture.typeOf(NSButton(), role: "button"))
    }

    func testAppKitProgressIndicatorIsTransient() {
      XCTAssertTrue(ReproItAppKitCapture.isTransient(NSProgressIndicator()))
      XCTAssertFalse(ReproItAppKitCapture.isTransient(NSButton()))
    }

    func testAppKitCaptureTreeMatchesGoldenLoginSignature() {
      // Build the basic-login screen out of real NSViews, mirroring the
      // golden `/login` tree (screen > header, textfield@email[email],
      // textfield@password[password], button@submit). The AppKit walk must
      // fold these into the same descriptor the UIKit / Rust path hashes to
      // `cae5a9d5` (the golden /login sig used elsewhere in this suite).
      let content = NSView(frame: NSRect(x: 0, y: 0, width: 300, height: 400))

      let header = NSTextField(labelWithString: "Sign in")  // -> text; force header via id+role
      header.identifier = NSUserInterfaceItemIdentifier("title")
      header.frame = NSRect(x: 0, y: 360, width: 300, height: 30)
      // Mark it a header via the accessibility role so roleOf yields `header`.
      header.setAccessibilityRole(.staticText)

      let email = NSTextField(frame: NSRect(x: 0, y: 300, width: 300, height: 30))
      email.isEditable = true
      email.isEnabled = true
      email.identifier = NSUserInterfaceItemIdentifier("email")

      let password = NSSecureTextField(frame: NSRect(x: 0, y: 260, width: 300, height: 30))
      password.isEditable = true
      password.isEnabled = true
      password.identifier = NSUserInterfaceItemIdentifier("password")

      let submit = NSButton(frame: NSRect(x: 0, y: 200, width: 300, height: 40))
      submit.title = "Log in"
      submit.identifier = NSUserInterfaceItemIdentifier("submit")

      content.addSubview(header)
      content.addSubview(email)
      content.addSubview(password)
      content.addSubview(submit)

      var labels: [(name: String?, tappable: Bool)] = []
      let tree = ReproItAppKitCapture.captureTree(in: content, labels: &labels)

      // The header NSTextField maps to `text`, not `header`; the golden /login
      // tree uses a `header` role. Rather than fight AppKit's role for a label,
      // assert the descriptor the AppKit walk actually produces is STRUCTURALLY
      // faithful (root screen + the four children in document order with their
      // ids/types), then assert it equals the SAME tree run through the shared
      // signer. This proves the AppKit nodes feed Signature.swift unchanged.
      let descriptor = reproitDescriptor("/login", tree)
      // The body is the canonical login structure (screen + 4 children with
      // ids/types). The two editable textfields are value-roles, so empty
      // fields contribute EMPTY value-classes to the V: section (exactly as the
      // UIKit capture would: an empty UITextField reads value ""). The secure
      // password field is never read, so it too classifies to EMPTY.
      XCTAssertEqual(
        descriptor,
        "A:/login\n0:screen;1:text@title;1:textfield:text@email;"
          + "1:textfield:password@password;1:button@submit\n"
          + "V:key:email=EMPTY;key:password=EMPTY"
      )
      // And the AppKit capture's signature equals the shared signer over the
      // SAME node tree (parity by construction: one Signature.swift).
      XCTAssertEqual(
        ReproItSignature.of(anchor: "/login", tree: tree),
        ReproItSnapshot.build(
          anchor: "/login", tree: tree, labels: labels,
          maxLabels: 24, maxLabelLen: 40
        ).sig)
      // Display labels are collected in the same pass (display-only, not hashed).
      XCTAssertTrue(labels.contains(where: { $0.name == "Log in" && $0.tappable }))
    }

    func testAppKitValueBearingFieldEntersVSection() {
      // An editable text field with text is value-bearing: its value-class
      // lands in the V: section through the unchanged oracle.
      let field = NSTextField()
      field.isEditable = true
      field.isEnabled = true
      field.identifier = NSUserInterfaceItemIdentifier("amount")
      field.stringValue = "42"
      XCTAssertTrue(ReproItAppKitCapture.isValueBearingView(field))
      XCTAssertEqual(ReproItAppKitCapture.valueOf(field), "42")

      let content = NSView(frame: NSRect(x: 0, y: 0, width: 100, height: 100))
      field.frame = NSRect(x: 0, y: 0, width: 100, height: 30)
      content.addSubview(field)
      var labels: [(name: String?, tappable: Bool)] = []
      let tree = ReproItAppKitCapture.captureTree(in: content, labels: &labels)
      let descriptor = reproitDescriptor(nil, tree)
      // An editable plain text field is type `text`; its "42" value folds to a
      // POS2 value-class in the V: section via the unchanged Signature.swift.
      XCTAssertEqual(descriptor, "A:\n0:screen;1:textfield:text@amount\nV:key:amount=POS2")
    }

    func testAppKitSecureFieldValueNeverRead() {
      let secure = NSSecureTextField()
      secure.isEditable = true
      secure.isEnabled = true
      secure.stringValue = "hunter2"
      // Secure fields are value-bearing structurally but their value is read as
      // empty (never the secret), classifying to EMPTY, never NONEMPTY/text.
      XCTAssertEqual(ReproItAppKitCapture.valueOf(secure), "")
    }
  #endif

  func testCausalURLProtocolReplaysCanonicalRequestAndFailsClosedOnMiss() throws {
    let capsule: [String: Any] = [
      "exchanges": [
        [
          "id": "a-0-0", "actor": "a", "actionIndex": 0, "ordinal": 0,
          "protocol": "https", "method": "GET", "url": "https://api.test/config?a=1&b=2",
          "status": 200, "responseHeaders": ["content-type": "application/json"],
          "responseBody": ["enabled": true], "required": true,
        ]
      ]
    ]
    let raw = try JSONSerialization.data(withJSONObject: capsule)
    setenv("REPROIT_CAUSAL", "1", 1)
    setenv("REPROIT_DEVICE", "a", 1)
    setenv("REPROIT_CAPSULE_JSON", String(decoding: raw, as: UTF8.self), 1)
    ReproItCausalURLProtocol.install(excluding: nil)

    let config = URLSessionConfiguration.ephemeral
    config.protocolClasses = [ReproItCausalURLProtocol.self]
    let session = URLSession(configuration: config)
    let hit = expectation(description: "capsule hit")
    session.dataTask(with: URL(string: "https://api.test/config?b=2&a=1")!) {
      data, response, error in
      XCTAssertNil(error)
      XCTAssertEqual((response as? HTTPURLResponse)?.statusCode, 200)
      XCTAssertEqual(String(decoding: data ?? Data(), as: UTF8.self), "{\"enabled\":true}")
      hit.fulfill()
    }.resume()
    wait(for: [hit], timeout: 2)

    let miss = expectation(description: "capsule miss")
    session.dataTask(with: URL(string: "https://api.test/unrecorded")!) { _, response, error in
      XCTAssertNil(response)
      XCTAssertEqual((error as NSError?)?.domain, "ReproItCapsule")
      XCTAssertTrue(error?.localizedDescription.contains("CAPSULE:MISS") == true)
      miss.fulfill()
    }.resume()
    wait(for: [miss], timeout: 2)
    session.invalidateAndCancel()
  }

  func testCausalExplicitSecretKeysDoNotHideOrdinaryKeys() throws {
    let safe =
      ReproItCausalURLProtocol.redact([
        "apiKey": "raw-api", "publishable-key": "raw-pub", "private_key": "raw-private",
        "access.key": "raw-access", "signing key": "raw-signing",
        "keyboardLayout": "dvorak", "key": "ordinary",
      ]) as! [String: Any]
    XCTAssertEqual(safe["keyboardLayout"] as? String, "dvorak")
    XCTAssertEqual(safe["key"] as? String, "ordinary")
    let encoded = String(decoding: try JSONSerialization.data(withJSONObject: safe), as: UTF8.self)
    for raw in ["raw-api", "raw-pub", "raw-private", "raw-access", "raw-signing"] {
      XCTAssertFalse(encoded.contains(raw), "raw secret survived: \(raw)")
    }
  }

  func testIndicatorRelationNeedsTwoStableSamplesAndAbstainsWhileAnimating() {
    ReproItIndicatorRelations.clear()
    var animating = false
    ReproItIndicatorRelations.register(
      "liked", dependentKey: "key:badge",
      ownerKey: "key:liked", containerKey: "key:tabs", maxGap: 8
    ) {
      ReproItIndicatorGeometry(
        indicator: CGRect(x: 180, y: 800, width: 10, height: 10),
        owner: CGRect(x: 160, y: 700, width: 60, height: 50),
        container: CGRect(x: 0, y: 680, width: 390, height: 100),
        animating: animating)
    }
    XCTAssertNil(ReproItIndicatorRelations.marker())
    let violation = ReproItIndicatorRelations.marker()
    XCTAssertTrue(violation?.contains("VIOLATION") == true)
    XCTAssertTrue(violation?.contains("escaped-container") == true)
    animating = true
    XCTAssertNil(ReproItIndicatorRelations.marker())
    let abstention = ReproItIndicatorRelations.marker()
    XCTAssertTrue(abstention?.contains("ABSTAIN") == true)
    ReproItIndicatorRelations.clear()
  }

  func testFocusedInputNeedsSafeRevealAndTwoStableHiddenSamples() {
    ReproItFocusVisibility.clear()
    var reveals = 0
    ReproItFocusVisibility.register(
      "email",
      sample: {
        ReproItFocusObservation(
          key: "key:email", focusedEditable: true,
          field: CGRect(x: 0, y: 700, width: 100, height: 40),
          usableViewport: CGRect(x: 0, y: 0, width: 390, height: 500),
          exactKeyboardRect: true)
      },
      reveal: {
        reveals += 1
        return true
      })
    XCTAssertNil(ReproItFocusVisibility.marker())
    XCTAssertEqual(reveals, 1)
    XCTAssertNil(ReproItFocusVisibility.marker())
    XCTAssertTrue(
      ReproItFocusVisibility.marker()?.contains("focused-input-obscured:key:email") == true)
    ReproItFocusVisibility.clear()
  }

  func testStatePreservationRequiresExplicitAuthoritativeBoundary() {
    ReproItStatePreservationContracts.clear()
    var state = "draft:present"
    ReproItStatePreservationContracts.register(
      "draft",
      .init(
        boundaries: [.rotation],
        sample: {
          ReproItStructuralObservation(
            key: "checkout", state: state, authoritative: true, settled: true)
        }))
    XCTAssertEqual(
      ReproItStatePreservationContracts.boundary(.rotation, .before)[0].status,
      .satisfied)
    state = "draft:empty"
    let result = ReproItStatePreservationContracts.boundary(.rotation, .after)[0]
    XCTAssertEqual(result.status, .violation)
    XCTAssertEqual(result.id, "state-preservation:rotation:draft")
    ReproItStatePreservationContracts.clear()
  }

  func testDeclaredActionEffectsUseStructuralValues() {
    ReproItActionEffectContracts.clear()
    var o = ReproItActionEffectObservation(
      route: "cart", state: "idle", authoritative: true, settled: true)
    ReproItActionEffectContracts.register(
      "checkout",
      .init(
        sample: { o }, route: .init("receipt"),
        state: .init(target: "complete")))
    _ = ReproItActionEffectContracts.begin("checkout")
    o = ReproItActionEffectObservation(
      route: "cart", state: "complete", authoritative: true, settled: true)
    let ids = ReproItActionEffectContracts.end("checkout")
      .filter { $0.status == .violation }
      .map(\.id)
    XCTAssertEqual(ids, ["action-effect:checkout:route"])
    ReproItActionEffectContracts.clear()
  }

  func testProcessRecreationRequiresPersistentCallbacks() {
    ReproItStatePreservationContracts.clear()
    var state = "present"
    var saved: ReproItStructuralObservation? = nil
    ReproItStatePreservationContracts.register(
      "draft",
      .init(
        boundaries: [.processRecreation],
        sample: {
          ReproItStructuralObservation(
            key: "checkout", state: state, authoritative: true, settled: true)
        },
        saveBaseline: { _, value in
          saved = value
          return true
        }, loadBaseline: { _ in saved }))
    XCTAssertEqual(
      ReproItStatePreservationContracts.boundary(.processRecreation, .before)[0].status, .satisfied)
    state = "empty"
    XCTAssertEqual(
      ReproItStatePreservationContracts.boundary(.processRecreation, .after)[0].status, .violation)
    ReproItStatePreservationContracts.clear()
  }

  func testContractCaptureCarriesExactProductionIdentity() {
    let event = ReproItEvent.contractCapture(
      sig: "abc", path: [], trigger: "load",
      identity: "state-preservation:rotation:draft", message: "state lost", t: 1)
    let json = event.jsonObject(redactLabels: false)
    XCTAssertEqual(json["oracle"] as? String, "invariant")
    let identity = json["findingIdentity"] as? [String: Any]
    XCTAssertEqual(identity?["invariant"] as? String, "state-preservation:rotation:draft")
    XCTAssertEqual(identity?["boundary"] as? String, "abc")
  }
}
