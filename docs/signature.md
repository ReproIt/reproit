# Screen signatures

A *signature* is a short fingerprint (8 hex characters) that answers one question:
**are these two screen captures the same screen?** reproit uses it to decide
whether a screen the fuzzer just reached is one it has already seen, so it can
build a compact graph of your app instead of treating every frame as new.

## Why it exists (the short version)

If you fingerprinted a screen by its visible text, the fingerprint would be
useless:

- It would change when you translate the app ("Welcome" vs "Willkommen").
- It would change on a harmless copy edit.
- A feed of 3 posts and the same feed with 5 posts would look like two different
  screens, and the graph would explode.

So reproit fingerprints **structure** instead: the shape of the screen (roles,
developer keys, nesting), with all human-readable text stripped out. The result:

- The same screen in any language gets the same signature.
- A list of 3 items and a list of 5 identical items get the same signature.
- A production crash and a fuzz finding on the same screen land in the same
  bucket, because every part of reproit (the runners and the production SDKs)
  computes the identical signature.

That last point is the reason this document exists at all.

## Do you need to read the rest?

Probably not. If you're just *using* reproit, this is background and you can stop
here. The rest is a precise contract for people **implementing a reproit runner
or SDK** for a new platform. Every implementation must produce the exact same
signature for the same screen, verified against the golden vectors in
`signature_vectors.json`; CI fails if any implementation drifts. The detail below
is what makes that exactness possible.

---

# The contract (for implementers)

Each component walks its platform's accessibility tree and turns it into a
normalized node tree, serializes that tree into a text *descriptor*, and hashes
the descriptor. Same descriptor, same hash, same node.

## Inputs

Every node has:

- `role`: one of a fixed, language-independent vocabulary (see Roles).
- `id`: a stable developer identifier if present (key, test-id, a11y-id,
  resource-id). Omitted if none.
- `type`: optional input refinement (`text`, `password`, `email`, `number`,
  `search`, `checkbox`, `radio`, `switch`, `slider`).
- `icon`: optional language-independent icon identity (Material codepoint, SF
  Symbol name, asset name).
- `children`: ordered child nodes.

A screen-level **anchor** is captured if available: the route/path, the
screen-level key, or an explicit annotation (`ReproItScreen("name")`).

## Roles (fixed vocabulary)

```
screen, header, text, button, link, textfield, image, icon, list, listitem,
tab, switch, checkbox, radio, slider, menu, menuitem, dialog, group, node
```

These come from accessibility roles/traits, never from the visible label. Any
unknown role maps to `node`.

## Normalization (applied before hashing)

1. **Exclude localized text.** No accessible name, label, or value text enters
   the descriptor.
2. **Drop transient nodes.** Things that flicker in and out are removed: toast,
   snackbar, spinner/progress, tooltip, transient error banner, badge. (A screen
   with an error banner showing hashes the same as without it.)
3. **Collapse repeated siblings.** When two or more consecutive siblings have an
   identical child-descriptor, emit the subtree once with a `*` repeat marker and
   drop the count. (A list of 3 vs 5 identical items hashes the same.)
4. **Stable order.** Children stay in document order; a node's `id`s are not
   reordered.

## Descriptor serialization

A pre-order walk. For each retained node, emit one token:

```
<depth>:<role>[:<type>][#<icon>][@<id>]
```

Tokens are joined with `;`. The screen anchor, if present, is prefixed:

```
descriptor = "A:" + anchor + "\n" + tokens.join(";")
```

If there's no anchor, the `A:` line is just `A:` followed by a newline. A repeat
marker from rule 3 appends `*` to the role: `2:listitem*`.

### Clarifications (normative)

These pin the simplest deterministic reading where the prose above is silent, so
every language matches the Rust oracle in
`crates/reproit/src/model/signature.rs`:

- **Token field order is fixed**: `:type` then `#icon` then `@id`, each present
  only when set. The `*` repeat marker is appended to the whole token after the
  id: `1:textfield:password#lock@pwd*` (repeats rarely carry an id, but the order
  is defined regardless).
- **Collapse comparison basis (rule 3)**: two consecutive siblings are
  "identical" iff their full subtree descriptors are byte-equal after depth is
  re-based to 0 at the sibling. Compare the normalized subtree (transients already
  dropped), not the raw input. Collapse runs are maximal and only over
  *consecutive* siblings (a, b, a does not collapse). Nested collapse applies
  recursively.
- **`transient` flag**: "transient error banner" is not a role in the vocabulary,
  so an explicit boolean `transient` field on any node marks it (and its whole
  subtree) for dropping under rule 2. The transient roles are: `toast, snackbar,
  spinner, progress, tooltip, badge` (`progress` is the role name for
  spinner/progress).
- **Depth numbering**: the root node is depth 0; each level down adds 1;
  pre-order traversal.
- **Empty / fully-transient tree**: if the root itself is transient (or no nodes
  are retained), the token body is the empty string, so the descriptor is just
  `"A:" + anchor + "\n"`. This is deterministic, not an error.
- **`type+role` selector tier**: the addressable selector string is always
  `key:<id>` (id present) or `role:<role>#<idx>` (no id). The `type+role` tier in
  the precedence list describes how the *hash* discriminates; it does not
  introduce a third selector string form.

## Hash

FNV-1a, 32-bit, over the UTF-8 bytes of the descriptor:

```
h = 0x811c9dc5
for each byte b: h = (h XOR b) * 0x01000193   (mod 2^32)
output = 8-char zero-padded lowercase hex
```

A standard FNV-1a. The function is plain; the real work is the structural
descriptor it hashes.

## Value-state apps (counters, calculators, clocks)

There's one place excluding all text goes wrong. In an app whose whole job is to
show a changing value, every keypress changes only displayed text, the structure
never moves, and the whole app would collapse to a single signature. The fix is
layered.

### Layer 1: effect detection (runner-local, NOT in the signature)

After an action, a runner decides whether the action did anything by comparing a
**content fingerprint** before and after:

```
content_fingerprint = structural_signature
                    + sorted list of (stable-key, trimmed-raw-text)
                      over text-bearing nodes
```

An action is **effective** iff the structural signature changed OR the content
fingerprint changed; otherwise it's a **no-op** (a dead key, a disabled button).
This fingerprint is ephemeral and runner-local: it carries raw localized text and
must never enter the canonical graph key (that would re-introduce the churn rule 1
removes). It's only a per-step liveness check.

### Layer 2: bounded value-classes (canonical; in the oracle)

For value-bearing nodes only, a small, locale-safe **value-class token** is folded
into the signature. The structural body is untouched; value-classes go in a
separate `V:` section.

A node is **value-bearing** iff it has a `value` AND either its role is in the
value-role set OR it carries the opt-in `value_node` flag (Layer 3). The
value-role set is:

```
textfield, status, log, progressbar, meter, timer, output
```

Several of these are not in the structural role vocabulary, so they normalize to
`node` in the body; the value-role test therefore uses the node's **raw** role.
**Chrome roles are never value-bearing** (`button`, `label`, `header`, `text`,
`link`), so rule 1's text exclusion is preserved.

`value_class(s)` maps a value string to one bounded token, deterministically and
locale-safely (it refuses to guess grouped or locale number formats):

```
trim s
  s is empty / all whitespace                 -> "EMPTY"
  s matches ^[+-]?[0-9]+(\.[0-9]+)?$  (period decimal, NO grouping)
      parse to a number n:
          n == 0                               -> "ZERO"
          n < 0                                -> "NEG"
          |n| < 10                             -> "POS1"
          |n| < 100                            -> "POS2"
          |n| < 1000                           -> "POS3"
          |n| >= 1000                          -> "POSL"
  otherwise (non-numeric OR ambiguously formatted) -> "NONEMPTY"
```

The numeric grammar is strict ASCII: optional sign, digits, optional period and
digits. No grouping, no exponent, no leading/trailing dot, no non-ASCII digits, no
currency or percent. Anything outside it (`1,234`, `$5`, `5%`) falls to
`NONEMPTY`, because the locale is unknown and we refuse to guess.

**`V:` serialization.** For each value-bearing node emit `key=valueclass`, where
`key` is `key:<id>` if present else the structural fallback `role:<role>#<idx>`.
Entries are sorted by key, joined with `;`, and appended as a trailing line:

```
descriptor = "A:" + anchor + "\n" + tokens.join(";") + "\nV:" + v_entries
```

Transient subtrees are excluded from `V:` too, so body and `V:` stay consistent.

**Backward compatibility (hard requirement).** The `V:` line is emitted only when
at least one value-bearing node exists, so a tree with none produces the exact
same descriptor and hash as before this layer existed and every old golden vector
still passes. (One deliberate interaction: two keyless, structurally identical
value nodes still collapse to one `*` token in the body, but their two
value-classes still appear separately in `V:` under distinct
`role:<role>#<idx>` keys.)

**Hard cap (runner-enforced).** At most 8 distinct value-class combinations per
structural node. The oracle is stateless per call; the cap is a runner bound: once
a runner has seen more than 8 variants of one node, it falls back to
structural-only for that node, so an adversarial value generator can't explode the
graph.

### Layer 3: opt-in value selectors (config)

A `reproit.yaml` may list extra nodes to treat as value-bearing even when their
role isn't in the set:

```yaml
value_nodes:
  - key:score
  - role:text#2
```

A runner resolves each selector and sets the `value_node` flag before signing;
the oracle then treats it as value-bearing through the same path.

## Surfaces without an accessibility tree (terminals, ImGui/Clay)

Most surfaces have an accessibility tree to walk. Some don't: a terminal is a grid
of character cells, and an immediate-mode GUI (ImGui, Clay) has only a per-frame
widget tree. The rule for these: **derive the descriptor from the surface's own
structural source, then hash it with the same FNV-1a primitive and 8-hex output.**
The source differs by necessity, but the hash family is identical, so every
signature lives in one comparable namespace. This is intentional and fully
deterministic, not a fallback.

- **ImGui / Clay:** the structural source is the widget tree the app already
  builds each frame (window/child/group nesting, widget kinds, stable string ids).
  Normalize and serialize it like the node descriptor (structure and ids, never
  the drawn text), then hash.

- **TUI (terminal):** the structural source is the **layout skeleton**, the part
  that survives translation. From the cell grid we keep what's stable across
  locales and erase what isn't:
  - box-drawing / block glyphs -> one edge marker (`#`), unifying single/double/
    rounded edges;
  - digits -> one marker (`9`): "a number is here", value dropped;
  - ASCII punctuation / symbols (`:`, `[`, `]`, `/`, `$`) -> kept verbatim (these
    are non-localized field markers and bracketed hotkeys);
  - spaces / newlines -> kept verbatim (they *are* the layout);
  - any run of letters in any language -> one word placeholder (`W`): the
    localized identity of words is excluded, exactly as rule 1 excludes text.

  Each maximal run of one class is collapsed to a length-prefixed token, so the
  *extents* (a 20-wide border vs a 4-wide one) stay structural while per-glyph
  identity is gone. The cursor cell (row, col) is appended, because which field is
  focused is structure. That skeleton is hashed with the same FNV-1a. The same
  screen in English and German produces the same skeleton, so the same signature.

### TUI value-state

The skeleton maps digits to `9` and words to `W`, which is the value-state problem
again on a terminal (a counter's skeleton never moves). A terminal has no
accessibility tree and no value roles, so the node value-class doesn't apply; the
TUI backend reproduces the two layers from screen text directly:

- **Effect detection (Layer 1 analogue, runner-local).** Alongside the skeleton
  signature, the runner computes `FNV-1a(full raw screen text + cursor cell)`,
  hashing the actual cells (digits and words verbatim). An action is effective iff
  the skeleton signature changed OR this fingerprint changed. This catches a
  counter ticking `0 -> 1 -> 2` whose skeleton is byte-identical. The fingerprint
  is ephemeral and never enters the canonical state set.
- **Numeric value-class (Layer 2 analogue, bounded, in the signature).** The
  runner extracts numeric tokens from the screen and maps each through the **same
  `value_class` bucketer and strict grammar** as the oracle, sorts and caps them
  (first 8), and appends them in a trailing `V:` section before hashing. So `0`,
  `1`, `12` become three distinct signatures (`ZERO`, `POS1`, `POS2`) while `3`
  and `7` (both `POS1`) collapse to one.

Because the descriptor source isn't a node tree, TUI and ImGui/Clay signatures are
**not** expected to match the a11y golden vectors in `signature_vectors.json`.
The guarantee they share with every surface is narrower and exact: the same hash
primitive and output format, a deterministic locale-invariant descriptor, and the
identical `value_class` buckets. The TUI backend's own tests
(`crates/reproit/src/backends/tui.rs`) pin that contract directly.

## Anchors

The anchor is a prefix, not a replacement: the descriptor includes both the
anchor and the structural tokens. So:

- same route + same structure -> same node (correct)
- same route + different structure -> different nodes (a multi-step wizard at one
  route)
- different route + same structure -> different nodes (Settings vs Profile)
- a parameterized route (`/item/:id`) with the same structure for any id -> one
  node (the id isn't in the anchor; the route *template* is)

## Selectors (for actions and repros)

Actions address elements by the same stable hierarchy, never by text:

```
id  >  type+role  >  role + structural-index
```

So `tap:<selector>` where the selector is `key:<id>` or `role:<role>#<idx>`.
Elements with no stable id carry `nokey: true` in the emitted elements list
(metadata only; it doesn't affect the hash), so `map --show` can warn you to add
one.

## The full identity model

```
identity =
  1. anchor:             route / screen-key / ReproItScreen annotation
  2. structural backbone: normalized(roles + ids + icons + types + tree)
       - collapse repeated subtrees   (fixes over-split + dynamic explosion)
       - drop transient nodes         (fixes "error banner splits the screen")
       - exclude all localized text   (fixes language + copy churn)
  3. discriminators:     icons, input types, ids   (fixes collisions)
  4. offline refinement: LLM-proposed merge/split rules + names, frozen to
                         deterministic config (never runs in the hot loop)
```

This is deterministic end to end (the LLM only emits frozen rules), tunable
(`map --show` explains why two screens are the same or different, and config can
add discriminators or merge rules), and self-correcting (a bad signature shows up
as graph explosion or a behavior collision, which tells you to add a rule).

## Parity gate

`signature_vectors.json` holds `{ description, anchor, tree, expected_sig }`
entries. Every implementation (runners and SDKs) has a test asserting it produces
`expected_sig` for each vector. CI fails if any component drifts.
