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
                new RawNode("", true),         // clickable + unnamed -> unlabeled
            };
            var tree = Screen(
                new Node("button") { Id = "home" },
                new Node("button") { Id = "settings" });
            var snap = engine.Reduce(nodes, tree, "/home");
            Assert.Equal(2, snap.Labels.Count);
            Assert.Equal(1, snap.Unlabeled);
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
            engine.NoteTap("Open Settings");
            clock = 2000L;
            engine.Observe(engine.Reduce(new List<RawNode> { new RawNode("Settings", false), new RawNode("Back", false) }, settings, "/settings"));

            Assert.Equal(2, captured.Count);
            var load = captured[0];
            Assert.Equal("edge", load["kind"]);
            Assert.False(load.ContainsKey("from"));
            Assert.Equal("load", load["action"]);
            Assert.Equal(homeSig, load["to"]);

            var tap = captured[1];
            Assert.Equal("tap:Open Settings", tap["action"]);
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
                { "action", "tap:Open \"Settings\"" },
                { "to", "054d1bbf" },
                { "labels", new List<object> { "Settings", "Back" } },
                { "skip", null }, // null fields are omitted
                { "t", 1_717_939_200_123L },
            };
            string body = engine.BuildBatch(new List<IDictionary<string, object>> { ev });
            Assert.Equal(
                "{\"appId\":\"example\",\"sentAt\":1717939200123,\"events\":" +
                "[{\"kind\":\"edge\",\"action\":\"tap:Open \\\"Settings\\\"\"," +
                "\"to\":\"054d1bbf\",\"labels\":[\"Settings\",\"Back\"]," +
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
    }
}
