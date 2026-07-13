// THE .NET parity gate for the canonical STRUCTURAL screen signature.
//
// It loads the canonical golden vectors at the repo root (signature_vectors.json)
// and asserts the C# implementation produces expected_sig for every vector,
// exactly as the Rust oracle's tests::golden_vectors_match and the Android / iOS /
// web parity tests do. If a vector mismatches, the failure prints the descriptor
// string so you can diff it against docs/signature.md before touching anything.
// Never edit the vectors or the oracle to make this pass.
//
// The descriptor that gets hashed is byte-identical to the Rust oracle:
//   token = <depth>:<role>[:<type>][#<icon>][@<id>] (trailing `*` on a repeat)
//   desc  = "A:" + anchor + "\n" + tokens.join(";") (+ a trailing "\nV:" section)
//   sig   = FNV-1a 32-bit over UTF-8(desc), 8-char lowercase hex.
//
// This test references ONLY the cross-platform ReproIt.Core project (the
// signature core, the dependency-free JSON reader), so it runs on the plain
// .NET SDK on macOS / Linux / Windows without any WPF / WinUI dependency.

using System;
using System.Collections.Generic;
using System.IO;
using ReproIt.Core;
using Xunit;

namespace ReproIt.ParityTests
{
    public class SignatureParityTest
    {
        private sealed class Vector
        {
            public string Description;
            public string Anchor;
            public Node Tree;
            public string ExpectedSig;
        }

        private static Node NodeFromJson(IDictionary<string, object> j)
        {
            var node = new Node((string)j["role"]);
            node.Id = j.TryGetValue("id", out var id) ? id as string : null;
            node.Type = j.TryGetValue("type", out var type) ? type as string : null;
            node.Icon = j.TryGetValue("icon", out var icon) ? icon as string : null;
            node.Transient = j.TryGetValue("transient", out var tr) && tr is bool b && b;
            node.Value = j.TryGetValue("value", out var val) ? val as string : null;
            node.ValueNode = j.TryGetValue("value_node", out var vn) && vn is bool vb && vb;
            var children = new List<Node>();
            if (j.TryGetValue("children", out var kids) && kids is List<object> list)
            {
                foreach (var c in list)
                {
                    children.Add(NodeFromJson((IDictionary<string, object>)c));
                }
            }
            node.Children = children;
            return node;
        }

        private static List<Vector> LoadVectors()
        {
            // signature_vectors.json lives at the repo root. The test runs with CWD
            // somewhere under sdk/reproit-windows/test/...; probe a few candidates so
            // it also works from the repo root.
            string[] candidates =
            {
                "signature_vectors.json",
                "../../signature_vectors.json",
                "../../../signature_vectors.json",
                "../../../../signature_vectors.json",
                "../../../../../signature_vectors.json",
                "../../../../../../signature_vectors.json",
            };
            string path = null;
            foreach (var c in candidates)
            {
                if (File.Exists(c))
                {
                    path = c;
                    break;
                }
            }
            if (path == null)
            {
                throw new FileNotFoundException(
                    "could not locate signature_vectors.json (cwd=" + Directory.GetCurrentDirectory() + ")");
            }
            var raw = File.ReadAllText(path);
            var list = (List<object>)Json.Decode(raw);
            var vectors = new List<Vector>();
            foreach (var item in list)
            {
                var j = (IDictionary<string, object>)item;
                vectors.Add(new Vector
                {
                    Description = (string)j["description"],
                    Anchor = j.TryGetValue("anchor", out var a) ? a as string : null,
                    Tree = NodeFromJson((IDictionary<string, object>)j["tree"]),
                    ExpectedSig = (string)j["expected_sig"],
                });
            }
            return vectors;
        }

        [Fact]
        public void GoldenVectorsMatch()
        {
            var vectors = LoadVectors();
            Assert.True(vectors.Count >= 24, "need >= 24 vectors, got " + vectors.Count);
            foreach (var v in vectors)
            {
                string got = Signature.Of(v.Anchor, v.Tree);
                Assert.True(
                    got == v.ExpectedSig,
                    "vector '" + v.Description + "' mismatch.\n" +
                    "  descriptor = " + Signature.Descriptor(v.Anchor, v.Tree) + "\n" +
                    "  expected " + v.ExpectedSig + " got " + got);
            }
        }

        [Fact]
        public void CrossVectorRelationshipsHold()
        {
            var vectors = LoadVectors();
            string By(string needle)
            {
                foreach (var v in vectors)
                {
                    if (v.Description.Contains(needle))
                    {
                        return v.ExpectedSig;
                    }
                }
                throw new Xunit.Sdk.XunitException("no vector matching \"" + needle + "\"");
            }

            string login = By("basic login");
            // text-exclusion + transient-drop all collapse to the basic login.
            Assert.Equal(login, By("locale-invariance"));
            Assert.Equal(login, By("transient-drop (spinner)"));
            Assert.Equal(login, By("transient-drop (snackbar"));
            // collapse drops the count.
            Assert.Equal(By("repeated-collapse (3 items)"), By("repeated-collapse (5 items"));
            // discriminators split.
            Assert.NotEqual(login, By("collision-fix via input type"));
            Assert.NotEqual(login, By("collision-fix via icon"));
            Assert.NotEqual(By("collision-fix via input type"), By("collision-fix via icon"));
            // anchor semantics.
            string settings = By("same route + same structure");
            Assert.NotEqual(settings, By("different route + same structure"));
            Assert.NotEqual(settings, By("same route + different structure"));
            Assert.Equal(By("parameterized route (item 42)"), By("parameterized route (item 99)"));

            // value-state (Layer 2): EMPTY / ZERO / POS1 are three distinct states.
            string vEmpty = By("empty value-class");
            string vZero = By("zero value-class");
            string vPos1 = By("POS1 value-class");
            Assert.NotEqual(vEmpty, vZero);
            Assert.NotEqual(vEmpty, vPos1);
            Assert.NotEqual(vZero, vPos1);
            // numeric counter 0 vs 5 -> ZERO vs POS1 distinct.
            Assert.NotEqual(By("counter at 0"), By("counter at 5"));
            // grouped/locale number is locale-safe (NONEMPTY), distinct from numerics.
            string vGrouped = By("grouped/locale number");
            Assert.NotEqual(vGrouped, vPos1);
            Assert.NotEqual(vGrouped, vZero);
            // two different POS1 values (3 vs 7) bucket the same.
            Assert.Equal(
                By("two different POS1 values bucket the same (3)"),
                By("two different POS1 values bucket the same (7)"));
        }

        [Fact]
        public void ValueClassBucketsMatchOracle()
        {
            var cases = new (string Input, string Expected)[]
            {
                ("", "EMPTY"), ("   ", "EMPTY"), ("0", "ZERO"), ("0.0", "ZERO"), ("-0", "ZERO"),
                ("-3", "NEG"), ("-0.5", "NEG"), ("3", "POS1"), ("9.99", "POS1"), ("+7", "POS1"),
                ("10", "POS2"), ("99", "POS2"), ("100", "POS3"), ("999.99", "POS3"),
                ("1000", "POSL"), ("123456", "POSL"), ("  42  ", "POS2"),
                ("1,234", "NONEMPTY"), ("1.234.567", "NONEMPTY"), ("1 234", "NONEMPTY"),
                ("$5", "NONEMPTY"), ("5%", "NONEMPTY"), ("1e3", "NONEMPTY"), ("0x10", "NONEMPTY"),
                (".", "NONEMPTY"), ("3.", "NONEMPTY"), (".5", "NONEMPTY"), ("--5", "NONEMPTY"),
                ("hello", "NONEMPTY"), ("١٢٣", "NONEMPTY"), // non-ASCII (Arabic) digits
            };
            foreach (var c in cases)
            {
                Assert.True(
                    Signature.ValueClass(c.Input) == c.Expected,
                    "value_class(" + Json.Encode(c.Input) + ") expected " + c.Expected + " got " + Signature.ValueClass(c.Input));
            }
        }

        [Fact]
        public void Fnv1aKnownValues()
        {
            // "" -> the FNV-1a 32-bit offset basis itself.
            Assert.Equal("811c9dc5", Signature.Fnv1a32Hex(new byte[0]));
            // Cross-check a known FNV-1a 32 value for "a" = 0xe40c292c.
            Assert.Equal("e40c292c", Signature.Fnv1a32Hex(System.Text.Encoding.UTF8.GetBytes("a")));
        }

        [Fact]
        public void DescriptorShapeMatchesSpec()
        {
            // Empty anchor still has the A: prefix line.
            Assert.Equal("A:\n0:screen", Signature.Descriptor(null, new Node("screen")));
            Assert.Equal("A:\n0:screen", Signature.Descriptor("", new Node("screen")));
            // Unknown role normalizes to node.
            Assert.Equal("A:\n0:node", Signature.Descriptor(null, new Node("carousel")));
            // Token field order: type, icon, id.
            var tf = new Node("textfield") { Type = "password", Icon = "lock", Id = "pwd" };
            Assert.Equal("A:\n0:textfield:password#lock@pwd", Signature.Descriptor(null, tf));

            // Repeated siblings collapse to one *-marked token, count dropped.
            Node MkList(int n)
            {
                var list = new Node("list");
                for (int i = 0; i < n; i++)
                {
                    var li = new Node("listitem");
                    li.Children.Add(new Node("text"));
                    list.Children.Add(li);
                }
                return list;
            }
            Assert.Equal("A:\n0:list;1:listitem*;2:text", Signature.Descriptor(null, MkList(3)));
            Assert.Equal(Signature.Descriptor(null, MkList(3)), Signature.Descriptor(null, MkList(5)));

            // Non-consecutive identical siblings are NOT collapsed.
            var g = new Node("group");
            g.Children.Add(new Node("button"));
            g.Children.Add(new Node("link"));
            g.Children.Add(new Node("button"));
            Assert.Equal("A:\n0:group;1:button;1:link;1:button", Signature.Descriptor(null, g));

            // Transient subtree dropped (spinner + its child).
            var withSpinner = new Node("screen");
            withSpinner.Children.Add(new Node("text"));
            var spinner = new Node("spinner");
            spinner.Children.Add(new Node("text"));
            withSpinner.Children.Add(spinner);
            var withoutSpinner = new Node("screen");
            withoutSpinner.Children.Add(new Node("text"));
            Assert.Equal(Signature.Descriptor(null, withoutSpinner), Signature.Descriptor(null, withSpinner));
        }

        [Fact]
        public void ValueStateDescriptorShapeMatchesSpec()
        {
            // A textfield WITHOUT a value -> no V: section (byte-identical to before).
            Assert.Equal("A:\n0:textfield@email",
                Signature.Descriptor(null, new Node("textfield") { Id = "email" }));
            // A chrome node WITH a value is still not value-bearing: no V: section.
            Assert.Equal("A:\n0:header@title",
                Signature.Descriptor(null, new Node("header") { Id = "title", Value = "Welcome" }));
            // A value-bearing textfield adds the V: section.
            Assert.Equal("A:\n0:textfield@email\nV:key:email=NONEMPTY",
                Signature.Descriptor(null, new Node("textfield") { Id = "email", Value = "a@b.com" }));
            // status is a value-role but not in ROLES, so the body token is `node`.
            Assert.Equal("A:\n0:node@count\nV:key:count=POS1",
                Signature.Descriptor(null, new Node("status") { Id = "count", Value = "5" }));

            // V: section is sorted by key (independent of document order).
            var screen = new Node("screen");
            screen.Children.Add(new Node("textfield") { Id = "zeta", Value = "0" });
            screen.Children.Add(new Node("textfield") { Id = "alpha", Value = "12" });
            Assert.Equal(
                "A:\n0:screen;1:textfield@zeta;1:textfield@alpha\nV:key:alpha=POS2;key:zeta=ZERO",
                Signature.Descriptor(null, screen));

            // Keyless value nodes collapse structurally but survive in V: by index.
            var keyless = new Node("screen");
            keyless.Children.Add(new Node("textfield") { Value = "3" });
            keyless.Children.Add(new Node("textfield") { Value = "99" });
            Assert.Equal(
                "A:\n0:screen;1:textfield*\nV:role:textfield#0=POS1;role:textfield#1=POS2",
                Signature.Descriptor(null, keyless));

            // Layer 3 opt-in: a chrome `text` role becomes value-bearing when flagged.
            Assert.Equal("A:\n0:text@display",
                Signature.Descriptor(null, new Node("text") { Id = "display", Value = "42" }));
            Assert.Equal("A:\n0:text@display\nV:key:display=POS2",
                Signature.Descriptor(null, new Node("text") { Id = "display", Value = "42", ValueNode = true }));

            // Transient value subtree is dropped from both body and V: section.
            var transientVal = new Node("screen");
            var box = new Node("group") { Transient = true };
            box.Children.Add(new Node("status") { Id = "loading", Value = "50" });
            transientVal.Children.Add(box);
            Assert.Equal("A:\n0:screen", Signature.Descriptor(null, transientVal));
        }
    }
}
