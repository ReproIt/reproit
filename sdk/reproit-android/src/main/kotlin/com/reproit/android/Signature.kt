package com.reproit.android

/**
 * Canonical structural screen signature for Android.
 *
 * This is the Kotlin port of the Rust parity oracle
 * (`crates/reproit/src/model/signature.rs`). `docs/signature.md` is the spec;
 * `signature_vectors.json` (repo root) holds the golden vectors every
 * implementation must reproduce bit-for-bit. The production SDK ([ReproIt],
 * via [Engine]) computes the signature through THIS file, so it agrees with the
 * runners and the other SDKs by construction.
 *
 * A signature hashes STRUCTURE (roles + ids + types + icons + tree shape), never
 * localized text, so an EN and a DE render of the same screen hash identically.
 * The descriptor string that gets hashed is built exactly as the spec defines:
 *
 *   token = `<depth>:<role>[:<type>][#<icon>][@<id>]` (trailing `*` on a repeat)
 *   body  = tokens joined by `;`, pre-order
 *   desc  = `"A:" + anchor + "\n" + body`
 *   sig   = FNV-1a 32-bit over the UTF-8 bytes of desc, 8-char lowercase hex
 *
 * This file has NO `android.*` imports on purpose: it is pure Kotlin so the
 * parity test runs on the host JVM without the Android SDK.
 */
object Signature {

    /** The fixed, language-independent role vocabulary (docs/signature.md
     * "Roles"). Anything outside this set normalizes to `node`. */
    val ROLES: Set<String> = setOf(
        "screen", "header", "text", "button", "link", "textfield", "image",
        "icon", "list", "listitem", "tab", "switch", "checkbox", "radio",
        "slider", "menu", "menuitem", "dialog", "group", "node",
    )

    /** Roles that flicker in and out of the tree and must be dropped before
     * hashing (docs/signature.md normalization rule 2). "transient error banner"
     * is not a distinct role in the vocabulary, so it is expressed via the
     * [Node.transient] flag; both paths drop the node and its whole subtree.
     * `progress` is the role name for spinner/progress. */
    val TRANSIENT_ROLES: Set<String> = setOf(
        "toast", "snackbar", "spinner", "progress", "tooltip", "badge",
    )

    /** Value-role set (docs/signature.md "Value-state", Layer 2). A node carries
     * a canonical value-class in the `V:` section only if it has a [Node.value]
     * AND either its RAW role is in this set OR it is flagged [Node.valueNode]
     * (the Layer 3 opt-in). Several of these (`status, log, progressbar, meter,
     * timer, output`) are NOT in the structural [ROLES] vocabulary, so they
     * normalize to `node` in the descriptor body; the value-role test therefore
     * uses the RAW role, not the normalized one. Chrome roles
     * (button/label/header/text/...) are NEVER value-bearing, so rule 1's
     * chrome-text exclusion is preserved exactly. */
    val VALUE_ROLES: Set<String> = setOf(
        "textfield", "status", "log", "progressbar", "meter", "timer", "output",
    )

    /**
     * A normalized accessibility node: the input to the signature.
     *
     * Mirrors the Rust `Node` JSON shape so each golden vector's `tree` parses
     * directly via [Node.fromJson]:
     * ```json
     * { "role": "button", "id": "submit", "type": "text",
     *   "icon": "e5cd", "transient": false, "children": [ ... ] }
     * ```
     * All fields except `role`/`children` are optional. There is deliberately no
     * text/label/value field: localized text is excluded from the descriptor by
     * construction (rule 1), so there is nothing to hash.
     */
    data class Node(
        /** Role from the fixed vocabulary; unknown roles normalize to `node`. */
        val role: String,
        /** Stable developer id (resource-id / testTag / a11y-id). Omitted if none. */
        val id: String? = null,
        /** Optional input-type refinement (text, password, email, ...). */
        val type: String? = null,
        /** Optional language-independent icon identity (codepoint / asset name). */
        val icon: String? = null,
        /** Explicit transient marker (e.g. a transient error banner). Dropped
         * like a transient role. */
        val transient: Boolean = false,
        /** The node's displayed data value (Layer 2, docs/signature.md
         * "Value-state"). Only consulted when the node is value-bearing (a
         * value-role or a [valueNode]-flagged node). Chrome text never goes here.
         * Null by default, so a tree with no values is byte-identical to a
         * pre-value-state tree. */
        val value: String? = null,
        /** Opt-in value-node flag (Layer 3). When true the node is treated as
         * value-bearing even if its role is not in [VALUE_ROLES] (a `reproit.yaml`
         * `value_nodes:` selector resolves to this flag). False by default. */
        val valueNode: Boolean = false,
        /** Ordered children, in document order. */
        val children: List<Node> = emptyList(),
    )

    /** Normalize a role to the fixed vocabulary: known roles pass through,
     * unknown roles map to `node` (docs/signature.md "Roles"). */
    fun normalizeRole(role: String): String = if (ROLES.contains(role)) role else "node"

    private fun isTransient(n: Node): Boolean =
        n.transient || TRANSIENT_ROLES.contains(n.role)

    /** A normalized node after rules 1, 2, 4 are applied (transients removed,
     * children normalized in order). Rule 3 (collapse) is applied at
     * serialization time over the children of this tree. */
    private class NormNode(
        val role: String,
        val type: String?,
        val icon: String?,
        val id: String?,
        val children: List<NormNode>,
    )

    /** Apply rules 1, 2, 4: exclude text (no text field exists), drop transient
     * subtrees, keep document order. Returns null if this node itself is
     * transient (caller drops it). */
    private fun normalize(node: Node): NormNode? {
        if (isTransient(node)) return null
        val children = ArrayList<NormNode>()
        for (c in node.children) {
            val nc = normalize(c)
            if (nc != null) children.add(nc)
        }
        return NormNode(
            normalizeRole(node.role),
            node.type,
            node.icon,
            node.id,
            children,
        )
    }

    /** One node's token body (everything after `<depth>:`), without the repeat
     * marker: `<role>[:<type>][#<icon>][@<id>]`. */
    private fun tokenBody(n: NormNode): String {
        val sb = StringBuilder(n.role)
        if (n.type != null) {
            sb.append(':')
            sb.append(n.type)
        }
        if (n.icon != null) {
            sb.append('#')
            sb.append(n.icon)
        }
        if (n.id != null) {
            sb.append('@')
            sb.append(n.id)
        }
        return sb.toString()
    }

    /** The canonical subtree descriptor used for collapse comparison (rule 3):
     * the pre-order token list of this subtree, depths re-based to 0, so two
     * sibling subtrees at the same level compare equal regardless of absolute
     * depth. */
    private fun subtreeKey(n: NormNode): String {
        val tokens = ArrayList<String>()
        walkKey(n, 0, tokens)
        return tokens.joinToString(";")
    }

    private fun walkKey(n: NormNode, depth: Int, tokens: MutableList<String>) {
        tokens.add("$depth:${tokenBody(n)}")
        for (c in n.children) walkKey(c, depth + 1, tokens)
    }

    /** Emit one node's token (optionally marked repeated) then recurse,
     * collapsing across the children run. */
    private fun serializeNode(
        n: NormNode,
        depth: Int,
        repeated: Boolean,
        tokens: MutableList<String>,
    ) {
        var tok = "$depth:${tokenBody(n)}"
        if (repeated) tok += "*"
        tokens.add(tok)
        serializeChildren(n.children, depth + 1, tokens)
    }

    /** Walk a run of siblings, collapsing maximal runs of >= 2 consecutive
     * children whose subtreeKey is identical into a single emission with the `*`
     * marker (count dropped). */
    private fun serializeChildren(
        children: List<NormNode>,
        depth: Int,
        tokens: MutableList<String>,
    ) {
        var i = 0
        while (i < children.size) {
            val key = subtreeKey(children[i])
            var j = i + 1
            while (j < children.size && subtreeKey(children[j]) == key) j++
            val run = j - i
            serializeNode(children[i], depth, run >= 2, tokens)
            i = j
        }
    }

    // ---- Layer 2: bounded, locale-safe value-classes ------------------------
    // (docs/signature.md "Value-state").

    /** True if [n] carries a canonical value-class in the `V:` section: it has a
     * [Node.value] AND it is value-bearing, i.e. its RAW role is a value-role OR
     * it is [Node.valueNode]-flagged. The raw role is used deliberately (roles
     * like `status`/`meter` normalize to `node` but are still value-roles). */
    private fun isValueBearing(n: Node): Boolean =
        n.value != null && (VALUE_ROLES.contains(n.role) || n.valueNode)

    /** Strict `^[+-]?[0-9]+(\.[0-9]+)?$`: an optional sign, one or more ASCII
     * digits, optionally a period followed by one or more ASCII digits. No
     * grouping separators, no exponent, no leading/trailing dot. Locale-safe by
     * construction. */
    private fun isStrictDecimal(s: String): Boolean {
        var i = 0
        val n = s.length
        if (i < n && (s[i] == '+' || s[i] == '-')) i++
        val intStart = i
        while (i < n && s[i] in '0'..'9') i++
        if (i == intStart) return false // need at least one integer digit
        if (i < n && s[i] == '.') {
            i++
            val fracStart = i
            while (i < n && s[i] in '0'..'9') i++
            if (i == fracStart) return false // trailing dot with no fraction
        }
        return i == n
    }

    /** Map a value string to a bounded, deterministic, locale-safe value-class
     * token (docs/signature.md "Value-state"). EMPTY / ZERO / NEG / POS1<10 /
     * POS2<100 / POS3<1000 / POSL>=1000 for the strict period-decimal grammar;
     * NONEMPTY for anything ambiguous (grouped/locale numbers, currency, text)
     * because we do not guess locale formats. */
    fun valueClass(s: String): String {
        val t = s.trim()
        if (t.isEmpty()) return "EMPTY"
        if (isStrictDecimal(t)) {
            val num = t.toDouble()
            val a = kotlin.math.abs(num)
            return when {
                num == 0.0 -> "ZERO"
                num < 0.0 -> "NEG"
                a < 10.0 -> "POS1"
                a < 100.0 -> "POS2"
                a < 1000.0 -> "POS3"
                else -> "POSL"
            }
        }
        return "NONEMPTY"
    }

    /** The `V:`-section key for a value-bearing node: its stable `id` as
     * `key:<id>` if present, otherwise the structural fallback `role:<role>#<idx>`
     * using the NORMALIZED role (so the key namespace matches the selector
     * grammar). This is the "stable-key" the `V:` section sorts on. */
    private fun valueKey(n: Node, structuralIndex: Int): String =
        if (n.id != null) "key:${n.id}"
        else "role:${normalizeRole(n.role)}#$structuralIndex"

    /** Collect `(value_key, value_class)` pairs for every value-bearing node in
     * the tree, in pre-order, skipping transient subtrees (rule 2) so the `V:`
     * section stays consistent with the structural body. The structural index for
     * a keyless node is its position among same-(normalized-)role, non-transient
     * siblings under the same parent. The root has no peers, so it gets index 0.
     * The result is later sorted by key for deterministic serialization. */
    private fun valuePairs(root: Node): List<Pair<String, String>> {
        val out = ArrayList<Pair<String, String>>()
        if (isTransient(root)) return out
        if (isValueBearing(root)) {
            out.add(valueKey(root, 0) to valueClass(root.value!!))
        }
        collectChildValues(root, out)
        out.sortBy { it.first }
        return out
    }

    private fun collectChildValues(node: Node, out: MutableList<Pair<String, String>>) {
        val roleCounts = HashMap<String, Int>()
        for (c in node.children) {
            if (isTransient(c)) continue
            val role = normalizeRole(c.role)
            val idx = roleCounts[role] ?: 0
            roleCounts[role] = idx + 1
            if (isValueBearing(c)) {
                out.add(valueKey(c, idx) to valueClass(c.value!!))
            }
            collectChildValues(c, out)
        }
    }

    /** The `V:` section suffix (docs/signature.md "Value-state"). Empty string
     * when there are NO value-bearing pairs, which keeps the descriptor (and
     * hash) byte-identical to a pre-value-state tree (backward-compatible). */
    private fun valueSection(root: Node): String {
        val pairs = valuePairs(root)
        if (pairs.isEmpty()) return ""
        return "\nV:" + pairs.joinToString(";") { "${it.first}=${it.second}" }
    }

    /** Build the exact UTF-8 descriptor string that gets hashed
     * (docs/signature.md "Descriptor serialization"):
     * `"A:" + anchor + "\n" + tokens.join(";")`, with the Layer 2 `V:` section
     * appended only when at least one value-bearing node exists. The `A:` prefix
     * line is always present, even with no anchor (then it is the empty string
     * `A:` + newline). A tree with no value-bearing nodes is byte-identical to a
     * pre-value-state tree. */
    fun descriptor(anchor: String?, root: Node): String {
        val tokens = ArrayList<String>()
        val norm = normalize(root)
        if (norm != null) serializeNode(norm, 0, false, tokens)
        return "A:${anchor ?: ""}\n${tokens.joinToString(";")}${valueSection(root)}"
    }

    /** THE canonical structural signature: FNV-1a 32-bit over [descriptor],
     * 8-char lowercase hex (docs/signature.md "Hash"). */
    fun of(anchor: String?, root: Node): String =
        fnv1a32Hex(descriptor(anchor, root).toByteArray(Charsets.UTF_8))

    /**
     * FNV-1a, 32-bit, over [bytes]; 8-char zero-padded lowercase hex.
     *
     * Kotlin `Int` is signed and overflows by wrapping (two's complement), which
     * is exactly the 32-bit modular arithmetic FNV-1a needs. We compute in `Int`
     * with wraparound, then reinterpret the bits as unsigned only for the final
     * hex formatting via `(h.toLong() and 0xFFFFFFFFL)`. Operating over UTF-8
     * BYTES (not chars) keeps non-ASCII descriptors byte-identical to the Rust
     * oracle; ASCII descriptors (the common case) are unaffected.
     */
    fun fnv1a32Hex(bytes: ByteArray): String {
        var h = 0x811c9dc5.toInt() // offset basis as a (negative) signed Int
        for (b in bytes) {
            h = h xor (b.toInt() and 0xFF)
            h *= 0x01000193 // wrapping multiply == 32-bit modular multiply
        }
        return (h.toLong() and 0xFFFFFFFFL).toString(16).padStart(8, '0')
    }

    // ---- vector JSON parsing (host-test only) -------------------------------
    // A tiny, dependency-free JSON reader sufficient to parse the golden vectors
    // in signature_vectors.json into [Node] trees. The production capture path
    // builds [Node]s directly from the view tree; this exists so the parity test
    // can read the canonical vectors on the host JVM without a JSON library
    // (plain `kotlinc` has no org.json).

    /** Build a [Node] from a parsed JSON map (the `tree` field of a vector). */
    fun nodeFromJson(j: Map<String, Any?>): Node {
        @Suppress("UNCHECKED_CAST")
        val kids = (j["children"] as? List<Any?>)?.map {
            @Suppress("UNCHECKED_CAST")
            nodeFromJson(it as Map<String, Any?>)
        } ?: emptyList()
        return Node(
            role = j["role"] as String,
            id = j["id"] as String?,
            type = j["type"] as String?,
            icon = j["icon"] as String?,
            transient = (j["transient"] as? Boolean) ?: false,
            value = j["value"] as String?,
            valueNode = (j["value_node"] as? Boolean) ?: false,
            children = kids,
        )
    }
}
