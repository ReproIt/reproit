# Screen signature: the canonical contract

A *signature* is the deterministic fingerprint that decides whether two screen
captures are the same node in the graph. It must be **identical** across every
component that computes it (the fuzz runners and the production SDKs), so a
production crash buckets to the same node a fuzz finding hits. This document is
the single source of truth; every implementation must pass the golden vectors in
`signature_vectors.json`.

## Why structural, over the same a11y tree

Hashing the sorted set of visible/accessible **names** (localized text) is
fragile: it changes with language, with copy edits, and with dynamic content (a
feed of N posts looks like N screens). Instead we hash **structure**, derived from
the **accessibility tree**, because the a11y tree is the one representation both
the in-app SDK and the external runner can read identically (roles + identifiers +
shape). Localized text is excluded from the hash entirely; it is kept only as a
display-only `labels` field for `map show`.

## Inputs

Each component walks its platform's accessibility tree and produces a normalized
node tree. Every node has:

- `role`: one of a fixed, language-independent vocabulary (see Roles).
- `id`: a stable developer identifier if present (key / test-id / a11y-id /
  resource-id). Omitted if none.
- `type`: optional refinement for inputs (`text`, `password`, `email`, `number`,
  `search`, `checkbox`, `radio`, `switch`, `slider`).
- `icon`: optional language-independent icon identity (Material codepoint, SF
  Symbol name, asset name).
- `children`: ordered child nodes.

A screen-level **anchor** is captured if available: the route/path, the
screen-level key, or an explicit developer annotation (`ReproItScreen("name")`).

## Roles (fixed vocabulary)

`screen, header, text, button, link, textfield, image, icon, list, listitem,
tab, switch, checkbox, radio, slider, menu, menuitem, dialog, group, node`

Derived from a11y roles/traits, never from the visible label. Unknown roles map
to `node`.

## Normalization (applied before hashing)

1. **Exclude localized text.** No accessible name / label / value text enters the
   descriptor.
2. **Drop transient nodes.** Roles/classes that flicker in and out are removed:
   toast, snackbar, spinner/progress, tooltip, transient error banner, badge.
   (A screen with an error showing hashes the same as without.)
3. **Collapse repeated siblings.** When >= 2 consecutive siblings have an
   identical child-descriptor, emit the subtree once with a `*` repeat marker and
   DROP the count. (A list of 3 vs 5 identical items hashes the same; the
   dynamic-content explosion is gone.)
4. **Stable order.** Children are kept in document order (already deterministic);
   `id`s within a node are not reordered.

## Descriptor serialization

Pre-order walk. For each retained node, emit one token:

```
<depth>:<role>[:<type>][#<icon>][@<id>]
```

Tokens are joined with `;`. The screen anchor, if present, is prefixed:

```
descriptor = "A:" + anchor + "\n" + tokens.join(";")
```

If no anchor, the `A:` line is the empty string `A:` followed by newline. Repeat
markers from rule 3 append `*` to the role: `2:listitem*`.

### Clarifications (reference implementation, normative)

These pin the simplest deterministic reading where the prose above is silent, so
every language matches the Rust oracle in `crates/reproit/src/model/signature.rs`:

- **Token field order is fixed**: `:type` then `#icon` then `@id`, each present
  only when set. The `*` repeat marker, when present, is appended to the whole
  token after the id: `1:textfield:password#lock@pwd*` (in practice repeats rarely
  carry an id, but the order is defined regardless).
- **Collapse comparison basis (rule 3)**: two consecutive siblings are "identical"
  iff their full subtree descriptors are byte-equal *after depth is re-based to 0
  at the sibling*. Compare the normalized subtree (transients already dropped),
  not the raw input. Collapse runs are maximal and only over *consecutive*
  siblings (a, b, a does not collapse). Nested collapse applies recursively: a
  collapsed subtree is still serialized with its own children collapsed.
- **`transient` flag**: "transient error banner" is not a role in the vocabulary,
  so an explicit boolean `transient` field on any node marks it (and its whole
  subtree) for dropping under rule 2, exactly like the transient roles. The
  transient roles are: `toast, snackbar, spinner, progress, tooltip, badge`
  (`progress` is the role name for spinner/progress).
- **Depth numbering**: the root node is depth 0; each level down adds 1; document
  (pre-order) traversal.
- **Empty / fully-transient tree**: if the root itself is transient (or there are
  no retained nodes), the token body is the empty string, so the descriptor is
  just `"A:" + anchor + "\n"`. This is deterministic, not an error.
- **`type+role` selector tier**: the addressable selector string is always
  `key:<id>` (id present) or `role:<role>#<idx>` (no id). The `type+role` tier in
  the precedence list describes how the *hash* discriminates (type is in the
  descriptor token); it does not introduce a third selector string form.

## Value-state (effect detection + bounded value-classes)

The structural descriptor excludes ALL text (rule 1). That is correct for chrome
(button labels, headers, copy), but it collapses **value-state apps** (a
calculator, a counter, a stopwatch) to a single node: every keypress changes only
displayed text, so the structure never moves and the whole app is one signature.
The fix is layered. Layer 2 is part of the canonical signature (the oracle in
`crates/reproit/src/model/signature.rs` implements it and the golden vectors pin
it). Layers 1 and 3 are documented here and supported in the `Node` model, but
Layer 1 is runner-local (it never enters the canonical key) and Layer 3 is config.

### Layer 1 - effect detection (runner-local, NOT in the canonical signature)

After an action, a runner decides whether the action did anything by comparing a
**content fingerprint** before and after:

```
content_fingerprint = structural_signature
                    + sorted list of (stable-key, trimmed-raw-text)
                      over text-bearing nodes
```

An action is **effective** iff the structural signature changed OR the content
fingerprint changed; otherwise it is a **no-op** (a dead key, a disabled button).
This fingerprint is **ephemeral and runner-local**. It carries raw localized text
and MUST NOT enter the canonical graph key (doing so would re-introduce the
language/copy churn that rule 1 removes). It is a per-step liveness check only;
runners will implement it. The canonical signature is unchanged by Layer 1.

### Layer 2 - bounded value-class identity (canonical; implemented in the oracle)

A bounded, locale-safe **value-class token** is folded into the canonical
signature for **value-role nodes only**. The structural body is untouched; the
value-classes are appended in a separate, deterministic `V:` section.

**Value-role set.** A node is **value-bearing** iff it has a `value` AND either
its role is in the value-role set OR it carries the opt-in `value_node` flag
(Layer 3). The value-role set is:

```
textfield, status, log, progressbar, meter, timer, output
```

Several of these (`status, log, progressbar, meter, timer, output`) are NOT in the
structural role vocabulary, so they normalize to `node` in the descriptor body;
the value-role test therefore uses the node's **raw** role, not the normalized
one. **Chrome roles are NEVER value-bearing**: `button`, `label`, `header`,
`text`, `link`, etc. carry no value-class even if a `value` field is present, so
the chrome-text exclusion (rule 1) is preserved exactly.

**`value_class(s)`** maps a value string to one bounded token. It is deterministic
and **locale-safe**: it does not guess grouped or locale number formats.

```
trim s
  s is empty / all whitespace            -> "EMPTY"
  s matches strict ^[+-]?[0-9]+(\.[0-9]+)?$   (period decimal, NO grouping)
      parse to a number n:
          n == 0                         -> "ZERO"
          n < 0                          -> "NEG"
          |n| < 10                       -> "POS1"
          |n| < 100                      -> "POS2"
          |n| < 1000                     -> "POS3"
          |n| >= 1000                    -> "POSL"
  otherwise (non-numeric OR ambiguously formatted) -> "NONEMPTY"
```

The numeric grammar is strict ASCII: an optional sign, one or more digits, an
optional period followed by one or more digits. No grouping separators, no
exponent (`1e3`), no leading/trailing dot (`.5`, `3.`), no non-ASCII digits, no
currency or percent. Anything outside it (including `1,234`, `1.234.567`, `$5`,
`5%`) falls to `NONEMPTY` because the value's locale is unknown and we refuse to
guess. `1,234` is therefore `NONEMPTY`, NOT `POSL`.

**`V:` serialization.** For every value-bearing node, emit `key=valueclass`. The
`key` is the **stable-key**: the node's `id` rendered as `key:<id>` if present,
otherwise the structural fallback `role:<role>#<idx>` (normalized role, with the
per-parent structural index among same-role non-transient siblings, matching the
selector grammar). The entries are **sorted by key** and joined with `;`, then
appended to the descriptor as a trailing line:

```
descriptor = "A:" + anchor + "\n" + tokens.join(";") + "\nV:" + v_entries
```

Transient subtrees (rule 2) are excluded from the `V:` section as well as the
body, so the two stay consistent.

**Backward compatibility (hard requirement).** The `V:` line is emitted ONLY when
at least one value-bearing node exists. A tree with NO value-bearing nodes
produces the EXACT SAME descriptor and hash as before this layer existed, so every
pre-existing golden vector still passes byte-for-byte. (Note one deliberate
interaction: two keyless, structurally identical value nodes still **collapse** to
one `*`-marked token in the body, because the value is not structural; their two
value-classes still appear separately in the `V:` section under distinct
`role:<role>#<idx>` keys.)

**Hard cap (runner-enforced).** At most **8 distinct value-class combinations per
structural node**. The oracle is stateless per call, so it always computes the
value-class for the given state; the cap is a **runner** bound: once a runner has
observed more than 8 distinct value-class variants of one structural node, it
falls back to **structural-only** for that node (drops it from the `V:` section)
so an adversarial value generator cannot explode the graph. The oracle's job is
the per-state value-class; the runner's job is to enforce the cap.

### Layer 3 - opt-in value selectors (config; supported in the Node model)

A `reproit.yaml` may carry a `value_nodes:` list of selectors that mark **extra**
nodes as value-bearing even when their role is not in the value-role set:

```yaml
value_nodes:
  - key:score
  - role:text#2
```

A runner resolves each selector and sets the `value_node` flag on the matching
`Node` before signing; the oracle then treats it as value-bearing through the same
path as a value-role node (same `value_class`, same `V:` entry). A future
extension may let `value_nodes` attach a custom bucket function per node; for now
the bounded `value_class` above is the only bucketer.

## Hash

FNV-1a, 32-bit, over the UTF-8 bytes of `descriptor`:

```
h = 0x811c9dc5
for each byte b: h = (h XOR b) * 0x01000193   (mod 2^32)
output = 8-char zero-padded lowercase hex
```

(A standard FNV-1a; the function is plain, the real work is the structural
descriptor it hashes.)

## Terminal and instrumented surfaces

The contract above assumes an **accessibility tree** parsed into the `Node` model:
roles, ids, icons, types, children. Most surfaces have one (native a11y, the DOM
a11y tree, the platform accessibility APIs). Some do not. A terminal UI is a grid
of character cells with no a11y tree to walk; an immediate-mode GUI (imgui, clay)
has no retained a11y tree either, but it *does* have its own per-frame widget tree.

For these surfaces the rule is: **derive the canonical descriptor from the
surface's own structural source, then hash it with the SAME FNV-1a 32-bit
primitive and 8-char hex output.** The descriptor *source* differs by necessity,
but the hash family is identical, so every signature, a11y or not, is one
comparable 8-hex value in the same namespace. This is intentional and fully
deterministic, not a fallback or an approximation.

- **imgui / clay (and any immediate-mode toolkit):** the structural source is the
  widget tree the app already builds each frame (window/child/group nesting,
  widget kinds, stable string ids). Normalize and serialize it in the spirit of
  the `Node` descriptor (structure and ids, never the drawn label text), then hash
  with FNV-1a. The widget tree stands in for the a11y tree.

- **TUI (terminal):** the structural source is the **screen layout skeleton**, the
  thing that survives translation. From the VT cell grid we keep what is stable
  across locales and carries the layout, and erase what is not:
  - box-drawing / block glyphs (U+2500..U+259F) -> one structural edge marker
    (`#`): borders and panel extents, with single/double/rounded edges unified;
  - digits -> one marker (`9`): "a number is here" positionally, value-agnostic
    (counters and clocks churn, so the value is dropped);
  - ASCII punctuation / symbols (`:`, `[`, `]`, `/`, `$`, ...) -> kept verbatim:
    these are the non-localized field markers and bracketed hotkeys that genuinely
    distinguish layouts;
  - spaces / newlines -> kept verbatim: they *are* the layout (gaps set field and
    column extents; newlines delimit rows);
  - any run of natural-language letters (any language, including CJK) -> one word
    placeholder (`W`): the localized *identity* of words is excluded, exactly as
    rule 1 excludes localized text from the a11y descriptor.

  Each maximal run of one class is collapsed to a length-prefixed token so the
  *extents* (a 20-wide border vs a 4-wide one; a long field vs a short one) remain
  structural while the per-glyph identity is already gone. The cursor cell
  (row, col) is appended, because which field/row is focused is structure, not
  text. That skeleton string is then fed through the **same FNV-1a 32-bit hash**
  with the **same 8-char hex output** as the canonical hash above. The same screen
  rendered in English and German produces the same skeleton and therefore the same
  signature.

### TUI value-state (the terminal analogue of Layers 1 and 2)

The layout skeleton maps every digit to `9` and every word to `W`, which is the
locale-invariant win for chrome but is exactly the **value-state problem** on a
terminal: a counter, a clock, or a calculator changes only displayed values, so
its skeleton never moves and the whole app collapses to one signature, and the
explorer stalls. A terminal has no accessibility tree and no value roles, so the
a11y Node value-class (the `value-role set`, the `V:` node section) does NOT
apply. The TUI backend reproduces the two value-state layers from the screen text
directly:

- **Effect detection (Layer 1 analogue, runner-local).** Alongside the skeleton
  signature the runner computes a **content fingerprint** = `FNV-1a(full raw
  screen text + cursor cell)`, hashing the actual rendered cells (digits and
  words verbatim), NOT the skeleton. An action is **effective** iff the skeleton
  signature changed OR the content fingerprint changed; otherwise it is a no-op.
  This catches value-only updates (a counter ticking `0 -> 1 -> 2`) whose
  skeleton is byte-identical, so the explorer's no-progress counter does not
  abandon a live value-state screen. The fingerprint is **ephemeral and
  runner-local**: it carries raw localized text and NEVER enters the canonical
  state set (`seen`), exactly as the a11y Layer-1 fingerprint must not enter the
  canonical graph key.

- **Numeric value-class (Layer 2 analogue, bounded, in the TUI signature).** So a
  counter yields a few distinct states rather than one, a bounded set of the
  screen's **numeric value-classes** is folded into the TUI signature. The runner
  extracts contiguous numeric tokens from the screen text and maps each through
  the **same `value_class` bucketer and the same strict period-decimal grammar**
  as the oracle (`EMPTY / ZERO / NEG / POS1 / POS2 / POS3 / POSL`, with the
  locale-safe `NONEMPTY` fallback for anything ambiguous, e.g. `1,234`). The
  buckets are sorted, the count is capped (at most the first 8, mirroring the
  oracle's per-node hard cap so a number-dense screen cannot explode the graph),
  and appended to the skeleton in a trailing `V:` section before hashing. The
  effect: `0`, `1`, `12` land in `ZERO`, `POS1`, `POS2` (three distinct
  signatures) while `3` and `7` (both `POS1`) collapse to one, the same bucketing
  the a11y oracle applies to node values. A screen with no numeric tokens
  produces an empty `V:` section, so the skeleton-only signature (and its locale
  and word invariants) is unchanged.

This is the **terminal value-state rule**. It does NOT match the a11y golden
vectors (those assume a `Node` tree and a `V:` section keyed by node selectors);
the binding guarantee is the one shared by every surface here, the same FNV-1a
hash family, deterministic, locale-safe, and bounded, plus the identical
`value_class` buckets and strict-decimal grammar.

Because the descriptor source for these surfaces is not a `Node` tree, **TUI (and
imgui/clay) signatures are NOT expected to match the a11y golden vectors in
`signature_vectors.json`**. Those vectors assume a `Node` tree and exercise the
a11y descriptor rules (roles, collapse, transient drop). The parity guarantee that
binds these surfaces is narrower and exact: the hash primitive and output format
are identical, and the descriptor is deterministic and locale-invariant. The TUI
backend's own tests (`crates/reproit/src/backends/tui.rs`) pin that contract
directly (locale-invariance, word-invariance, determinism, the numeric
value-class split, and the value-sensitive content fingerprint) rather than the
a11y vectors.

## Anchor short-circuit semantics

The anchor is a *prefix*, not a replacement: descriptor includes both the anchor
and the structural tokens. So:

- same route + same structure -> same node (correct)
- same route + different structure -> different nodes (handles a multi-step
  wizard at one route)
- different route + same structure -> different nodes (handles the Settings vs
  Profile collision)
- a parameterized route (`/item/:id`) with the same structure for any id ->
  one node (the id is not in the anchor; the route template is)

## Selectors (for actions / repros)

Actions address elements by the same stable hierarchy, never by text:

```
id  >  type+role  >  role + structural-index
```

`tap:<selector>` where selector is `key:<id>` or `role:<role>#<idx>`. Elements
with no stable id carry `nokey: true` in the emitted elements list (metadata
only; it does NOT affect the hash) so `map show` can warn the developer to add
one.

## The layered identity model (how the flaws are fixed)

```
identity =
  1. explicit anchor:    route / screen-key / ReproItScreen annotation
  2. structural backbone: normalized(roles + ids + icons + types + tree)
       - collapse repeated subtrees      (fixes over-split + dynamic explosion)
       - drop transient nodes            (fixes error-banner-splits-the-screen)
       - exclude all localized text      (fixes language + copy churn)
  3. discriminators:     icons, input types, ids   (fixes collisions)
  4. offline refinement: LLM-proposed merge/split rules + names, FROZEN to
                         deterministic config (never runs in the hot loop)
```

Properties: deterministic end to end (the LLM only emits frozen rules), tunable
(`map show` explains why two screens are same/different; config can add
discriminators or merge rules), and self-correcting (a bad signature shows itself
as graph explosion or behavior-collision, then you add a rule).

## Parity gate

`signature_vectors.json` holds `{ description, anchor, tree, expected_sig }`
entries. Every implementation (runners + SDKs) has a test that asserts it
produces `expected_sig` for each vector. CI fails if any component drifts.
