/**
 * Canonical structural screen signature for React Native.
 *
 * Byte-identical to the Rust oracle (`crates/reproit/src/model/signature.rs`),
 * the web SDK (`sdk/reproit-web.js`), and the runners (`runners/rn/runner.mjs`,
 * `runners/web/runner.mjs`). Spec: `docs/signature.md`. Proven against the golden
 * vectors in `signature_vectors.json` by `test/signature.test.ts`.
 *
 * A signature hashes STRUCTURE (roles + ids + types + icons + tree shape), never
 * localized text, so an EN and a JA render of the same screen hash identically.
 * The descriptor is:
 *     "A:" + anchor + "\n" + tokens.join(";")
 * where each retained node emits one pre-order token:
 *     <depth>:<role>[:<type>][#<icon>][@<id>]   (plus "*" when collapsed)
 * hashed with FNV-1a 32-bit -> 8 lowercase hex chars.
 */

/** A normalized accessibility node: the input to the signature. */
export interface Node {
  /** Role from the fixed vocabulary; unknown roles normalize to `node`. */
  role: string;
  /** Stable developer identifier (testID / nativeID / a11y-id / resource-id). */
  id?: string | null;
  /** Optional input-type refinement (text, password, email, number, ...). */
  type?: string | null;
  /** Optional language-independent icon identity (codepoint / symbol / asset). */
  icon?: string | null;
  /** Explicit transient marker (e.g. a transient error banner). Dropped. */
  transient?: boolean;
  /**
   * The node's displayed data value (Layer 2, docs/signature.md "Value-state").
   * Only consulted when the node is value-bearing (a value-role, or value_node-
   * flagged). Chrome text NEVER goes here. Undefined/null by default, so a tree
   * with no values is byte-identical to a pre-value-state tree.
   */
  value?: string | null;
  /**
   * Opt-in value-node flag (Layer 3). When true, the node is treated as
   * value-bearing even if its role is not in the value-role set (a reproit.yaml
   * `value_nodes:` selector resolves to this flag). Defaults to false.
   */
  value_node?: boolean;
  /** Ordered children, in document order. */
  children?: Node[];
}

// Fixed, language-independent role vocabulary (docs/signature.md "Roles").
// Anything outside this set normalizes to "node".
const ROLES: Record<string, 1> = {
  screen: 1, header: 1, text: 1, button: 1, link: 1, textfield: 1, image: 1,
  icon: 1, list: 1, listitem: 1, tab: 1, switch: 1, checkbox: 1, radio: 1,
  slider: 1, menu: 1, menuitem: 1, dialog: 1, group: 1, node: 1,
};

// Roles that flicker in/out and are dropped before hashing (rule 2).
// "progress" is the role name for spinner/progress.
const TRANSIENT_ROLES: Record<string, 1> = {
  toast: 1, snackbar: 1, spinner: 1, progress: 1, tooltip: 1, badge: 1,
};

// Value-role set (docs/signature.md "Value-state", Layer 2). A node is
// value-bearing iff it has a `value` AND either its RAW role is one of these OR
// it carries the opt-in `value_node` flag (Layer 3). Several of these roles
// (status, log, progressbar, meter, timer, output) are NOT in the structural
// ROLES vocabulary, so they normalize to "node" in the token body; the
// value-role test deliberately uses the RAW role, not the normalized one. Chrome
// roles (button/label/header/text/link) are NEVER value-bearing, so the
// chrome-text exclusion (rule 1) is preserved exactly.
const VALUE_ROLES: Record<string, 1> = {
  textfield: 1, status: 1, log: 1, progressbar: 1, meter: 1, timer: 1, output: 1,
};

function normalizeRole(role: string): string {
  return ROLES[role] ? role : 'node';
}

function isTransientNode(node: Node): boolean {
  return !!node.transient || !!TRANSIENT_ROLES[node.role];
}

// True if this node carries a canonical value-class in the V: section
// (docs/signature.md "Value-state"): it has a `value` AND it is value-bearing,
// i.e. its RAW role is a value-role OR it is value_node-flagged. The raw role is
// used deliberately: roles like status/meter normalize to "node" but are still
// value-roles. Mirrors the oracle's is_value_bearing exactly.
function isValueBearing(node: Node): boolean {
  return node.value != null && (!!VALUE_ROLES[node.role] || !!node.value_node);
}

// The shared UTF-8 encoder for the canonical hash + V: byte-order sort. The
// descriptor and V: keys can carry non-ASCII (a localized route in the anchor, a
// non-ASCII developer id, an emoji icon), so we MUST fold the UTF-8 BYTES of the
// string, exactly like the Rust oracle's `desc.as_bytes()`. Hashing the UTF-16
// code units instead silently diverged on any non-ASCII descriptor.
const REPROIT_UTF8 = new TextEncoder();

/**
 * FNV-1a 32-bit over the UTF-8 BYTES of the descriptor -> 8 hex. Byte-for-byte
 * identical to the Rust oracle's fnv1a32_hex (offset basis 0x811c9dc5, prime
 * 0x01000193) over `descriptor.as_bytes()`.
 */
export function fnv1a32hex(s: string): string {
  const bytes = REPROIT_UTF8.encode(s);
  let h = 0x811c9dc5;
  for (let i = 0; i < bytes.length; i++) {
    h ^= bytes[i];
    h = Math.imul(h, 0x01000193) >>> 0;
  }
  return ('0000000' + (h >>> 0).toString(16)).slice(-8);
}

// Lexicographic comparison of two strings by their UTF-8 byte sequence, to match
// Rust's `String::cmp` (which compares bytes). JS `<` compares UTF-16 code units,
// which diverges from byte order for astral vs high-BMP keys, so the V: section
// MUST sort with this instead.
function reproitCmpUtf8(a: string, b: string): number {
  const ab = REPROIT_UTF8.encode(a);
  const bb = REPROIT_UTF8.encode(b);
  const n = Math.min(ab.length, bb.length);
  for (let i = 0; i < n; i++) {
    if (ab[i] !== bb[i]) return ab[i] < bb[i] ? -1 : 1;
  }
  return ab.length === bb.length ? 0 : ab.length < bb.length ? -1 : 1;
}

// ---- Layer 2: value-class identity (canonical, mirrors the Rust oracle) ----
// Strict ^[+-]?[0-9]+(\.[0-9]+)?$: optional sign, one or more ASCII digits,
// optionally a period and one or more ASCII digits. No grouping separators, no
// exponent, no leading/trailing dot. Locale-safe by construction. Mirrors the
// oracle's is_strict_decimal byte-for-byte.
function isStrictDecimal(s: string): boolean {
  let i = 0;
  const n = s.length;
  if (i < n && (s.charCodeAt(i) === 43 || s.charCodeAt(i) === 45)) i++; // + or -
  const intStart = i;
  while (i < n && s.charCodeAt(i) >= 48 && s.charCodeAt(i) <= 57) i++;
  if (i === intStart) return false; // need at least one integer digit
  if (i < n && s.charCodeAt(i) === 46) {
    i++;
    const fracStart = i;
    while (i < n && s.charCodeAt(i) >= 48 && s.charCodeAt(i) <= 57) i++;
    if (i === fracStart) return false; // trailing dot with no fraction
  }
  return i === n;
}

/**
 * Map a value string to a bounded, deterministic, locale-safe value-class token
 * (docs/signature.md "Value-state"). Identical rule to the Rust oracle's
 * value_class: EMPTY / strict-decimal -> ZERO|NEG|POS1|POS2|POS3|POSL / else
 * NONEMPTY. Anything ambiguously formatted (grouped/locale numbers, currency,
 * exponent, non-ASCII digits) falls to NONEMPTY because we do not guess locale.
 */
export function valueClass(s: string | null | undefined): string {
  const t = (s == null ? '' : String(s)).replace(/^\s+|\s+$/g, '');
  if (t.length === 0) return 'EMPTY';
  if (isStrictDecimal(t)) {
    const num = parseFloat(t);
    const a = Math.abs(num);
    if (num === 0) return 'ZERO';
    if (num < 0) return 'NEG';
    if (a < 10) return 'POS1';
    if (a < 100) return 'POS2';
    if (a < 1000) return 'POS3';
    return 'POSL';
  }
  return 'NONEMPTY';
}

// The V:-section key for a value-bearing node: its stable `id` rendered as
// key:<id> if present, else the structural fallback role:<role>#<idx> using the
// NORMALIZED role and the per-parent structural index among same-role
// non-transient siblings (matching the selector grammar). Mirrors value_key.
function valueKeyOf(node: Node, structuralIndex: number): string {
  if (node.id != null) return 'key:' + node.id;
  return 'role:' + normalizeRole(node.role) + '#' + structuralIndex;
}

// Collect (value_key, value_class) pairs for every value-bearing node in the
// tree, pre-order, skipping transient subtrees (rule 2) so the V: section is
// consistent with the structural body. The root gets index 0 (no peers); each
// keyless child gets its position among same-normalized-role non-transient
// siblings under the same parent. Mirrors collect_values + collect_values_children.
function collectValues(node: Node, out: Array<[string, string]>): void {
  if (isTransientNode(node)) return;
  if (isValueBearing(node)) out.push([valueKeyOf(node, 0), valueClass(node.value)]);
  collectValuesChildren(node, out);
}
function collectValuesChildren(node: Node, out: Array<[string, string]>): void {
  const roleCounts: Record<string, number> = {};
  const children = node.children || [];
  for (const child of children) {
    if (isTransientNode(child)) continue;
    const role = normalizeRole(child.role);
    const idx = roleCounts[role] || 0;
    roleCounts[role] = idx + 1;
    if (isValueBearing(child)) out.push([valueKeyOf(child, idx), valueClass(child.value)]);
    collectValuesChildren(child, out);
  }
}

// Build the V: section suffix. Returns "" when there are NO value-bearing nodes,
// which keeps the descriptor (and hash) byte-identical to a pre-value-state tree.
// Otherwise returns "\nV:" + sorted key=class entries joined by ";".
function valueSection(root: Node): string {
  const pairs: Array<[string, string]> = [];
  collectValues(root, pairs);
  if (pairs.length === 0) return '';
  pairs.sort((a, b) => reproitCmpUtf8(a[0], b[0]));
  const body = pairs.map((p) => p[0] + '=' + p[1]).join(';');
  return '\nV:' + body;
}

/** A node after rules 1, 2, 4 (transients dropped, document order kept). */
interface NormNode {
  role: string;
  type: string | null;
  icon: string | null;
  id: string | null;
  children: NormNode[];
}

// Rules 1, 2, 4: exclude text (no text field exists), drop transient subtrees,
// keep document order. Returns null if the node itself is transient.
function normalizeNode(node: Node): NormNode | null {
  if (isTransientNode(node)) return null;
  const kids: NormNode[] = [];
  const children = node.children || [];
  for (const c of children) {
    const n = normalizeNode(c);
    if (n) kids.push(n);
  }
  return {
    role: normalizeRole(node.role),
    type: node.type != null ? node.type : null,
    icon: node.icon != null ? node.icon : null,
    id: node.id != null ? node.id : null,
    children: kids,
  };
}

// Token body after "<depth>:", without the repeat marker:
//   <role>[:<type>][#<icon>][@<id>]
function tokenBody(n: NormNode): string {
  let s = n.role;
  if (n.type != null) s += ':' + n.type;
  if (n.icon != null) s += '#' + n.icon;
  if (n.id != null) s += '@' + n.id;
  return s;
}

// Subtree key for collapse comparison (rule 3): pre-order token list with depth
// re-based to 0 so sibling subtrees compare regardless of absolute depth.
function subtreeKey(n: NormNode): string {
  const tokens: string[] = [];
  const walk = (node: NormNode, depth: number): void => {
    tokens.push(depth + ':' + tokenBody(node));
    for (const c of node.children) walk(c, depth + 1);
  };
  walk(n, 0);
  return tokens.join(';');
}

function serializeNode(n: NormNode, depth: number, repeated: boolean, tokens: string[]): void {
  let tok = depth + ':' + tokenBody(n);
  if (repeated) tok += '*';
  tokens.push(tok);
  serializeChildren(n.children, depth + 1, tokens);
}

// Collapse maximal runs of >= 2 consecutive identical sibling subtrees into a
// single "*"-marked emission (count dropped).
function serializeChildren(children: NormNode[], depth: number, tokens: string[]): void {
  let i = 0;
  while (i < children.length) {
    const key = subtreeKey(children[i]);
    let j = i + 1;
    while (j < children.length && subtreeKey(children[j]) === key) j++;
    serializeNode(children[i], depth, j - i >= 2, tokens);
    i = j;
  }
}

/**
 * The exact UTF-8 descriptor string that gets hashed (docs/signature.md
 * "Descriptor serialization"): `"A:" + anchor + "\n" + tokens.join(";")`. The
 * `A:` prefix line is always present, even when there is no anchor.
 */
export function descriptorOf(anchor: string | null | undefined, root: Node): string {
  const tokens: string[] = [];
  const norm = normalizeNode(root);
  if (norm) serializeNode(norm, 0, false, tokens);
  // The V: section (Layer 2 value-classes) is appended only when at least one
  // value-bearing node exists; otherwise valueSection returns "" and the
  // descriptor stays purely structural.
  return 'A:' + (anchor == null ? '' : anchor) + '\n' + tokens.join(';') + valueSection(root);
}

/**
 * Canonical structural signature: FNV-1a over the descriptor of the canonical
 * Node tree, anchored on the route. The single source of cross-platform parity.
 */
export function signatureOf(anchor: string | null | undefined, root: Node): string {
  return fnv1a32hex(descriptorOf(anchor, root));
}
