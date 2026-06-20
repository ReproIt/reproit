import XCTest
import Foundation
@testable import ReproIt

/// These tests run on the macOS HOST under `swift test`. They cover the
/// canonical contract (STRUCTURAL signature parity against the golden vectors,
/// name normalization, snapshot rules, event/batch encoding, engine edge logic)
/// using only the Foundation-only surface, so no UIKit is required to validate
/// parity with the runners and the other SDKs.
final class ReproItTests: XCTestCase {

    // MARK: signature parity, the load-bearing assertions
    //
    // THE parity gate: load signature_vectors.json (repo root) and assert that
    // `ReproItSignature.of(anchor:tree:)` reproduces every vector's expected_sig
    // bit-for-bit, exactly like the Rust oracle's `golden_vectors_match`.

    struct Vector {
        let description: String
        let anchor: String?
        let tree: ReproItNode
        let expectedSig: String
    }

    /// Locate signature_vectors.json relative to THIS source file: the test file
    /// lives at <repo>/sdk/reproit-ios/Tests/ReproItTests/, so the repo root is
    /// four directories up. Falls back to the current working directory.
    static func vectorsURL() -> URL {
        let here = URL(fileURLWithPath: #filePath)
        let root = here
            .deletingLastPathComponent() // ReproItTests
            .deletingLastPathComponent() // Tests
            .deletingLastPathComponent() // reproit-ios
            .deletingLastPathComponent() // sdk
            .deletingLastPathComponent() // repo root
        let candidate = root.appendingPathComponent("signature_vectors.json")
        if FileManager.default.fileExists(atPath: candidate.path) { return candidate }
        return URL(fileURLWithPath: FileManager.default.currentDirectoryPath)
            .appendingPathComponent("signature_vectors.json")
    }

    func loadVectors() throws -> [Vector] {
        let url = ReproItTests.vectorsURL()
        let data = try Data(contentsOf: url)
        let raw = try JSONSerialization.jsonObject(with: data) as? [[String: Any]]
        let arr = try XCTUnwrap(raw, "signature_vectors.json must be a JSON array")
        return arr.map { obj in
            Vector(
                description: (obj["description"] as? String) ?? "",
                anchor: obj["anchor"] as? String,
                tree: ReproItNode.fromJSON(try! XCTUnwrap(obj["tree"] as? [String: Any])),
                expectedSig: (obj["expected_sig"] as? String) ?? ""
            )
        }
    }

    func testGoldenVectorsMatch() throws {
        let vectors = try loadVectors()
        // The current contract ships 24 golden vectors (structural + value-state);
        // assert ALL of them are present and each reproduces bit-for-bit.
        XCTAssertEqual(vectors.count, 24, "expected 24 vectors, got \(vectors.count)")
        for v in vectors {
            let got = ReproItSignature.of(anchor: v.anchor, tree: v.tree)
            XCTAssertEqual(
                got, v.expectedSig,
                """
                vector '\(v.description)' mismatch.
                  descriptor = \(reproitDescriptor(v.anchor, v.tree).debugDescription)
                  expected \(v.expectedSig) got \(got)
                """)
        }
    }

    /// Assert the cross-vector relationships the spec promises, mirroring the
    /// Rust oracle's `vector_relationships_hold`.
    func testVectorRelationshipsHold() throws {
        let vectors = try loadVectors()
        func by(_ needle: String) -> String {
            guard let v = vectors.first(where: { $0.description.contains(needle) }) else {
                XCTFail("no vector matching \(needle)"); return ""
            }
            return v.expectedSig
        }
        let login = by("basic login")
        XCTAssertEqual(login, by("locale-invariance"))
        XCTAssertEqual(login, by("transient-drop (spinner)"))
        XCTAssertEqual(login, by("transient-drop (snackbar"))
        XCTAssertEqual(by("repeated-collapse (3 items)"), by("repeated-collapse (5 items"))
        XCTAssertNotEqual(login, by("collision-fix via input type"))
        XCTAssertNotEqual(login, by("collision-fix via icon"))
        XCTAssertNotEqual(by("collision-fix via input type"), by("collision-fix via icon"))
        let settings = by("same route + same structure")
        XCTAssertNotEqual(settings, by("different route + same structure"))
        XCTAssertNotEqual(settings, by("same route + different structure"))
        XCTAssertEqual(by("parameterized route (item 42)"), by("parameterized route (item 99)"))

        // value-state (Layer 2): EMPTY / ZERO / POS1 are three distinct states.
        let vEmpty = by("empty value-class")
        let vZero = by("zero value-class")
        let vPos1 = by("POS1 value-class")
        XCTAssertNotEqual(vEmpty, vZero)
        XCTAssertNotEqual(vEmpty, vPos1)
        XCTAssertNotEqual(vZero, vPos1)
        // numeric counter 0 vs 5 -> ZERO vs POS1 distinct.
        XCTAssertNotEqual(by("counter at 0"), by("counter at 5"))
        // a chrome label with a value is backward-compatible: identical to the
        // same structure with no value field at all (no V: section emitted).
        let s = ReproItNode(role: "screen", children: [
            ReproItNode(role: "header", id: "title"),
        ])
        XCTAssertEqual(ReproItSignature.of(anchor: "/home", tree: s), by("chrome label with text"))
        // grouped/locale number is locale-safe (NONEMPTY), distinct from numerics.
        let vGrouped = by("grouped/locale number")
        XCTAssertNotEqual(vGrouped, vPos1)
        XCTAssertNotEqual(vGrouped, vZero)
        // two different POS1 values (3 vs 7) bucket the same.
        XCTAssertEqual(by("two different POS1 values bucket the same (3)"),
                       by("two different POS1 values bucket the same (7)"))
    }

    // MARK: Layer 2 value-state unit checks (mirror the oracle's unit tests)

    func testValueClassAllBuckets() {
        XCTAssertEqual(reproitValueClass(""), "EMPTY")
        XCTAssertEqual(reproitValueClass("   "), "EMPTY")
        XCTAssertEqual(reproitValueClass("0"), "ZERO")
        XCTAssertEqual(reproitValueClass("0.0"), "ZERO")
        XCTAssertEqual(reproitValueClass("-0"), "ZERO")
        XCTAssertEqual(reproitValueClass("-3"), "NEG")
        XCTAssertEqual(reproitValueClass("-0.5"), "NEG")
        XCTAssertEqual(reproitValueClass("3"), "POS1")
        XCTAssertEqual(reproitValueClass("9.99"), "POS1")
        XCTAssertEqual(reproitValueClass("+7"), "POS1")
        XCTAssertEqual(reproitValueClass("10"), "POS2")
        XCTAssertEqual(reproitValueClass("99"), "POS2")
        XCTAssertEqual(reproitValueClass("100"), "POS3")
        XCTAssertEqual(reproitValueClass("999.99"), "POS3")
        XCTAssertEqual(reproitValueClass("1000"), "POSL")
        XCTAssertEqual(reproitValueClass("123456"), "POSL")
        XCTAssertEqual(reproitValueClass("  42  "), "POS2")
    }

    func testValueClassLocaleSafeFallback() {
        XCTAssertEqual(reproitValueClass("1,234"), "NONEMPTY")
        XCTAssertEqual(reproitValueClass("1.234.567"), "NONEMPTY")
        XCTAssertEqual(reproitValueClass("1 234"), "NONEMPTY")
        XCTAssertEqual(reproitValueClass("$5"), "NONEMPTY")
        XCTAssertEqual(reproitValueClass("5%"), "NONEMPTY")
        XCTAssertEqual(reproitValueClass("1e3"), "NONEMPTY")
        XCTAssertEqual(reproitValueClass("0x10"), "NONEMPTY")
        XCTAssertEqual(reproitValueClass("."), "NONEMPTY")
        XCTAssertEqual(reproitValueClass("3."), "NONEMPTY")
        XCTAssertEqual(reproitValueClass(".5"), "NONEMPTY")
        XCTAssertEqual(reproitValueClass("--5"), "NONEMPTY")
        XCTAssertEqual(reproitValueClass("hello"), "NONEMPTY")
        XCTAssertEqual(reproitValueClass("١٢٣"), "NONEMPTY")
    }

    func testZeroValueTreeByteIdenticalToStructural() {
        // A textfield WITHOUT a value: byte-identical to the pre-value-state form.
        let tf = ReproItNode(role: "textfield", id: "email")
        XCTAssertEqual(reproitDescriptor(nil, tf), "A:\n0:textfield@email")
        // A chrome node WITH a value is still not value-bearing: no V: section.
        let header = ReproItNode(role: "header", id: "title", value: "Welcome")
        XCTAssertEqual(reproitDescriptor(nil, header), "A:\n0:header@title")
    }

    func testValueBearingAddsVSection() {
        let tf = ReproItNode(role: "textfield", id: "email", value: "a@b.com")
        XCTAssertEqual(reproitDescriptor(nil, tf), "A:\n0:textfield@email\nV:key:email=NONEMPTY")
        // status is a value-role but not in ROLES, so the body token is `node`.
        let counter = ReproItNode(role: "status", id: "count", value: "5")
        XCTAssertEqual(reproitDescriptor(nil, counter), "A:\n0:node@count\nV:key:count=POS1")
    }

    func testVSectionSortedByKey() {
        let screen = ReproItNode(role: "screen", children: [
            ReproItNode(role: "textfield", id: "zeta", value: "0"),
            ReproItNode(role: "textfield", id: "alpha", value: "12"),
        ])
        XCTAssertEqual(
            reproitDescriptor(nil, screen),
            "A:\n0:screen;1:textfield@zeta;1:textfield@alpha\nV:key:alpha=POS2;key:zeta=ZERO")
    }

    func testKeylessValueNodeUsesStructuralIndex() {
        let screen = ReproItNode(role: "screen", children: [
            ReproItNode(role: "textfield", value: "3"),
            ReproItNode(role: "textfield", value: "99"),
        ])
        // The two keyless textfields collapse to one *-marked body token (value
        // is not structural); the V: section still distinguishes them by index.
        XCTAssertEqual(
            reproitDescriptor(nil, screen),
            "A:\n0:screen;1:textfield*\nV:role:textfield#0=POS1;role:textfield#1=POS2")
    }

    func testOptInValueNodeFlag() {
        // A `text` role is chrome, so even with a value it is not value-bearing...
        var t = ReproItNode(role: "text", id: "display", value: "42")
        XCTAssertEqual(reproitDescriptor(nil, t), "A:\n0:text@display")
        // ...unless explicitly flagged via valueNode (Layer 3 opt-in).
        t.valueNode = true
        XCTAssertEqual(reproitDescriptor(nil, t), "A:\n0:text@display\nV:key:display=POS2")
    }

    func testTwoPos1ValuesSameSignature() {
        func mk(_ v: String) -> ReproItNode { ReproItNode(role: "status", id: "count", value: v) }
        XCTAssertEqual(ReproItSignature.of(anchor: nil, tree: mk("3")),
                       ReproItSignature.of(anchor: nil, tree: mk("7")))
        XCTAssertNotEqual(ReproItSignature.of(anchor: nil, tree: mk("0")),
                          ReproItSignature.of(anchor: nil, tree: mk("3")))
    }

    func testTransientValueNodeExcludedFromVSection() {
        let screen = ReproItNode(role: "screen", children: [
            ReproItNode(role: "group", transient: true, children: [
                ReproItNode(role: "status", id: "loading", value: "50"),
            ]),
        ])
        XCTAssertEqual(reproitDescriptor(nil, screen), "A:\n0:screen")
    }

    func testRunnerCapExcludesKey() {
        // The runner cap drops a capped value-key from the V: section, falling
        // back to structural-only for that node.
        let tf = ReproItNode(role: "textfield", id: "amount", value: "5")
        let full = ReproItSignature.of(anchor: nil, tree: tf)
        let capped = ReproItSignature.from(anchor: nil, tree: tf, excludeKeys: ["key:amount"])
        let structural = ReproItSignature.of(anchor: nil, tree: ReproItNode(role: "textfield", id: "amount"))
        XCTAssertNotEqual(full, capped)
        XCTAssertEqual(capped, structural)
    }

    // MARK: descriptor / hash unit checks (mirror the oracle's unit tests)

    func testFnv1aKnownValues() {
        XCTAssertEqual(ReproItSignature.fnv1a32Hex(Array("".utf8)), "811c9dc5")
        XCTAssertEqual(ReproItSignature.fnv1a32Hex(Array("a".utf8)), "e40c292c")
    }

    func testUnknownRoleMapsToNode() {
        XCTAssertEqual(reproitDescriptor(nil, ReproItNode(role: "carousel")), "A:\n0:node")
    }

    func testEmptyAnchorStillHasPrefixLine() {
        let n = ReproItNode(role: "screen")
        XCTAssertEqual(reproitDescriptor(nil, n), "A:\n0:screen")
        XCTAssertEqual(reproitDescriptor("", n), "A:\n0:screen")
    }

    func testTransientSubtreeDropped() {
        let with = ReproItNode(role: "screen", children: [
            ReproItNode(role: "text"),
            ReproItNode(role: "spinner", children: [ReproItNode(role: "text")]),
        ])
        let without = ReproItNode(role: "screen", children: [ReproItNode(role: "text")])
        XCTAssertEqual(reproitDescriptor(nil, with), reproitDescriptor(nil, without))
    }

    func testTransientFlagDropped() {
        let with = ReproItNode(role: "screen", children: [
            ReproItNode(role: "group", transient: true),
        ])
        let without = ReproItNode(role: "screen")
        XCTAssertEqual(reproitDescriptor(nil, with), reproitDescriptor(nil, without))
    }

    func testRepeatedSiblingsCollapseRegardlessOfCount() {
        func mk(_ n: Int) -> ReproItNode {
            var kids: [ReproItNode] = []
            for _ in 0..<n {
                kids.append(ReproItNode(role: "listitem", children: [ReproItNode(role: "text")]))
            }
            return ReproItNode(role: "list", children: kids)
        }
        XCTAssertEqual(reproitDescriptor(nil, mk(3)), reproitDescriptor(nil, mk(5)))
        XCTAssertEqual(reproitDescriptor(nil, mk(3)), "A:\n0:list;1:listitem*;2:text")
    }

    func testNonConsecutiveIdenticalNotCollapsed() {
        let g = ReproItNode(role: "group", children: [
            ReproItNode(role: "button"),
            ReproItNode(role: "link"),
            ReproItNode(role: "button"),
        ])
        XCTAssertEqual(reproitDescriptor(nil, g), "A:\n0:group;1:button;1:link;1:button")
    }

    func testTokenFieldOrder() {
        let n = ReproItNode(role: "textfield", id: "pwd", type: "password", icon: "lock")
        XCTAssertEqual(reproitDescriptor(nil, n), "A:\n0:textfield:password#lock@pwd")
    }

    func testSelectorPrefersId() {
        let s = reproitSelector(id: "submit", role: "button", structuralIndex: 3)
        XCTAssertEqual(s.selector, "key:submit")
        XCTAssertFalse(s.nokey)
        let s2 = reproitSelector(id: nil, role: "button", structuralIndex: 2)
        XCTAssertEqual(s2.selector, "role:button#2")
        XCTAssertTrue(s2.nokey)
    }

    // MARK: name normalization (display labels only; never hashed)

    func testNormalizeTrimsAndTakesFirstLine() {
        XCTAssertEqual(ReproItName.normalize("  Hello \n World ", maxLabelLen: 40), "Hello")
    }

    func testNormalizeRejectsEmptyAndOverlong() {
        XCTAssertNil(ReproItName.normalize("   ", maxLabelLen: 40))
        XCTAssertNil(ReproItName.normalize(nil, maxLabelLen: 40))
        XCTAssertNil(ReproItName.normalize(String(repeating: "x", count: 41), maxLabelLen: 40))
        XCTAssertEqual(ReproItName.normalize(String(repeating: "x", count: 40), maxLabelLen: 40),
                       String(repeating: "x", count: 40))
    }

    // MARK: snapshot build rules (structural sig + display labels)

    /// The basic-login tree, reused below; its expected structural sig is the
    /// golden `cae5a9d5` from signature_vectors.json under anchor /login.
    static func loginTree() -> ReproItNode {
        ReproItNode(role: "screen", children: [
            ReproItNode(role: "header", id: "title"),
            ReproItNode(role: "textfield", id: "email", type: "email"),
            ReproItNode(role: "textfield", id: "password", type: "password"),
            ReproItNode(role: "button", id: "submit"),
        ])
    }

    func testSnapshotUsesStructuralSignature() {
        let snap = ReproItSnapshot.build(
            anchor: "/login",
            tree: ReproItTests.loginTree(),
            labels: [("Email", false), ("Password", false), ("Log in", true)],
            maxLabels: 24, maxLabelLen: 40)
        XCTAssertEqual(snap.sig, "cae5a9d5")
    }

    func testSnapshotSignatureIgnoresLabels() {
        // Same structure + anchor but totally different (e.g. localized) labels
        // MUST hash identically: text is excluded from the signature.
        let a = ReproItSnapshot.build(
            anchor: "/login", tree: ReproItTests.loginTree(),
            labels: [("Email", false)], maxLabels: 24, maxLabelLen: 40)
        let b = ReproItSnapshot.build(
            anchor: "/login", tree: ReproItTests.loginTree(),
            labels: [("メール", false), ("パスワード", false)], maxLabels: 24, maxLabelLen: 40)
        XCTAssertEqual(a.sig, b.sig)
        XCTAssertEqual(a.sig, "cae5a9d5")
    }

    func testSnapshotDedupesLabelsAndCountsUnlabeled() {
        let snap = ReproItSnapshot.build(
            anchor: nil,
            tree: ReproItNode(role: "screen"),
            labels: [
                ("Settings", true),
                ("Settings", true),   // duplicate => one label
                ("Back", true),
                (nil, true),          // tappable + unnamed => unlabeled++
                ("   ", true),        // empty after trim => unlabeled++
                ("Just a label", false),
            ],
            maxLabels: 24, maxLabelLen: 40)
        XCTAssertEqual(Set(snap.labels), ["Settings", "Back", "Just a label"])
        XCTAssertEqual(snap.unlabeled, 2)
    }

    func testSnapshotCapsLabels() {
        let many = (0..<50).map { ("label\($0)", true) }
        let snap = ReproItSnapshot.build(
            anchor: nil, tree: ReproItNode(role: "screen"),
            labels: many, maxLabels: 24, maxLabelLen: 40)
        XCTAssertEqual(snap.labels.count, 24)
    }

    // MARK: event + batch encoding shapes

    func testEdgeEventShape() throws {
        let ev = ReproItEvent.edge(
            from: "aaaa1111", action: "tap:key:submit", to: "cae5a9d5",
            labels: ["Email", "Password"], t: 1_717_939_200_123)
        let obj = ev.jsonObject(redactLabels: false)
        XCTAssertEqual(obj["kind"] as? String, "edge")
        XCTAssertEqual(obj["from"] as? String, "aaaa1111")
        XCTAssertEqual(obj["action"] as? String, "tap:key:submit")
        XCTAssertEqual(obj["to"] as? String, "cae5a9d5")
        XCTAssertEqual(obj["labels"] as? [String], ["Email", "Password"])
        XCTAssertEqual(obj["t"] as? Int64, 1_717_939_200_123)
    }

    func testEdgeOmitsFromWhenNilAndLabelsWhenRedacted() {
        let ev = ReproItEvent.edge(
            from: nil, action: "load", to: "811c9dc5", labels: ["x"], t: 1)
        let obj = ev.jsonObject(redactLabels: true)
        XCTAssertNil(obj["from"])
        XCTAssertNil(obj["labels"])
    }

    func testErrorEventShape() {
        let ev = ReproItEvent.error(
            sig: "cae5a9d5",
            path: [ReproItStep(sig: "s1", action: "tap:X"),
                   ReproItStep(sig: "s2", action: "back")],
            message: "boom", stack: ["frame0", "frame1"],
            source: "File.swift", line: 42, t: 1)
        let obj = ev.jsonObject(redactLabels: false)
        XCTAssertEqual(obj["kind"] as? String, "error")
        XCTAssertEqual(obj["sig"] as? String, "cae5a9d5")
        XCTAssertEqual(obj["message"] as? String, "boom")
        XCTAssertEqual(obj["source"] as? String, "File.swift")
        XCTAssertEqual(obj["line"] as? Int, 42)
        let path = obj["path"] as? [[String: String]]
        XCTAssertEqual(path?.count, 2)
        XCTAssertEqual(path?.first?["action"], "tap:X")
    }

    func testBatchEncodesValidJSON() throws {
        let data = ReproItBatch.encode(
            appId: "myapp", sentAt: 12345,
            events: [.edge(from: nil, action: "load", to: "811c9dc5", labels: [], t: 1)],
            redactLabels: false)
        let obj = try XCTUnwrap(
            try JSONSerialization.jsonObject(with: try XCTUnwrap(data)) as? [String: Any])
        XCTAssertEqual(obj["appId"] as? String, "myapp")
        XCTAssertEqual(obj["sentAt"] as? Int64, 12345)
        XCTAssertEqual((obj["events"] as? [[String: Any]])?.count, 1)
    }

    // MARK: engine edge logic (no network: endpoint = nil, onEvent sink)

    func testEngineEmitsLoadThenEdge() {
        var events: [ReproItEvent] = []
        let cfg = ReproItConfig(appId: "t", onEvent: { events.append($0) })
        let engine = ReproItEngine(config: cfg)

        let settings = ReproItSnapshot.build(
            anchor: "/settings",
            tree: ReproItNode(role: "screen", children: [
                ReproItNode(role: "header", id: "title"),
                ReproItNode(role: "switch", id: "notifications"),
            ]),
            labels: [("Settings", false)], maxLabels: 24, maxLabelLen: 40)
        // initial snapshot => load edge
        engine.observe(settings)
        // same structure again => no edge
        engine.observe(ReproItSnapshot.build(
            anchor: "/settings",
            tree: ReproItNode(role: "screen", children: [
                ReproItNode(role: "header", id: "title"),
                ReproItNode(role: "switch", id: "notifications"),
            ]),
            labels: [("Settings", false)], maxLabels: 24, maxLabelLen: 40))
        // a tap then a new screen => tap edge with the pending action
        engine.setPendingAction("tap:key:submit")
        engine.observe(ReproItSnapshot.build(
            anchor: "/login", tree: ReproItTests.loginTree(),
            labels: [("Log in", true)], maxLabels: 24, maxLabelLen: 40))

        XCTAssertEqual(events.count, 2)
        guard case let .edge(from0, action0, to0, _, _) = events[0] else {
            return XCTFail("first event is not an edge")
        }
        XCTAssertNil(from0)
        XCTAssertEqual(action0, "load")
        XCTAssertEqual(to0, "f62301bb") // golden /settings switch-row sig

        guard case let .edge(from1, action1, to1, _, _) = events[1] else {
            return XCTFail("second event is not an edge")
        }
        XCTAssertEqual(from1, "f62301bb")
        XCTAssertEqual(action1, "tap:key:submit")
        XCTAssertEqual(to1, "cae5a9d5") // golden /login sig
    }

    func testEngineErrorCarriesPath() {
        var events: [ReproItEvent] = []
        let cfg = ReproItConfig(appId: "t", onEvent: { events.append($0) })
        let engine = ReproItEngine(config: cfg)
        engine.observe(ReproItSnapshot.build(
            anchor: "/login", tree: ReproItTests.loginTree(),
            labels: [("Log in", true)], maxLabels: 24, maxLabelLen: 40))
        engine.recordError(message: "kaboom", stack: ["f0"], source: nil, line: nil)

        guard case let .error(sig, path, message, _, _, _, _, _) = events.last else {
            return XCTFail("last event is not an error")
        }
        XCTAssertEqual(sig, "cae5a9d5")
        XCTAssertEqual(message, "kaboom")
        XCTAssertEqual(path.first?.action, "load")
    }

    func testStackCappedAtEight() {
        var events: [ReproItEvent] = []
        let cfg = ReproItConfig(appId: "t", onEvent: { events.append($0) })
        let engine = ReproItEngine(config: cfg)
        let frames = (0..<20).map { "f\($0)" }
        engine.recordError(message: "x", stack: frames, source: nil, line: nil)
        guard case let .error(_, _, _, stack, _, _, _, _) = events.last else {
            return XCTFail("not an error")
        }
        XCTAssertEqual(stack.count, 8)
    }

    // MARK: context API (mirrors reproit_flutter)

    func testAutoDimensionsPresent() {
        let dims = ReproItContext.autoDimensions()
        XCTAssertEqual(dims["platform"] as? String, "ios")
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
        XCTAssertEqual(ctx["platform"] as? String, "ios")
        XCTAssertNotNil(ctx["locale"] as? String)
        XCTAssertNotNil(ctx["tz"] as? String)
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
        XCTAssertNotEqual(ReproItContext.hashUserId(raw),
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

    func testBatchIncludesCtxWhenNonEmptyAndOmitsWhenEmpty() throws {
        let withCtx = ReproItBatch.encode(
            appId: "a", sentAt: 1,
            ctx: ["platform": "ios", "uid": "deadbeefdeadbeef"],
            events: [.edge(from: nil, action: "load", to: "811c9dc5", labels: [], t: 1)],
            redactLabels: false)
        let obj1 = try XCTUnwrap(
            try JSONSerialization.jsonObject(with: try XCTUnwrap(withCtx)) as? [String: Any])
        let ctx = obj1["ctx"] as? [String: Any]
        XCTAssertEqual(ctx?["platform"] as? String, "ios")
        XCTAssertEqual(ctx?["uid"] as? String, "deadbeefdeadbeef")

        let noCtx = ReproItBatch.encode(
            appId: "a", sentAt: 1,
            events: [.edge(from: nil, action: "load", to: "811c9dc5", labels: [], t: 1)],
            redactLabels: false)
        let obj2 = try XCTUnwrap(
            try JSONSerialization.jsonObject(with: try XCTUnwrap(noCtx)) as? [String: Any])
        XCTAssertNil(obj2["ctx"])
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

    func testErrorEventCarriesContextFingerprintAndOmitsWhenNil() {
        let ev = ReproItEvent.error(
            sig: "cae5a9d5", path: [], message: "boom", stack: [],
            source: nil, line: nil,
            context: ["fingerprint": [["field": "email", "len": 6, "charset": "ascii",
                                       "hasEmoji": false, "isEmpty": false, "isRtl": false]]],
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
        engine.recordError(message: "x", stack: ["f0"], source: nil, line: nil,
                           context: ["fingerprint": [["field": "#0", "len": 0, "charset": "ascii",
                                                      "hasEmoji": false, "isEmpty": true, "isRtl": false]]])
        guard case let .error(_, _, _, _, _, _, context, _) = events.last else {
            return XCTFail("not an error")
        }
        XCTAssertNotNil(context?["fingerprint"])
    }
}
