// Canonical structural screen signature for .NET (WPF / WinUI 3).
//
// This is the C# port of the Rust parity oracle
// (crates/reproit/src/model/signature.rs). docs/signature.md is the spec;
// signature_vectors.json (repo root) holds the golden vectors every
// implementation must reproduce bit-for-bit. The production SDK (the WPF / WinUI
// capture in ReproIt.Windows) and the host parity test compute the signature
// through THIS file, so they agree by construction.
//
// The descriptor string that gets hashed is built exactly as the spec defines:
//
//   token = "<depth>:<role>[:<type>][#<icon>][@<id>]" (trailing "*" on a repeat)
//   body  = tokens joined by ";", pre-order
//   desc  = "A:" + anchor + "\n" + body  (+ a trailing "\nV:" value section)
//   sig   = FNV-1a 32-bit over the UTF-8 bytes of desc, 8-char lowercase hex
//
// This file targets netstandard2.0 / net8.0 and has NO Windows (WPF / WinUI)
// dependency on purpose: it is plain C# so the parity test runs cross-platform
// on any host with the .NET SDK, exactly like the Kotlin/Swift signature cores
// run on the host JVM / Foundation without the platform SDK.

using System;
using System.Collections.Generic;
using System.Globalization;
using System.Text;

namespace ReproIt.Core
{
    /// <summary>
    /// A normalized accessibility node: the input to the signature.
    ///
    /// Mirrors the Rust <c>Node</c> JSON shape so each golden vector's <c>tree</c>
    /// parses directly:
    /// <code>
    /// { "role": "button", "id": "submit", "type": "text",
    ///   "icon": "e5cd", "transient": false, "value": "3",
    ///   "value_node": false, "children": [ ... ] }
    /// </code>
    /// All fields except <c>role</c> and <c>children</c> are optional. There is
    /// deliberately no text/label field: localized text is excluded from the
    /// descriptor by construction (rule 1), so there is nothing to hash. The
    /// <c>value</c> field is the displayed data value (Layer 2) and is consulted
    /// ONLY when the node is value-bearing (a value-role or value_node-flagged);
    /// chrome text never goes here, so a value-less tree is byte-identical to a
    /// pre-value-state tree.
    /// </summary>
    public sealed class Node
    {
        /// <summary>Role from the fixed vocabulary; unknown roles normalize to "node".</summary>
        public string Role { get; set; }

        /// <summary>Stable developer id (AutomationId / x:Name / a11y-id). Null if none.</summary>
        public string Id { get; set; }

        /// <summary>Optional input-type refinement (text, password, email, number, ...).</summary>
        public string Type { get; set; }

        /// <summary>Optional language-independent icon identity (codepoint / symbol / asset).</summary>
        public string Icon { get; set; }

        /// <summary>Explicit transient marker (e.g. a transient error banner). Dropped
        /// like a transient role, together with its whole subtree.</summary>
        public bool Transient { get; set; }

        /// <summary>The node's displayed data value (Layer 2, docs/signature.md
        /// "Value-state"). Only consulted when the node is value-bearing. Null by
        /// default.</summary>
        public string Value { get; set; }

        /// <summary>Opt-in value-node flag (Layer 3). When true the node is treated
        /// as value-bearing even if its role is not in the value-role set.</summary>
        public bool ValueNode { get; set; }

        /// <summary>Ordered children, in document order.</summary>
        public List<Node> Children { get; set; }

        public Node(string role)
        {
            Role = role;
            Children = new List<Node>();
        }

        public Node() : this("node")
        {
        }
    }

    /// <summary>
    /// The canonical structural-signature core. Static, pure, allocation-light.
    /// Every method here mirrors the Rust oracle line-for-line so the descriptor
    /// it builds is byte-identical across all SDKs and runners.
    /// </summary>
    public static class Signature
    {
        /// <summary>The fixed, language-independent role vocabulary (docs/signature.md
        /// "Roles"). Anything outside this set normalizes to "node".</summary>
        public static readonly HashSet<string> Roles = new HashSet<string>(StringComparer.Ordinal)
        {
            "screen", "header", "text", "button", "link", "textfield", "image",
            "icon", "list", "listitem", "tab", "switch", "checkbox", "radio",
            "slider", "menu", "menuitem", "dialog", "group", "node",
        };

        /// <summary>Roles that flicker in and out of the tree and must be dropped
        /// before hashing (docs/signature.md normalization rule 2). "transient error
        /// banner" is not a distinct role in the vocabulary, so it is expressed via
        /// the <see cref="Node.Transient"/> flag; both paths drop the node and its
        /// whole subtree. "progress" is the role name for spinner/progress.</summary>
        public static readonly HashSet<string> TransientRoles = new HashSet<string>(StringComparer.Ordinal)
        {
            "toast", "snackbar", "spinner", "progress", "tooltip", "badge",
        };

        /// <summary>Value-role set (docs/signature.md "Value-state", Layer 2). A node
        /// carries a canonical value-class in the "V:" section only if it has a
        /// <see cref="Node.Value"/> AND either its RAW role is in this set OR it is
        /// flagged <see cref="Node.ValueNode"/> (the Layer 3 opt-in). Several of these
        /// (status, log, progressbar, meter, timer, output) are NOT in the structural
        /// <see cref="Roles"/> vocabulary, so they normalize to "node" in the
        /// descriptor body; the value-role test therefore uses the RAW role, not the
        /// normalized one. Chrome roles (button/header/text/link) are NEVER
        /// value-bearing, so rule 1's chrome-text exclusion is preserved exactly.</summary>
        public static readonly HashSet<string> ValueRoles = new HashSet<string>(StringComparer.Ordinal)
        {
            "textfield", "status", "log", "progressbar", "meter", "timer", "output",
        };

        /// <summary>Normalize a role to the fixed vocabulary: known roles pass
        /// through, unknown roles map to "node" (docs/signature.md "Roles").</summary>
        public static string NormalizeRole(string role)
        {
            return Roles.Contains(role) ? role : "node";
        }

        private static bool IsTransient(Node n)
        {
            return n.Transient || TransientRoles.Contains(n.Role);
        }

        // ---- Layer 2: bounded, locale-safe value-classes ------------------------
        // (docs/signature.md "Value-state").

        /// <summary>True if <paramref name="n"/> carries a canonical value-class in
        /// the "V:" section: it has a <see cref="Node.Value"/> AND it is value-bearing,
        /// i.e. its RAW role is a value-role OR it is value_node-flagged. The raw role
        /// is used deliberately (roles like "status"/"meter" normalize to "node" but
        /// are still value-roles). Mirrors the oracle's is_value_bearing exactly.</summary>
        private static bool IsValueBearing(Node n)
        {
            return n.Value != null && (ValueRoles.Contains(n.Role) || n.ValueNode);
        }

        /// <summary>Strict <c>^[+-]?[0-9]+(\.[0-9]+)?$</c>: an optional sign, one or
        /// more ASCII digits, optionally a period followed by one or more ASCII digits.
        /// No grouping separators, no exponent, no leading/trailing dot. Locale-safe by
        /// construction. Mirrors the oracle's is_strict_decimal byte-for-byte (it scans
        /// raw chars, never CultureInfo-aware parsing).</summary>
        private static bool IsStrictDecimal(string s)
        {
            int i = 0;
            int n = s.Length;
            if (i < n && (s[i] == '+' || s[i] == '-'))
            {
                i++;
            }
            int intStart = i;
            while (i < n && s[i] >= '0' && s[i] <= '9')
            {
                i++;
            }
            if (i == intStart)
            {
                return false; // need at least one integer digit
            }
            if (i < n && s[i] == '.')
            {
                i++;
                int fracStart = i;
                while (i < n && s[i] >= '0' && s[i] <= '9')
                {
                    i++;
                }
                if (i == fracStart)
                {
                    return false; // a trailing dot with no fraction digits is not allowed
                }
            }
            return i == n;
        }

        /// <summary>Map a value string to a bounded, deterministic, locale-safe
        /// value-class token (docs/signature.md "Value-state"): EMPTY / ZERO / NEG /
        /// POS1 (&lt;10) / POS2 (&lt;100) / POS3 (&lt;1000) / POSL (&gt;=1000) for the
        /// strict period-decimal grammar; NONEMPTY for anything ambiguous
        /// (grouped/locale numbers, currency, exponent, non-ASCII digits, text)
        /// because we do not guess locale formats. Identical buckets to the oracle.</summary>
        public static string ValueClass(string s)
        {
            string t = (s ?? string.Empty).Trim();
            if (t.Length == 0)
            {
                return "EMPTY";
            }
            if (IsStrictDecimal(t))
            {
                // The grammar is a strict subset of InvariantCulture's accepted
                // double syntax, so this parse never fails; InvariantCulture pins the
                // period as the decimal separator regardless of the host locale.
                double num = double.Parse(t, NumberStyles.AllowLeadingSign | NumberStyles.AllowDecimalPoint, CultureInfo.InvariantCulture);
                double a = Math.Abs(num);
                if (num == 0.0)
                {
                    return "ZERO";
                }
                if (num < 0.0)
                {
                    return "NEG";
                }
                if (a < 10.0)
                {
                    return "POS1";
                }
                if (a < 100.0)
                {
                    return "POS2";
                }
                if (a < 1000.0)
                {
                    return "POS3";
                }
                return "POSL";
            }
            return "NONEMPTY";
        }

        /// <summary>The "V:"-section key for a value-bearing node: its stable id as
        /// "key:&lt;id&gt;" if present, otherwise the structural fallback
        /// "role:&lt;role&gt;#&lt;idx&gt;" using the NORMALIZED role (so the key
        /// namespace matches the selector grammar). This is the "stable-key" the "V:"
        /// section sorts on.</summary>
        private static string ValueKey(Node n, int structuralIndex)
        {
            if (n.Id != null)
            {
                return "key:" + n.Id;
            }
            return "role:" + NormalizeRole(n.Role) + "#" + structuralIndex.ToString(CultureInfo.InvariantCulture);
        }

        /// <summary>Collect (value_key, value_class) pairs for every value-bearing
        /// node in the tree, in pre-order, skipping transient subtrees (rule 2) so the
        /// "V:" section stays consistent with the structural body. The root has no
        /// peers, so it gets index 0; each keyless child gets its position among
        /// same-(normalized-)role, non-transient siblings under the same parent. The
        /// result is later sorted by key. Mirrors collect_values + collect_values_children.</summary>
        private static void CollectValues(Node node, List<KeyValuePair<string, string>> outPairs)
        {
            if (IsTransient(node))
            {
                return;
            }
            if (IsValueBearing(node))
            {
                outPairs.Add(new KeyValuePair<string, string>(ValueKey(node, 0), ValueClass(node.Value)));
            }
            CollectValuesChildren(node, outPairs);
        }

        private static void CollectValuesChildren(Node node, List<KeyValuePair<string, string>> outPairs)
        {
            var roleCounts = new Dictionary<string, int>(StringComparer.Ordinal);
            var children = node.Children ?? EmptyChildren;
            foreach (var child in children)
            {
                if (IsTransient(child))
                {
                    continue;
                }
                string role = NormalizeRole(child.Role);
                int idx;
                roleCounts.TryGetValue(role, out idx);
                roleCounts[role] = idx + 1;
                if (IsValueBearing(child))
                {
                    outPairs.Add(new KeyValuePair<string, string>(ValueKey(child, idx), ValueClass(child.Value)));
                }
                CollectValuesChildren(child, outPairs);
            }
        }

        /// <summary>The "V:" section suffix (docs/signature.md "Value-state"). Empty
        /// string when there are NO value-bearing nodes, which keeps the descriptor
        /// (and hash) byte-identical to a pre-value-state tree (backward-compatible).
        /// Otherwise returns "\nV:" + sorted key=class entries joined by ";".</summary>
        private static string ValueSection(Node root)
        {
            var pairs = new List<KeyValuePair<string, string>>();
            CollectValues(root, pairs);
            if (pairs.Count == 0)
            {
                return string.Empty;
            }
            // Sort by the UTF-8 BYTE sequence of the key, matching the Rust
            // oracle's String::cmp (== Unicode code-POINT order). Note: string
            // .CompareOrdinal is UTF-16 code-UNIT order, which DIVERGES for astral
            // chars (surrogate pairs 0xD800-0xDBFF sort below high-BMP chars under
            // UTF-16, but above them under UTF-8/code-point order).
            pairs.Sort((a, b) => CompareUtf8(a.Key, b.Key));
            var sb = new StringBuilder();
            sb.Append("\nV:");
            for (int i = 0; i < pairs.Count; i++)
            {
                if (i > 0)
                {
                    sb.Append(';');
                }
                sb.Append(pairs[i].Key);
                sb.Append('=');
                sb.Append(pairs[i].Value);
            }
            return sb.ToString();
        }

        /// <summary>Compare two strings by their UTF-8 byte sequences, matching the
        /// Rust oracle's <c>String::cmp</c> (== Unicode code-POINT order). This is
        /// NOT the same as <c>string.CompareOrdinal</c> (UTF-16 code-UNIT order),
        /// which diverges for astral chars (surrogate pairs sort below high-BMP
        /// chars under UTF-16, but above them under UTF-8/code-point order).</summary>
        private static int CompareUtf8(string a, string b)
        {
            byte[] ba = Encoding.UTF8.GetBytes(a);
            byte[] bb = Encoding.UTF8.GetBytes(b);
            int n = ba.Length < bb.Length ? ba.Length : bb.Length;
            for (int i = 0; i < n; i++)
            {
                if (ba[i] != bb[i])
                {
                    return ba[i] - bb[i];
                }
            }
            return ba.Length - bb.Length;
        }

        // ---- structural body (rules 1-4) ----------------------------------------

        /// <summary>A normalized node after rules 1, 2, 4 are applied (transients
        /// removed, children normalized in order). Rule 3 (collapse) is applied at
        /// serialization time over the children of this tree.</summary>
        private sealed class NormNode
        {
            public string Role;
            public string Type;
            public string Icon;
            public string Id;
            public List<NormNode> Children;
        }

        private static readonly List<Node> EmptyChildren = new List<Node>();

        /// <summary>Apply rules 1, 2, 4: exclude text (no text field exists), drop
        /// transient subtrees, keep document order. Returns null if this node itself
        /// is transient (caller drops it).</summary>
        private static NormNode Normalize(Node node)
        {
            if (IsTransient(node))
            {
                return null;
            }
            var children = new List<NormNode>();
            var src = node.Children ?? EmptyChildren;
            foreach (var c in src)
            {
                var nc = Normalize(c);
                if (nc != null)
                {
                    children.Add(nc);
                }
            }
            return new NormNode
            {
                Role = NormalizeRole(node.Role),
                Type = node.Type,
                Icon = node.Icon,
                Id = node.Id,
                Children = children,
            };
        }

        /// <summary>One node's token body (everything after "&lt;depth&gt;:"), without
        /// the repeat marker: "&lt;role&gt;[:&lt;type&gt;][#&lt;icon&gt;][@&lt;id&gt;]".</summary>
        private static string TokenBody(NormNode n)
        {
            var sb = new StringBuilder(n.Role);
            if (n.Type != null)
            {
                sb.Append(':');
                sb.Append(n.Type);
            }
            if (n.Icon != null)
            {
                sb.Append('#');
                sb.Append(n.Icon);
            }
            if (n.Id != null)
            {
                sb.Append('@');
                sb.Append(n.Id);
            }
            return sb.ToString();
        }

        /// <summary>The canonical subtree descriptor used for collapse comparison
        /// (rule 3): the pre-order token list of this subtree, depths re-based to 0, so
        /// two sibling subtrees at the same level compare equal regardless of absolute
        /// depth.</summary>
        private static string SubtreeKey(NormNode n)
        {
            var tokens = new List<string>();
            WalkKey(n, 0, tokens);
            return string.Join(";", tokens);
        }

        private static void WalkKey(NormNode n, int depth, List<string> tokens)
        {
            tokens.Add(depth.ToString(CultureInfo.InvariantCulture) + ":" + TokenBody(n));
            foreach (var c in n.Children)
            {
                WalkKey(c, depth + 1, tokens);
            }
        }

        /// <summary>Emit one node's token (optionally marked repeated) then recurse,
        /// collapsing across the children run.</summary>
        private static void SerializeNode(NormNode n, int depth, bool repeated, List<string> tokens)
        {
            string tok = depth.ToString(CultureInfo.InvariantCulture) + ":" + TokenBody(n);
            if (repeated)
            {
                tok += "*";
            }
            tokens.Add(tok);
            SerializeChildren(n.Children, depth + 1, tokens);
        }

        /// <summary>Walk a run of siblings, collapsing maximal runs of &gt;= 2
        /// consecutive children whose SubtreeKey is identical into a single emission
        /// with the "*" marker (count dropped).</summary>
        private static void SerializeChildren(List<NormNode> children, int depth, List<string> tokens)
        {
            int i = 0;
            while (i < children.Count)
            {
                string key = SubtreeKey(children[i]);
                int j = i + 1;
                while (j < children.Count && SubtreeKey(children[j]) == key)
                {
                    j++;
                }
                int run = j - i;
                SerializeNode(children[i], depth, run >= 2, tokens);
                i = j;
            }
        }

        /// <summary>Build the exact UTF-8 descriptor string that gets hashed
        /// (docs/signature.md "Descriptor serialization"):
        /// "A:" + anchor + "\n" + tokens.join(";"), with the Layer 2 "V:" section
        /// appended only when at least one value-bearing node exists. The "A:" prefix
        /// line is always present, even with no anchor (then it is the empty string
        /// "A:" + newline). A tree with no value-bearing nodes is byte-identical to a
        /// pre-value-state tree.</summary>
        public static string Descriptor(string anchor, Node root)
        {
            var tokens = new List<string>();
            var norm = Normalize(root);
            if (norm != null)
            {
                SerializeNode(norm, 0, false, tokens);
            }
            return "A:" + (anchor ?? string.Empty) + "\n" + string.Join(";", tokens) + ValueSection(root);
        }

        /// <summary>THE canonical structural signature: FNV-1a 32-bit over the UTF-8
        /// bytes of <see cref="Descriptor"/>, 8-char zero-padded lowercase hex
        /// (docs/signature.md "Hash").</summary>
        public static string Of(string anchor, Node root)
        {
            string desc = Descriptor(anchor, root);
            return Fnv1a32Hex(Encoding.UTF8.GetBytes(desc));
        }

        /// <summary>FNV-1a, 32-bit, over <paramref name="bytes"/>; 8-char zero-padded
        /// lowercase hex (docs/signature.md "Hash").
        ///
        /// C# <c>uint</c> is unsigned and wraps on overflow by definition, which is
        /// exactly the 32-bit modular arithmetic FNV-1a needs; we do not even need an
        /// <c>unchecked</c> block for an unsigned multiply. Operating over UTF-8 BYTES
        /// (not chars) keeps non-ASCII descriptors byte-identical to the Rust oracle;
        /// ASCII descriptors (the common case) are unaffected.</summary>
        public static string Fnv1a32Hex(byte[] bytes)
        {
            uint h = 0x811c9dc5u; // FNV-1a 32-bit offset basis
            foreach (byte b in bytes)
            {
                h ^= b;
                h *= 0x01000193u; // FNV prime; unsigned multiply wraps mod 2^32
            }
            return h.ToString("x8", CultureInfo.InvariantCulture);
        }

        // ---- Selectors (docs/signature.md "Selectors") --------------------------

        /// <summary>A selector that addresses an element for actions / repros:
        /// id &gt; type+role &gt; role + structural-index. Returns "key:&lt;id&gt;" if
        /// the node has a stable id; otherwise the structural "role:&lt;role&gt;#&lt;idx&gt;"
        /// form. <see cref="NoKey"/> is true whenever no stable id was available
        /// (metadata for "map show"; it does NOT affect the hash).</summary>
        public struct SelectorResult
        {
            public string Selector;
            public bool NoKey;
        }

        /// <summary>Build a selector for a node given its structural index among peers.</summary>
        public static SelectorResult Selector(Node node, int structuralIndex)
        {
            if (node.Id != null)
            {
                return new SelectorResult { Selector = "key:" + node.Id, NoKey = false };
            }
            return new SelectorResult
            {
                Selector = "role:" + NormalizeRole(node.Role) + "#" + structuralIndex.ToString(CultureInfo.InvariantCulture),
                NoKey = true,
            };
        }
    }
}
