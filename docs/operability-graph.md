# Operability graph: the ground-truth-vs-accessibility diff

Status: design + phased plan. Web premise validated live (see §Validation).

## The idea

reproit already maps an app by walking its **accessibility/structural tree**. That
makes the engine blind to controls that aren't exposed accessibly — which is
exactly the apps that most need an audit (the paradox: worst-a11y apps are least
drivable). The fix is reproit's existing "map it twice and diff" pattern (the
cross-engine divergence oracle) applied to a new axis:

- **Graph 1 — ground truth**: everything a sighted pointer user can actually
  operate (real interactive controls / handlers / hit-testable affordances),
  discovered WITHOUT relying on accessibility semantics.
- **Graph 2 — accessibility**: the subset reachable & operable via the a11y tree
  + keyboard (correct role/name/value, focusable, keyboard-activatable, no traps).

The **diff (operable in 1, not in 2)** is the deliverable: deterministic WCAG
**2.1.1** (operable by mouse, not keyboard) and **4.1.2** (operable, no role/name)
+ focus-trap findings — the operability failures static linters (axe/Lighthouse)
structurally cannot find because they lint a snapshot instead of comparing
operability. It also un-cripples the engine (graph 1 doesn't need a11y) and
doubles as the "AI built a fake control" detector.

CONSTRAINT: the core diff stays DETERMINISTIC — no LLM, no ML/vision understanding
of pixels. (Deterministic framebuffer *diffing* is allowed; it's the visual-oracle
machinery.) Per-framework code is fine.

## Framework-agnostic contract (the architectural invariant)

Every backend emits one new marker per node, keyed by reproit's EXISTING
`sel`/`key` selector grammar, so the engine stays platform-blind:

```
EXPLORE:GROUNDTRUTH { sig, elements: [{
  id,                       // existing selector grammar (the join key)
  operable: true,           // graph 1: real pointer/gesture affordance
  gestureKind,              // tap|button|field|delegated|raw|...
  a11y: { rolePresent, namePresent, focusable, inTabOrder, keyboardActivatable }
}], focusTrap }
```

The Rust engine (`map.rs`) re-derives the diff (never trusts the runner) into the
existing `a11y` oracle category: `pointer_only`, `unlabeled`/`no_role`,
`keyboard_unreachable`, `focus_trap`. This generalizes today's scalar
`unlabeled_tappables: u32` into a structured, per-widget gap list
(`OperabilityGaps`): the counts PLUS `items: [{selector, kinds}]` — the per-
element detail (which selector failed which dimension), so the diff is grounded
and addressable, not just a tally.

CONSTRAINT (capture is non-destructive): determining the a11y dims must NEVER
activate a control. Probing `keyboardActivatable` by actually pressing Enter/
Space (or dispatching a synthetic key) fires the app's real handler as a side
effect — a navigation, or a destructive/crashing action — which pollutes the
crash oracle and corrupts exploration. So `keyboardActivatable` is derived
structurally: a native-activating control, or one carrying a real key listener
(read via CDP `getEventListeners` on web/electron), counts; a focusable
click-only control with no key handler is keyboard-dead (a 2.1.1 gap). The other
backends compute it from the widget/semantics tree, never by activating.

## Consumption (how the diff is surfaced)

The stored gaps are served two ways, both pure views (no analysis, fully
deterministic). Each gap is GROUNDED for a fixer: it carries the failing
selector, the dimension(s) it fails, and a static source location (file:line +
snippet, via `attribute`); each screen carries its route and a best-effort
action path (BFS over the map's transitions) to reach it. That closes the loop:
find -> locate (file:line) -> fix -> `reproit_check` confirms it.
- `reproit map accessibility` — the human/CLI view; `--state` and `--kind` filter.
- `reproit_accessibility(state?, kind?)` — the MCP tool, returns the same diff as
  JSON so an agent can read it, open the file:line, fix the control, and call
  `reproit_check` to deterministically confirm the gap closed (you propose,
  reproit disposes). Source attribution is best-effort (static); a sparse map may
  yield no action path, in which case the route alone locates the screen.

## How each backend sources graph 1

The engine is identical; only a thin per-surface adapter differs. Sourcing
priority (best signal first): in-process tree -> framebuffer probe -> a11y-only.

| Backend | Graph-1 source | Join to graph 2 | Verdict |
|---|---|---|---|
| **Flutter** | element/render tree (gesture detectors, `hitTestable`) | render-ancestry + rect | flagship; runs on headless `flutter test`, no device; public API survives AOT |
| **Web / Electron** | CDP `DOMDebugger.getEventListeners` (incl. delegated) + native + `cursor:pointer`; real Tab traversal; `keyboardActivatable` from listener *types* (native or a key handler) | same `sel` | clean |
| **Tauri** | in-page native/cursor/attr (no CDP); `keyboardActivatable` structural (native + inline key handlers) | same `sel` | partial (no listener enumeration) |
| **React Native** | JS fiber/press-handler tree vs exported a11y props | `nativeID`/`testID` | partial; needs dev build |
| **Native (Qt/WPF/AppKit/GTK)** | in-process agent walks the real widget/visual/NSView tree + handlers | **object identity** (a11y peer is created from the widget) | best once in-process; needs per-toolkit agent |
| **TUI** | (A) unlabeled-region from grid diff; (B) opt-in keys+SGR-mouse walk vs keys-only | n/a | reframed; (B) is the one true instance |
| **ImGui / Clay** | header already enumerates widgets; a11y is empty by construction | n/a | the whole surface is the gap; header can also *generate* an AccessKit tree |
| **universal floor** | deterministic click -> framebuffer-diff probe | spatial | works on any rendered surface; coarse, opt-in, side-effecting |

## In-process native agent (how `reproit map` works on Qt/WPF/AppKit/GTK)

The "runner" becomes an in-process agent speaking the same marker protocol
(exactly like the Dart explorer / ImGui header).

- **Get in**: white-box (one line + rebuild: `ReproIt::attach()` / a NuGet / a
  shim — primary, like the Flutter SDK), or black-box injection
  (`LD_PRELOAD` / `DYLD_INSERT_LIBRARIES` / CLR profiler — fallback; hardened
  runtime can block it).
- **Per-state cycle** (on the UI thread): settle -> walk ground-truth tree ->
  walk a11y tree -> compute the shared FNV-1a signature -> join by object
  identity -> emit `EXPLORE:STATE` + `EXPLORE:GROUNDTRUTH` -> receive action ->
  invoke in-process (`QAbstractButton::click()`, WPF `InvokePattern`/`RaiseEvent`,
  AppKit `performClick:`) -> emit `EXPLORE:EDGE` -> repeat.
- The a11y peer is created *from* the widget on these toolkits, so the
  graph-1<->graph-2 join is by **object identity** — cleaner than Flutter's
  geometric join. Once in-process, native is among the strongest tiers, not the
  weakest.

## Why not static code analysis (for the graph)

"What's actually on screen" is undecidable from source (conditional/data-driven
rendering, runtime state, dynamic handlers); a static graph over-approximates and
maps to no real screen. The slice it can do (missing-label lint) is a crowded,
solved space (eslint-jsx-a11y, axe-linter) and contradicts reproit's runtime
"reproduce forever" identity. Keep static analysis only for: (a) **source
attribution** of a runtime-found gap (for the fix/PR), (b) optional **route
seeding** to guide exploration. Never as the graph builder.

## Validation

**Web — validated live** against the running cloud dashboard (a real app with a
`<div role=option tabindex=-1>` finding item, clickable only via a document-level
delegated handler). A standalone Playwright+CDP two-graph probe found: 5 finding
items operable by pointer (cursor + delegated, behavioral-click confirmed),
**0** of them reachable by a real 60-press Tab traversal (12 other controls were)
-> 5 WCAG 2.1.1 gaps. CONFIRMED, deterministic.

Per-platform validation environments:
- Web / Electron / AppKit / Flutter (headless) / TUI / Linux Qt+GTK: local macOS + Linux.
- **WPF / WinUI / Windows UIA: the QEMU Windows VM** (ssh reproit@localhost:2222, dotnet 8).
- React Native: Android emulator / iOS sim (Appium).

## Phased plan

0. **Engine contract** — `EXPLORE:GROUNDTRUTH` marker + diff in `map.rs` + a11y
   gap categories. Framework-agnostic core.
1. **Web reference** — web-runner: CDP listeners + cursor/native + Tab/Enter pass.
   Dogfood target: the cloud dashboard (the validated case).
2. **Flutter** — element/render-vs-semantics in the headless explorer (cheapest;
   `unlabeled_tappables` already exists to generalize).
3. **Native in-process agents** — WPF/UIA first (validate on the Windows VM),
   then AppKit (mac), Qt + GTK (linux). White-box agent + inject fallback.
4. **RN** (fiber-vs-a11y), **Electron/Tauri** (reuse web), **TUI** (mouse + unlabeled),
   **ImGui/Clay** (`exposed` flag + AccessKit generation).
5. **Universal floor** — framebuffer-diff click probe; **source attribution** for fixes.

Each phase ships behind the existing `a11y`/`--only a11y` oracle filter and is
validated on a real app on that platform before the next.
