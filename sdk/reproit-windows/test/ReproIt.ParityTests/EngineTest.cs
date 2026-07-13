// Engine + JSON + Fingerprint behavior tests. These mirror the Android
// SignatureParityTest's Engine / context / fingerprint cases, pinning the exact
// wire shape ({appId, sentAt, ctx?, events}), the edge/error state machine, the
// SHA-256 uid hashing, and the PII-safe input fingerprint. They reference only
// ReproIt.Core, so they run cross-platform on the .NET SDK.

using System.Collections.Generic;
using System.Text.RegularExpressions;
using ReproIt.Core;
using Xunit;

namespace ReproIt.ParityTests
{
    public class EngineTest
    {
        private static Node Screen(params Node[] children)
        {
            var s = new Node("screen");
            s.Children = new List<Node>(children);
            return s;
        }

        [Fact]
        public void ReduceComputesStructuralSigAndDisplayLabels()
        {
            var cfg = new ReproItConfig("t") { MaxLabels = 2, MaxLabelLen = 40 };
            var engine = new Engine(cfg);
            var nodes = new List<RawNode>
            {
                new RawNode("Home", true),
                new RawNode("Home", true),     // dup -> collapsed
                new RawNode("Settings", true),
                new RawNode("Profile", true),  // over MaxLabels cap of 2
                new RawNode("", true),         // unnamed -> omitted from display labels
            };
            var tree = Screen(
                new Node("button") { Id = "home" },
                new Node("button") { Id = "settings" });
            var snap = engine.Reduce(nodes, tree, "/home");
            Assert.Equal(2, snap.Labels.Count);
            Assert.Equal(Signature.Of("/home", tree), snap.Sig);
        }

        [Fact]
        public void SignatureExcludesLocalizedText()
        {
            var engine = new Engine(new ReproItConfig("t"));
            var tree = Screen(
                new Node("header") { Id = "title" },
                new Node("button") { Id = "go" });
            var en = engine.Reduce(new List<RawNode> { new RawNode("Welcome", false), new RawNode("Continue", false) }, tree, "/login");
            var ja = engine.Reduce(new List<RawNode> { new RawNode("ようこそ", false), new RawNode("続ける", false) }, tree, "/login");
            Assert.Equal(en.Sig, ja.Sig);
        }

        [Fact]
        public void EdgeAndErrorPayloadsMatchContract()
        {
            var captured = new List<IDictionary<string, object>>();
            var cfg = new ReproItConfig("example") { OnEvent = e => captured.Add(e) };
            long clock = 1000L;
            var engine = new Engine(cfg, now: () => clock);

            var home = Screen(new Node("header") { Id = "home" });
            string homeSig = Signature.Of("/home", home);
            var settings = Screen(
                new Node("header") { Id = "title" },
                new Node("switch") { Id = "notifications" });
            string settingsSig = Signature.Of("/settings", settings);

            engine.Observe(engine.Reduce(new List<RawNode> { new RawNode("Home Screen", false) }, home, "/home"), "load");
            engine.NoteTap("key:open-settings", "Open Settings");
            clock = 2000L;
            engine.Observe(engine.Reduce(new List<RawNode> { new RawNode("Settings", false), new RawNode("Back", false) }, settings, "/settings"));

            Assert.Equal(2, captured.Count);
            var load = captured[0];
            Assert.Equal("edge", load["kind"]);
            Assert.False(load.ContainsKey("from"));
            Assert.Equal("load", load["action"]);
            Assert.Equal(homeSig, load["to"]);

            var tap = captured[1];
            Assert.Equal("tap:key:open-settings", tap["action"]);
            Assert.Equal("Open Settings", tap["label"]);
            Assert.Equal(homeSig, tap["from"]);
            Assert.Equal(settingsSig, tap["to"]);

            clock = 3000L;
            var err = engine.RecordError("boom", new List<string> { "a", "b" }, source: "X.cs", line: 9);
            Assert.Equal("error", err["kind"]);
            Assert.Equal(settingsSig, err["sig"]);
            Assert.Equal(9, err["line"]);
            var path = (List<object>)err["path"];
            Assert.Equal(2, path.Count);
        }

        [Fact]
        public void BatchEnvelopeOmitsCtxWhenEmpty()
        {
            var engine = new Engine(new ReproItConfig("example"), now: () => 1_717_939_200_123L);
            string body = engine.BuildBatch(new List<IDictionary<string, object>>());
            Assert.Equal("{\"appId\":\"example\",\"sentAt\":1717939200123,\"events\":[]}", body);
        }

        [Fact]
        public void BatchEnvelopeIncludesCtxWhenSetWithExactShape()
        {
            var engine = new Engine(new ReproItConfig("example"), now: () => 1_717_939_200_123L);
            engine.SetContexts(new Dictionary<string, object>
            {
                { "platform", "winui" },
                { "locale", "en-US" },
                { "tz", "America/New_York" },
            });
            var ev = new Dictionary<string, object>
            {
                { "kind", "edge" },
                { "action", "load" },
                { "to", "811c9dc5" },
                { "t", 1_717_939_200_123L },
            };
            string body = engine.BuildBatch(new List<IDictionary<string, object>> { ev });
            Assert.Equal(
                "{\"appId\":\"example\",\"sentAt\":1717939200123," +
                "\"ctx\":{\"platform\":\"winui\",\"locale\":\"en-US\"," +
                "\"tz\":\"America/New_York\"}," +
                "\"events\":[{\"kind\":\"edge\",\"action\":\"load\"," +
                "\"to\":\"811c9dc5\",\"t\":1717939200123}]}",
                body);
        }

        [Fact]
        public void IdentifyHashesUserIdAndMergesContext()
        {
            var engine = new Engine(new ReproItConfig("example"));
            engine.Identify("user-42", new Dictionary<string, object> { { "plan", "pro" }, { "role", "admin" } });
            var ctx = engine.Context();
            var uid = (string)ctx["uid"];
            Assert.NotEqual("user-42", uid);
            Assert.DoesNotContain("user-42", uid);
            Assert.Matches(new Regex("^[0-9a-f]{16}$"), uid);
            Assert.Equal("pro", ctx["plan"]);
            Assert.Equal("admin", ctx["role"]);
        }

        [Fact]
        public void IdentifiedBatchEnvelopeCarriesHashedUidNotRawValue()
        {
            var engine = new Engine(new ReproItConfig("example"), now: () => 1_717_939_200_123L);
            engine.Identify("secret-user");
            string body = engine.BuildBatch(new List<IDictionary<string, object>>());
            Assert.DoesNotContain("secret-user", body);
            Assert.Contains("\"ctx\":{\"uid\":\"", body);
        }

        [Fact]
        public void JsonEncodingEscapesAndOmitsNulls()
        {
            var engine = new Engine(new ReproItConfig("example"), now: () => 1_717_939_200_123L);
            var ev = new Dictionary<string, object>
            {
                { "kind", "edge" },
                { "action", "tap:key:open-settings" },
                { "label", "Open \"Settings\"" },
                { "to", "054d1bbf" },
                { "labels", new List<object> { "Settings", "Back" } },
                { "skip", null }, // null fields are omitted
                { "t", 1_717_939_200_123L },
            };
            string body = engine.BuildBatch(new List<IDictionary<string, object>> { ev });
            Assert.Equal(
                "{\"appId\":\"example\",\"sentAt\":1717939200123,\"events\":" +
                "[{\"kind\":\"edge\",\"action\":\"tap:key:open-settings\"," +
                "\"label\":\"Open \\\"Settings\\\"\",\"to\":\"054d1bbf\",\"labels\":[\"Settings\",\"Back\"]," +
                "\"t\":1717939200123}]}",
                body);
        }

        [Fact]
        public void FingerprintFeaturesAndNeverEchoesRawValue()
        {
            var jose = Fingerprint.FingerprintValue("José🎉");
            Assert.Equal(5, jose["len"]);
            Assert.Equal("unicode", jose["charset"]);
            Assert.Equal(true, jose["hasEmoji"]);
            Assert.Equal(false, jose["isEmpty"]);
            Assert.Equal(false, jose["isRtl"]);

            Assert.Equal("numeric", Fingerprint.FingerprintValue("12345")["charset"]);
            Assert.Equal("ascii", Fingerprint.FingerprintValue("hello")["charset"]);
            var empty = Fingerprint.FingerprintValue("");
            Assert.Equal(true, empty["isEmpty"]);
            Assert.Equal(0, empty["len"]);

            var ar = Fingerprint.FingerprintValue("مرحبا");
            Assert.Equal(true, ar["isRtl"]);
            Assert.Equal("unicode", ar["charset"]);

            string raw = "secret-pii-value";
            string json = Json.Encode(Fingerprint.FingerprintValue(raw));
            Assert.DoesNotContain(raw, json);

            var fields = Fingerprint.FingerprintFields(new List<KeyValuePair<string, string>>
            {
                new KeyValuePair<string, string>("email", "a@b.co"),
                new KeyValuePair<string, string>("#1", "12345"),
                new KeyValuePair<string, string>("note", ""),
            });
            Assert.Equal(3, fields.Count);
            Assert.Equal("email", fields[0]["field"]);
            Assert.Equal("numeric", fields[1]["charset"]);
            Assert.Equal(true, fields[2]["isEmpty"]);
            Assert.DoesNotContain("a@b.co", Json.Encode(fields));
        }

        [Fact]
        public void FingerprintV2Features()
        {
            // bytes: UTF-8 length, distinct from code-point len. "Jos\u00E9" + U+1F389.
            var jose = Fingerprint.FingerprintValue("Jos\u00E9\U0001F389");
            Assert.Equal(5, (int)jose["len"]);
            Assert.Equal(9, (int)jose["bytes"]);
            Assert.Equal(5, (int)Fingerprint.FingerprintValue("hello")["bytes"]);
            Assert.Equal(5, (int)Fingerprint.FingerprintValue("hello")["graphemes"]);
            Assert.Equal(2, (int)Fingerprint.FingerprintValue("e\u0301")["len"]);
            Assert.Equal(1, (int)Fingerprint.FingerprintValue("e\u0301")["graphemes"]);
            Assert.Equal(1, (int)Fingerprint.FingerprintValue("👨‍👩‍👧‍👦")["graphemes"]);

            // scripts: sorted unique buckets present.
            Assert.Equal(new List<string> { "Latin" }, (List<string>)Fingerprint.FingerprintValue("hello")["scripts"]);
            Assert.Equal(new List<string> { "Arabic" }, (List<string>)Fingerprint.FingerprintValue("\u0645\u0631\u062D\u0628\u0627")["scripts"]);
            Assert.Equal(new List<string> { "Arabic", "Latin" }, (List<string>)Fingerprint.FingerprintValue("hi \u0645\u0631\u062D\u0628\u0627")["scripts"]);
            Assert.Equal(new List<string> { "CJK" }, (List<string>)Fingerprint.FingerprintValue("\u65E5\u672C\u8A9E")["scripts"]);
            Assert.Empty((List<string>)Fingerprint.FingerprintValue("12345")["scripts"]);

            // hasNewline
            Assert.True((bool)Fingerprint.FingerprintValue("line1\nline2")["hasNewline"]);
            Assert.False((bool)Fingerprint.FingerprintValue("oneline")["hasNewline"]);

            // hasZeroWidth (U+200B ZWSP)
            Assert.True((bool)Fingerprint.FingerprintValue("a\u200Bb")["hasZeroWidth"]);
            Assert.False((bool)Fingerprint.FingerprintValue("ab")["hasZeroWidth"]);

            // hasCombiningMarks: "e" + U+0301 (decomposed) true; precomposed U+00E9 false.
            Assert.True((bool)Fingerprint.FingerprintValue("e\u0301")["hasCombiningMarks"]);
            Assert.False((bool)Fingerprint.FingerprintValue("\u00E9")["hasCombiningMarks"]);
            Assert.False((bool)Fingerprint.FingerprintValue("e")["hasCombiningMarks"]);

            // leadingTrailingWhitespace
            Assert.True((bool)Fingerprint.FingerprintValue(" hello")["leadingTrailingWhitespace"]);
            Assert.True((bool)Fingerprint.FingerprintValue("hello ")["leadingTrailingWhitespace"]);
            Assert.False((bool)Fingerprint.FingerprintValue("hello")["leadingTrailingWhitespace"]);
            Assert.False((bool)Fingerprint.FingerprintValue("a\tb")["leadingTrailingWhitespace"]);

            Assert.Equal(2, Fingerprint.FpVersion);
        }

        // Dogfood the app-invariant oracle both directions. Under the fuzzer
        // (REPROIT_UNDER_FUZZER set), Observe writes a REPROIT_INVARIANT marker to
        // stderr listing ONLY the violations; the UIA runner scrapes it and re-emits
        // EXPLORE:INVARIANT. A satisfied registry and a production run write nothing.
        private static string CaptureObserve(Engine engine, Snapshot snap, bool underFuzzer)
        {
            var prev = System.Console.Error;
            var sw = new System.IO.StringWriter();
            System.Environment.SetEnvironmentVariable(
                "REPROIT_UNDER_FUZZER", underFuzzer ? "1" : null);
            System.Console.SetError(sw);
            try
            {
                engine.Observe(snap);
            }
            finally
            {
                System.Console.SetError(prev);
                System.Environment.SetEnvironmentVariable("REPROIT_UNDER_FUZZER", null);
            }
            foreach (var line in sw.ToString().Split('\n'))
            {
                if (line.StartsWith("REPROIT_INVARIANT "))
                {
                    return line.Substring("REPROIT_INVARIANT ".Length).Trim();
                }
            }
            return null;
        }

        [Fact]
        public void InvariantReportsOnlyViolationsUnderTheFuzzer()
        {
            var engine = new Engine(new ReproItConfig("t"));
            engine.Invariant("holds", () => InvariantResult.Ok());
            engine.Invariant("neg", () => InvariantResult.Fail("balance < 0"));
            engine.Invariant("throws", () => throw new System.InvalidOperationException("kaboom"));

            var tree = Screen(new Node("button") { Id = "home" });
            var snap = engine.Reduce(new List<RawNode> { new RawNode("Home", true) }, tree, "/home");

            var marker = CaptureObserve(engine, snap, underFuzzer: true);
            Assert.NotNull(marker);
            var obj = (IDictionary<string, object>)Json.Decode(marker);
            Assert.Equal(snap.Sig, obj["sig"]);
            var byId = new Dictionary<string, string>();
            foreach (var it in (IList<object>)obj["items"])
            {
                var m = (IDictionary<string, object>)it;
                byId[(string)m["id"]] = (string)m["message"];
            }
            Assert.Equal(2, byId.Count);
            Assert.Equal("balance < 0", byId["neg"]);
            Assert.Equal("kaboom", byId["throws"]);
            Assert.False(byId.ContainsKey("holds"));
        }

        [Fact]
        public void InvariantSilentWhenCleanOrUngated()
        {
            var engine = new Engine(new ReproItConfig("t"));
            engine.Invariant("holds", () => InvariantResult.Ok());
            var tree = Screen(new Node("button") { Id = "home" });
            var snap = engine.Reduce(new List<RawNode> { new RawNode("Home", true) }, tree, "/home");
            // A satisfied registry writes nothing even under the fuzzer.
            Assert.Null(CaptureObserve(engine, snap, underFuzzer: true));

            // Inert in production: a violation writes nothing when the gate is unset.
            var e2 = new Engine(new ReproItConfig("t"));
            e2.Invariant("violated", () => InvariantResult.Fail("bad"));
            var s2 = e2.Reduce(new List<RawNode> { new RawNode("Home", true) }, tree, "/other");
            Assert.Null(CaptureObserve(e2, s2, underFuzzer: false));
        }
    }
}
