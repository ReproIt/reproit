package com.reproit.android

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.File

/**
 * THE Android parity gate for the canonical STRUCTURAL screen signature.
 *
 * It loads the canonical golden vectors at the repo root
 * (`signature_vectors.json`) and asserts the Kotlin implementation produces
 * `expected_sig` for every vector, exactly as the Rust oracle's
 * `tests::golden_vectors_match` and the Flutter / web parity tests do. If a
 * vector mismatches, the failure prints the descriptor string so you can diff it
 * against docs/signature.md before touching anything. Never edit the vectors or
 * the oracle to make this pass.
 *
 * The descriptor that gets hashed is byte-identical to the Rust oracle:
 *   token = <depth>:<role>[:<type>][#<icon>][@<id>] (trailing `*` on a repeat)
 *   desc  = "A:" + anchor + "\n" + tokens.join(";")
 *   sig   = FNV-1a 32-bit over UTF-8(desc), 8-char lowercase hex.
 *
 * This test imports only pure-Kotlin classes ([Signature], [Engine], [Json],
 * [Fingerprint]) and has NO `android.*` dependency, so it runs on the host JVM
 * without the Android SDK.
 */
class SignatureParityTest {

    /** One golden vector from signature_vectors.json. */
    private data class Vector(
        val description: String,
        val anchor: String?,
        val tree: Signature.Node,
        val expectedSig: String,
    )

    private fun loadVectors(): List<Vector> {
        // signature_vectors.json lives at the repo root. The host test runs with
        // CWD = sdk/reproit-android, so the repo root is two levels up; probe a
        // few candidates so it also works from the repo root.
        val candidates = listOf(
            "signature_vectors.json",
            "../../signature_vectors.json",
            "../../../signature_vectors.json",
        )
        val file = candidates.map { File(it) }.firstOrNull { it.exists() }
            ?: error("could not locate signature_vectors.json (cwd=${File(".").absolutePath})")
        val raw = file.readText()
        @Suppress("UNCHECKED_CAST")
        val list = Json.decode(raw) as List<Map<String, Any?>>
        return list.map { j ->
            @Suppress("UNCHECKED_CAST")
            Vector(
                description = j["description"] as String,
                anchor = j["anchor"] as String?,
                tree = Signature.nodeFromJson(j["tree"] as Map<String, Any?>),
                expectedSig = j["expected_sig"] as String,
            )
        }
    }

    @Test
    fun goldenVectorsMatch() {
        val vectors = loadVectors()
        assertTrue("need >= 24 vectors, got ${vectors.size}", vectors.size >= 24)
        for (v in vectors) {
            val got = Signature.of(v.anchor, v.tree)
            assertEquals(
                "vector '${v.description}' mismatch.\n" +
                    "  descriptor = ${Signature.descriptor(v.anchor, v.tree)}\n" +
                    "  expected ${v.expectedSig} got $got",
                v.expectedSig,
                got,
            )
        }
    }

    @Test
    fun crossVectorRelationshipsHold() {
        val vectors = loadVectors()
        fun by(needle: String): String =
            vectors.firstOrNull { it.description.contains(needle) }?.expectedSig
                ?: error("no vector matching \"$needle\"")

        val login = by("basic login")
        // text-exclusion + transient-drop all collapse to the basic login.
        assertEquals(login, by("locale-invariance"))
        assertEquals(login, by("transient-drop (spinner)"))
        assertEquals(login, by("transient-drop (snackbar"))
        // collapse drops the count.
        assertEquals(by("repeated-collapse (3 items)"), by("repeated-collapse (5 items"))
        // discriminators split.
        assertNotEquals(login, by("collision-fix via input type"))
        assertNotEquals(login, by("collision-fix via icon"))
        assertNotEquals(by("collision-fix via input type"), by("collision-fix via icon"))
        // anchor semantics.
        val settings = by("same route + same structure")
        assertNotEquals(settings, by("different route + same structure"))
        assertNotEquals(settings, by("same route + different structure"))
        assertEquals(by("parameterized route (item 42)"), by("parameterized route (item 99)"))

        // value-state (Layer 2): EMPTY / ZERO / POS1 are three distinct states.
        val vEmpty = by("empty value-class")
        val vZero = by("zero value-class")
        val vPos1 = by("POS1 value-class")
        assertNotEquals(vEmpty, vZero)
        assertNotEquals(vEmpty, vPos1)
        assertNotEquals(vZero, vPos1)
        // numeric counter 0 vs 5 -> ZERO vs POS1 distinct.
        assertNotEquals(by("counter at 0"), by("counter at 5"))
        // grouped/locale number is locale-safe (NONEMPTY), distinct from numerics.
        val vGrouped = by("grouped/locale number")
        assertNotEquals(vGrouped, vPos1)
        assertNotEquals(vGrouped, vZero)
        // two different POS1 values (3 vs 7) bucket the same.
        assertEquals(
            by("two different POS1 values bucket the same (3)"),
            by("two different POS1 values bucket the same (7)"),
        )
    }

    @Test
    fun valueStateDescriptorShapeMatchesSpec() {
        // value_class buckets (docs/signature.md "Value-state").
        assertEquals("EMPTY", Signature.valueClass(""))
        assertEquals("EMPTY", Signature.valueClass("   "))
        assertEquals("ZERO", Signature.valueClass("0"))
        assertEquals("ZERO", Signature.valueClass("0.0"))
        assertEquals("ZERO", Signature.valueClass("-0"))
        assertEquals("NEG", Signature.valueClass("-3"))
        assertEquals("NEG", Signature.valueClass("-0.5"))
        assertEquals("POS1", Signature.valueClass("3"))
        assertEquals("POS1", Signature.valueClass("9.99"))
        assertEquals("POS1", Signature.valueClass("+7"))
        assertEquals("POS2", Signature.valueClass("10"))
        assertEquals("POS2", Signature.valueClass("99"))
        assertEquals("POS2", Signature.valueClass("  42  "))
        assertEquals("POS3", Signature.valueClass("100"))
        assertEquals("POS3", Signature.valueClass("999.99"))
        assertEquals("POSL", Signature.valueClass("1000"))
        assertEquals("POSL", Signature.valueClass("123456"))
        // locale-safe fallback: ambiguous numbers are NONEMPTY, not guessed.
        assertEquals("NONEMPTY", Signature.valueClass("1,234"))
        assertEquals("NONEMPTY", Signature.valueClass("1.234.567"))
        assertEquals("NONEMPTY", Signature.valueClass("\$5"))
        assertEquals("NONEMPTY", Signature.valueClass("5%"))
        assertEquals("NONEMPTY", Signature.valueClass("1e3"))
        assertEquals("NONEMPTY", Signature.valueClass("3.")) // trailing dot
        assertEquals("NONEMPTY", Signature.valueClass(".5")) // leading dot
        assertEquals("NONEMPTY", Signature.valueClass("hello"))

        // A textfield WITHOUT a value -> no V: section (byte-identical to before).
        assertEquals(
            "A:\n0:textfield@email",
            Signature.descriptor(null, Signature.Node(role = "textfield", id = "email")),
        )
        // A chrome node WITH a value is still not value-bearing: no V: section.
        assertEquals(
            "A:\n0:header@title",
            Signature.descriptor(
                null,
                Signature.Node(role = "header", id = "title", value = "Welcome"),
            ),
        )
        // A value-bearing textfield adds the V: section.
        assertEquals(
            "A:\n0:textfield@email\nV:key:email=NONEMPTY",
            Signature.descriptor(
                null,
                Signature.Node(role = "textfield", id = "email", value = "a@b.com"),
            ),
        )
        // status is a value-role but not in ROLES, so the body token is `node`.
        assertEquals(
            "A:\n0:node@count\nV:key:count=POS1",
            Signature.descriptor(
                null,
                Signature.Node(role = "status", id = "count", value = "5"),
            ),
        )
        // V: section is sorted by key (independent of document order).
        val screen = Signature.Node(
            role = "screen",
            children = listOf(
                Signature.Node(role = "textfield", id = "zeta", value = "0"),
                Signature.Node(role = "textfield", id = "alpha", value = "12"),
            ),
        )
        assertEquals(
            "A:\n0:screen;1:textfield@zeta;1:textfield@alpha\nV:key:alpha=POS2;key:zeta=ZERO",
            Signature.descriptor(null, screen),
        )
        // Keyless value nodes collapse structurally but survive in V: by index.
        val keyless = Signature.Node(
            role = "screen",
            children = listOf(
                Signature.Node(role = "textfield", value = "3"),
                Signature.Node(role = "textfield", value = "99"),
            ),
        )
        assertEquals(
            "A:\n0:screen;1:textfield*\nV:role:textfield#0=POS1;role:textfield#1=POS2",
            Signature.descriptor(null, keyless),
        )
        // Layer 3 opt-in: a chrome `text` role becomes value-bearing when flagged.
        assertEquals(
            "A:\n0:text@display",
            Signature.descriptor(
                null,
                Signature.Node(role = "text", id = "display", value = "42"),
            ),
        )
        assertEquals(
            "A:\n0:text@display\nV:key:display=POS2",
            Signature.descriptor(
                null,
                Signature.Node(role = "text", id = "display", value = "42", valueNode = true),
            ),
        )
        // Transient value subtree is dropped from both body and V: section.
        val transientVal = Signature.Node(
            role = "screen",
            children = listOf(
                Signature.Node(
                    role = "group",
                    transient = true,
                    children = listOf(
                        Signature.Node(role = "status", id = "loading", value = "50"),
                    ),
                ),
            ),
        )
        assertEquals("A:\n0:screen", Signature.descriptor(null, transientVal))
    }

    @Test
    fun fnv1aKnownValues() {
        // "" -> the FNV-1a 32-bit offset basis itself.
        assertEquals("811c9dc5", Signature.fnv1a32Hex(ByteArray(0)))
        // Cross-check a known FNV-1a 32 value for "a" = 0xe40c292c.
        assertEquals("e40c292c", Signature.fnv1a32Hex("a".toByteArray(Charsets.UTF_8)))
    }

    @Test
    fun descriptorShapeMatchesSpec() {
        // Empty anchor still has the A: prefix line.
        assertEquals("A:\n0:screen", Signature.descriptor(null, Signature.Node(role = "screen")))
        // Unknown role normalizes to node.
        assertEquals("A:\n0:node", Signature.descriptor(null, Signature.Node(role = "carousel")))
        // Token field order: type, icon, id.
        assertEquals(
            "A:\n0:textfield:password#lock@pwd",
            Signature.descriptor(
                null,
                Signature.Node(role = "textfield", type = "password", icon = "lock", id = "pwd"),
            ),
        )
        // Repeated siblings collapse to one *-marked token, count dropped.
        val list = Signature.Node(
            role = "list",
            children = listOf(
                Signature.Node(role = "listitem", children = listOf(Signature.Node(role = "text"))),
                Signature.Node(role = "listitem", children = listOf(Signature.Node(role = "text"))),
                Signature.Node(role = "listitem", children = listOf(Signature.Node(role = "text"))),
            ),
        )
        assertEquals("A:\n0:list;1:listitem*;2:text", Signature.descriptor(null, list))
        // Non-consecutive identical siblings are NOT collapsed.
        val g = Signature.Node(
            role = "group",
            children = listOf(
                Signature.Node(role = "button"),
                Signature.Node(role = "link"),
                Signature.Node(role = "button"),
            ),
        )
        assertEquals("A:\n0:group;1:button;1:link;1:button", Signature.descriptor(null, g))
        // Transient subtree dropped (spinner + its child).
        val withSpinner = Signature.Node(
            role = "screen",
            children = listOf(
                Signature.Node(role = "text"),
                Signature.Node(role = "spinner", children = listOf(Signature.Node(role = "text"))),
            ),
        )
        val withoutSpinner = Signature.Node(
            role = "screen",
            children = listOf(Signature.Node(role = "text")),
        )
        assertEquals(
            Signature.descriptor(null, withoutSpinner),
            Signature.descriptor(null, withSpinner),
        )
    }

    // ---- Engine behavior over the STRUCTURAL signature -----------------------

    /** A small structural tree helper for the Engine tests. */
    private fun screen(vararg children: Signature.Node) =
        Signature.Node(role = "screen", children = children.toList())

    @Test
    fun reduceComputesStructuralSigAndDisplayLabels() {
        val cfg = ReproItConfig(appId = "t", maxLabels = 2, maxLabelLen = 40)
        val engine = Engine(cfg)
        // Display-only label list (deduped + capped) is independent of the hash.
        val nodes = listOf(
            RawNode("Home", tappable = true),
            RawNode("Home", tappable = true),   // dup -> collapsed
            RawNode("Settings", tappable = true),
            RawNode("Profile", tappable = true), // over maxLabels cap of 2
            RawNode("", tappable = true),        // unnamed nodes are omitted from labels
        )
        val tree = screen(
            Signature.Node(role = "button", id = "home"),
            Signature.Node(role = "button", id = "settings"),
        )
        val snap = engine.reduce(nodes, tree, anchor = "/home")
        assertEquals(2, snap.labels.size)
        // The signature is the STRUCTURAL descriptor of the tree + anchor, NOT
        // a function of the localized labels.
        assertEquals(Signature.of("/home", tree), snap.sig)
    }

    @Test
    fun signatureExcludesLocalizedText() {
        // Two label sets, identical structure + anchor -> identical signature.
        val cfg = ReproItConfig(appId = "t")
        val engine = Engine(cfg)
        val tree = screen(
            Signature.Node(role = "header", id = "title"),
            Signature.Node(role = "button", id = "go"),
        )
        val en = engine.reduce(listOf(RawNode("Welcome", false), RawNode("Continue", false)), tree, "/login")
        val ja = engine.reduce(listOf(RawNode("ようこそ", false), RawNode("続ける", false)), tree, "/login")
        assertEquals(en.sig, ja.sig)
    }

    @Test
    fun cleanLabelTrimsFirstLineAndDropsLong() {
        val cfg = ReproItConfig(appId = "t", maxLabelLen = 5)
        val engine = Engine(cfg)
        assertEquals("hi", engine.cleanLabel("  hi\nthere "))
        assertEquals(null, engine.cleanLabel("   "))
        assertEquals(null, engine.cleanLabel("toolonglabel"))
    }

    @Test
    fun edgeAndErrorPayloadsMatchContract() {
        val captured = ArrayList<Map<String, Any?>>()
        val cfg = ReproItConfig(appId = "example", onEvent = { captured.add(it) })
        var clock = 1_000L
        val engine = Engine(cfg, now = { clock })

        val home = screen(Signature.Node(role = "header", id = "home"))
        val homeSig = Signature.of("/home", home)
        val settings = screen(
            Signature.Node(role = "header", id = "title"),
            Signature.Node(role = "switch", id = "notifications"),
        )
        val settingsSig = Signature.of("/settings", settings)

        // first observation -> load edge with from omitted
        engine.observe(engine.reduce(listOf(RawNode("Home Screen", false)), home, "/home"), "load")
        // tap then a state change -> structural action + display label
        engine.noteTap("key:open-settings", "Open Settings")
        clock = 2_000L
        engine.observe(
            engine.reduce(listOf(RawNode("Settings", false), RawNode("Back", false)), settings, "/settings"),
        )

        assertEquals(2, captured.size)
        val load = captured[0]
        assertEquals("edge", load["kind"])
        assertTrue(!load.containsKey("from")) // from omitted on first state
        assertEquals("load", load["action"])
        assertEquals(homeSig, load["to"])

        val tap = captured[1]
        assertEquals("tap:key:open-settings", tap["action"])
        assertEquals("Open Settings", tap["label"])
        assertEquals(homeSig, tap["from"])
        assertEquals(settingsSig, tap["to"])
        @Suppress("UNCHECKED_CAST")
        val labels = tap["labels"] as List<String>
        assertTrue(labels.contains("Settings") && labels.contains("Back"))

        // error carries current sig + path
        clock = 3_000L
        val err = engine.recordError("boom", listOf("a", "b"), source = "X.kt", line = 9)
        assertEquals("error", err["kind"])
        assertEquals(settingsSig, err["sig"])
        assertEquals(9, err["line"])
        @Suppress("UNCHECKED_CAST")
        val path = err["path"] as List<Map<String, Any?>>
        assertEquals(2, path.size)
    }

    @Test
    fun jsonEncodingShapeAndBatchEnvelope() {
        val cfg = ReproItConfig(appId = "example")
        val engine = Engine(cfg, now = { 1_717_939_200_123L })
        val ev = linkedMapOf<String, Any?>(
            "kind" to "edge",
            "action" to "tap:key:open-settings",
            "label" to "Open \"Settings\"",
            "to" to "054d1bbf",
            "labels" to listOf("Settings", "Back"),
            "skip" to null, // null fields are omitted
            "t" to 1_717_939_200_123L,
        )
        val body = engine.buildBatch(listOf(ev))
        assertEquals(
            "{\"appId\":\"example\",\"sentAt\":1717939200123,\"events\":" +
                "[{\"kind\":\"edge\",\"action\":\"tap:key:open-settings\"," +
                "\"label\":\"Open \\\"Settings\\\"\",\"to\":\"054d1bbf\",\"labels\":[\"Settings\",\"Back\"]," +
                "\"t\":1717939200123}]}",
            body,
        )
    }

    @Test
    fun jsonRoundTripsThroughDecoder() {
        // The decoder used by the parity gate is exercised on a representative
        // shape (objects, arrays, nested, string escapes, bool, null).
        @Suppress("UNCHECKED_CAST")
        val obj = Json.decode(
            "{\"a\":1,\"b\":\"x\\\"y\",\"c\":[true,false,null],\"d\":{\"e\":\"f\"}}",
        ) as Map<String, Any?>
        assertEquals(1.0, obj["a"])
        assertEquals("x\"y", obj["b"])
        @Suppress("UNCHECKED_CAST")
        val c = obj["c"] as List<Any?>
        assertEquals(listOf(true, false, null), c)
        @Suppress("UNCHECKED_CAST")
        val d = obj["d"] as Map<String, Any?>
        assertEquals("f", d["e"])
    }

    // ---- context / identify (production-telemetry CONTEXT API) ---------------

    @Test
    fun identifyHashesUserIdAndMergesContext() {
        val engine = Engine(ReproItConfig(appId = "example"))
        engine.identify("user-42", mapOf("plan" to "pro", "role" to "admin"))

        val ctx = engine.context()
        val uid = ctx["uid"] as String
        assertNotEquals("user-42", uid)
        assertFalse(uid.contains("user-42"))
        assertTrue(Regex("^[0-9a-f]{16}$").matches(uid))
        assertEquals("pro", ctx["plan"])
        assertEquals("admin", ctx["role"])
    }

    @Test
    fun identifyUidIsStableAcrossCalls() {
        val a = Engine(ReproItConfig(appId = "example")).apply { identify("alice") }.context()["uid"]
        val b = Engine(ReproItConfig(appId = "example")).apply { identify("alice") }.context()["uid"]
        val c = Engine(ReproItConfig(appId = "example")).apply { identify("bob") }.context()["uid"]
        assertEquals(a, b)
        assertNotEquals(a, c)
    }

    @Test
    fun setContextAndSetContextsMerge() {
        val engine = Engine(ReproItConfig(appId = "example"))
        engine.setContext("locale", "tr-TR")
        engine.setContexts(mapOf("platform" to "android", "tz" to "Europe/Istanbul"))
        engine.setContext("locale", "en-US") // last write wins

        val ctx = engine.context()
        assertEquals("en-US", ctx["locale"])
        assertEquals("android", ctx["platform"])
        assertEquals("Europe/Istanbul", ctx["tz"])
    }

    @Test
    fun batchEnvelopeOmitsCtxWhenEmpty() {
        val engine = Engine(ReproItConfig(appId = "example"), now = { 1_717_939_200_123L })
        val body = engine.buildBatch(emptyList())
        assertEquals(
            "{\"appId\":\"example\",\"sentAt\":1717939200123,\"events\":[]}",
            body,
        )
    }

    @Test
    fun batchEnvelopeIncludesCtxWhenSetWithExactShape() {
        val engine = Engine(ReproItConfig(appId = "example"), now = { 1_717_939_200_123L })
        engine.setContexts(
            linkedMapOf(
                "platform" to "android",
                "locale" to "en-US",
                "tz" to "America/New_York",
            ),
        )
        val ev = linkedMapOf<String, Any?>(
            "kind" to "edge",
            "action" to "load",
            "to" to "811c9dc5",
            "t" to 1_717_939_200_123L,
        )
        val body = engine.buildBatch(listOf(ev))
        assertEquals(
            "{\"appId\":\"example\",\"sentAt\":1717939200123," +
                "\"ctx\":{\"platform\":\"android\",\"locale\":\"en-US\"," +
                "\"tz\":\"America/New_York\"}," +
                "\"events\":[{\"kind\":\"edge\",\"action\":\"load\"," +
                "\"to\":\"811c9dc5\",\"t\":1717939200123}]}",
            body,
        )
    }

    @Test
    fun identifiedBatchEnvelopeCarriesHashedUidNotRawValue() {
        val engine = Engine(ReproItConfig(appId = "example"), now = { 1_717_939_200_123L })
        engine.identify("secret-user")
        val body = engine.buildBatch(emptyList())
        assertFalse("raw user id must never appear in the wire body", body.contains("secret-user"))
        assertTrue(body.contains("\"ctx\":{\"uid\":\""))
    }

    // ---- PII-safe input fingerprint (tier-3 on-error context) ----------------

    @Test
    fun fingerprintJoseEmojiIsUnicodeAndEmoji() {
        val r = Fingerprint.fingerprintValue("José🎉")
        assertEquals(5, r["len"])
        assertEquals("unicode", r["charset"])
        assertEquals(true, r["hasEmoji"])
        assertEquals(false, r["isEmpty"])
        assertEquals(false, r["isRtl"])
    }

    @Test
    fun fingerprintNumericAsciiEmptyAndWhitespace() {
        assertEquals("numeric", Fingerprint.fingerprintValue("12345")["charset"])
        assertEquals("ascii", Fingerprint.fingerprintValue("hello")["charset"])
        val empty = Fingerprint.fingerprintValue("")
        assertEquals(true, empty["isEmpty"])
        assertEquals(0, empty["len"])
        assertEquals("ascii", empty["charset"])
        assertEquals(true, Fingerprint.fingerprintValue("   ")["isEmpty"])
    }

    @Test
    fun fingerprintRtlTurkishAndLength() {
        val ar = Fingerprint.fingerprintValue("مرحبا")
        assertEquals(true, ar["isRtl"])
        assertEquals("unicode", ar["charset"])
        assertEquals(false, ar["hasEmoji"])
        assertEquals(true, Fingerprint.fingerprintValue("שלום")["isRtl"])
        assertEquals("unicode", Fingerprint.fingerprintValue("ıstanbul")["charset"])
        val long = Fingerprint.fingerprintValue("a".repeat(312))
        assertEquals(312, long["len"])
        assertEquals("ascii", long["charset"])
    }

    @Test
    fun fingerprintNeverEchoesRawValue() {
        val raw = "secret-pii-value"
        val json = Json.encode(Fingerprint.fingerprintValue(raw))
        assertFalse(json.contains(raw))
    }

    @Test
    fun fingerprintFieldsKeepsLabelDropsValue() {
        val out = Fingerprint.fingerprintFields(
            listOf("email" to "a@b.co", "#1" to "12345", "note" to ""),
        )
        assertEquals(3, out.size)
        assertEquals("email", out[0]["field"])
        assertEquals("numeric", out[1]["charset"])
        assertEquals(true, out[2]["isEmpty"])
        assertFalse(Json.encode(out).contains("a@b.co"))
    }

    @Test
    fun errorEventCarriesContextFingerprintAndOmitsWhenNull() {
        val engine = Engine(ReproItConfig(appId = "example"), now = { 1L })
        val fp = Fingerprint.fingerprintFields(listOf("email" to "a@b.co"))
        val ev = engine.recordError("boom", listOf("f0"), context = mapOf("fingerprint" to fp))
        @Suppress("UNCHECKED_CAST")
        val context = ev["context"] as Map<String, Any?>
        assertTrue(context.containsKey("fingerprint"))

        val ev2 = engine.recordError("boom2", listOf("f0"))
        assertFalse(ev2.containsKey("context"))
    }

    // ---- v2 fingerprint features (bytes / scripts / combining / zw / nl / ws) -

    @Test
    fun fingerprintBytesIsUtf8LengthDistinctFromCodePointLen() {
        // J o s é(2B) 🎉(4B) -> 9 bytes, 5 code points.
        val r = Fingerprint.fingerprintValue("José🎉")
        assertEquals(5, r["len"])
        assertEquals(9, r["bytes"])
        assertEquals(5, Fingerprint.fingerprintValue("hello")["bytes"]) // ascii: bytes == len
    }

    @Test
    fun fingerprintGraphemesCountUserVisibleClusters() {
        assertEquals(5, Fingerprint.fingerprintValue("hello")["graphemes"])
        assertEquals(2, Fingerprint.fingerprintValue("é")["len"])
        assertEquals(1, Fingerprint.fingerprintValue("é")["graphemes"])
        assertEquals(1, Fingerprint.fingerprintValue("👨‍👩‍👧‍👦")["graphemes"])
    }

    @Test
    fun fingerprintScriptsListsBucketsSortedMixedScript() {
        assertEquals(listOf("Latin"), Fingerprint.fingerprintValue("hello")["scripts"])
        val ar = Fingerprint.fingerprintValue("مرحبا") // مرحبا
        assertEquals(listOf("Arabic"), ar["scripts"])
        assertEquals(true, ar["isRtl"])
        assertEquals(
            listOf("Arabic", "Latin"),
            Fingerprint.fingerprintValue("hi مرحبا")["scripts"], // hi مرحبا
        )
        assertEquals(
            listOf("CJK"),
            Fingerprint.fingerprintValue("日本語")["scripts"], // 日本語
        )
        assertEquals(emptyList<String>(), Fingerprint.fingerprintValue("12345")["scripts"])
    }

    @Test
    fun fingerprintHasNewlineDetectsLfAndCr() {
        assertEquals(true, Fingerprint.fingerprintValue("line1\nline2")["hasNewline"])
        assertEquals(true, Fingerprint.fingerprintValue("a\rb")["hasNewline"])
        assertEquals(false, Fingerprint.fingerprintValue("oneline")["hasNewline"])
    }

    @Test
    fun fingerprintHasZeroWidthDetectsInvisibleCodePoints() {
        assertEquals(true, Fingerprint.fingerprintValue("a​b")["hasZeroWidth"]) // ZWSP
        assertEquals(true, Fingerprint.fingerprintValue("a‍b")["hasZeroWidth"]) // ZWJ
        assertEquals(false, Fingerprint.fingerprintValue("ab")["hasZeroWidth"])
    }

    @Test
    fun fingerprintHasCombiningMarksDetectsDecomposedAccents() {
        // e + combining acute (U+0301).
        assertEquals(true, Fingerprint.fingerprintValue("é")["hasCombiningMarks"])
        assertEquals(false, Fingerprint.fingerprintValue("e")["hasCombiningMarks"])
        // precomposed é (U+00E9).
        assertEquals(false, Fingerprint.fingerprintValue("é")["hasCombiningMarks"])
    }

    @Test
    fun fingerprintLeadingTrailingWhitespaceFlagsEdgeWhitespace() {
        assertEquals(true, Fingerprint.fingerprintValue(" hello")["leadingTrailingWhitespace"])
        assertEquals(true, Fingerprint.fingerprintValue("hello ")["leadingTrailingWhitespace"])
        assertEquals(false, Fingerprint.fingerprintValue("hello")["leadingTrailingWhitespace"])
        // interior tab only -> not edge whitespace.
        assertEquals(false, Fingerprint.fingerprintValue("a\tb")["leadingTrailingWhitespace"])
    }

    @Test
    fun fingerprintFpVersionIsTwo() {
        assertEquals(2, Fingerprint.FP_VERSION)
    }
}
