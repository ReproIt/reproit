# Oracles by backend

An *oracle* is one named class of bug the fuzzer can catch. reproit ships ten,
all on by default. The oracle core is platform-agnostic: each per-backend runner
emits the same `EXPLORE:*` / `MEMORY:*` markers, and the Rust core
(`crates/reproit/src/model/{map,invariants}.rs`) evaluates them identically no
matter which UI framework produced them. That is the point of the marker
contract: one finding shape, one shrink/reproduce/report pipeline, every
platform.

But a marker can only be emitted where the platform actually exposes the signal.
A browser exposes a Long Tasks trace and a precise V8 heap; an accessibility tree
does not. So coverage is not uniform, and faking a signal a platform cannot
deliver would mean false positives, which are worse than a missing oracle. This
page records, honestly, what fires where and why the gaps exist.

## The oracles

| Oracle | Marker | Catches |
|---|---|---|
| crash | exception block | an uncaught exception / signal |
| choice-anomaly | `EXPLORE:CHOICEBUG` | one option of a multi-choice component (ARIA tab/radio group, button-cluster picker, or native `<select>`) shifts the global layout when its siblings do not (Web / Electron / Tauri) |
| overflow | `EXPLORE:OVERFLOW` | a child element spilling out of its container |
| dead-end | `EXPLORE:STATE` + `EXPLORE:EDGE` (graph) | a non-terminal screen with no way out |
| flicker | `EXPLORE:RERENDER` / `EXPLORE:FLICKER` | a wasteful repaint / transient visual divergence |
| content-bug | `EXPLORE:CONTENTBUG` | `[object Object]`, `undefined`, `{{unrendered}}`, NaN on screen |
| jank | `EXPLORE:JANK` / sim frame manifest | a transition that drops frames |
| hang | `EXPLORE:HANG` | an action that freezes the UI |
| leak | `MEMORY:SAMPLE` (`--soak`) | memory that grows and never comes back |
| broken-route | `EXPLORE:BROKENROUTE` | the app links to a URL whose document returns 404 / 410 / 5xx (a dead route). Not 401/403/429 (intentional auth gates / rate limits) (Web + Electron; Tauri gap, no document-status stream over WebDriver) |

The **choice-anomaly** oracle is differential, not absolute, which is what keeps
it false-positive-free. When the fuzzer finds a multi-choice component (an ARIA
`tab`/`radio` group, or a cluster of sibling buttons where exactly
one is selected, e.g. a code-block language picker), it exercises *every* choice
and measures each one's effect on the GLOBAL layout (page horizontal-overflow +
the page-absolute displacement of chrome anchors outside the component). The
expected behavior is the COMMON effect across choices (every language resizes the
code block a bit); a bug is the choice whose effect is an OUTLIER versus its
siblings (only one language also shifts the whole page). It fires only when one
choice's effect is >= 3x the sibling median and above a floor, so uniform choices
produce nothing. It needs the live layout, so it runs on the three browser-backed
backends -- Web, Electron (CDP, same as Web Chromium), and Tauri (the same in-page
pass injected via WebDriver `execute()`) -- and the differencing/threshold rule is
ONE shared implementation (`runners/web/choice-oracle.mjs`), so a finding means the
same thing on every backend. A multi-choice component is selected by accessible
label so below-fold pickers are scrolled into view and exercised. A native
`<select>` is now treated as a choice component too: its `<option>`s are
enumerated, each is selected (setting `.value` + dispatching `change`/`input` so
frameworks react) and differenced with the SAME global-layout measurement and the
SAME outlier rule, then the original value is restored (non-destructive). That
closes the most common real-world picker, which the snapshot otherwise maps to a
text field and never differences.

The **overflow** oracle reports only the kind+element combinations that are
*bugs*, not the many INTENTIONAL uses of CSS overflow (scroll containers, ellipsis
truncation, full-bleed art). The runner measures every overflow structurally; the
reporting filter (`model/invariants::overflow_is_break`) then keeps:
- `spill` — a child escaping its parent's content box by a visible margin
  (>= `max(overflowMinPx, 8px)`), excluding icon/inline artifacts (svg/img/...);
- `clip` — a single-line label hard-truncated inside an **interactive control**
  (button/link/tab): the i18n/long-string failure that hides the action's text. The
  identical ellipsis on a caption/cell is intended truncation and is dropped;
- `scroll` — only the **document** scroller (the whole page scrolls sideways);
  per-element scroll is an intended container;
- `flex` — Flutter's own `RenderFlex overflowed` (never intentional).

The user-configurable floor `invariants.overflowMinPx` (default 2px) raises the
spill/clip/page-scroll threshold on top of this (set, say, 40 to report only
dramatic overflows). It is exact-pixel, not a magic heuristic.

## Coverage matrix

`Y` = fires. `Y*` = fires, but coarse (session/process-level, not per-transition):
leak via process-RSS sampling under `--soak`. `~` = best-effort with a documented
caveat. `gap` = the platform does not expose the signal (or the oracle is not wired
for it yet); not emitted (never faked). `n/a` = the bug class cannot exist on that
surface. `choice` = choice-anomaly; `route` = broken-route.

| Backend (driver) | crash | choice | overflow | dead-end | flicker | content-bug | jank | hang | leak | route |
|---|---|---|---|---|---|---|---|---|---|---|
| Web Chromium (CDP) | Y | Y | Y | Y | Y (+pixel) | Y | Y | Y | Y | Y |
| Web Firefox/WebKit | Y | Y | Y | Y | Y | Y | Y | Y | gap | Y |
| Electron (CDP) | Y | Y | Y | ~ | Y (+pixel) | Y | Y | Y | Y | Y |
| Tauri (WebDriver) | Y | Y | Y | Y | Y (DOM) | Y | Y | Y | Y* | gap |
| Flutter sim | Y | gap | Y | Y | Y | Y | Y | Y | Y | n/a |
| Flutter headless | Y | gap | Y | Y | gap | Y | n/a | n/a | ~ | n/a |
| RN / native Android (Appium) | Y | gap | Y | ~ | gap | Y | Y | Y | Y | n/a |
| RN / native iOS (Appium) | Y | gap | Y | ~ | gap | Y | gap | Y | Y* | n/a |
| Desktop macOS (AX) | Y | gap | Y | Y | gap | Y | n/a | ~ | Y | n/a |
| Desktop Windows (UIA) | Y | gap | Y | Y | gap | Y | gap | Y | Y | n/a |
| Desktop Linux (AT-SPI) | Y | gap | Y | Y | gap | Y | n/a | ~ | Y | n/a |
| TUI (PTY) | Y | n/a | gap | Y | Y | Y | n/a | Y | Y* | n/a |
| Dear ImGui / Clay (instrumented) | Y | n/a | gap | Y | gap | Y | Y | gap | Y* | n/a |

## Recently closed (and how)

These were gaps that turned out to have a real, deterministic signal that was
just never wired. Each holds the same false-positive bar as the rest.

- **jank + hang on Firefox/WebKit** (`Y`): the Long Tasks trace is Chromium-only,
  so a cross-engine `requestAnimationFrame` frame-drop detector now covers the
  non-Chromium engines (Chromium keeps the more precise Long Tasks path). FP-safe:
  a lone late frame counts only past 350ms (well above GC blips) or as a sustained
  run of long frames; a single GC pause is dropped. The classifier is unit-tested
  in both directions, and the no-false-positive behavior is runtime-validated on
  real firefox and webkit (clean static and animated sites stay silent).
- **leak via process-RSS sampling under `--soak`** (`Y*`, coarse): a leaked process
  still grows its resident set, so where there is no precise heap readout, the
  runner samples the target process's RSS per soak cycle and emits the same
  `MEMORY:SAMPLE` series the slope oracle reads. Now covers Tauri (the webview
  process, replacing the quantized `performance.memory` fallback), iOS (the sim
  app's host pid via `simctl launchctl list`), the TUI (the child pid), and
  ImGui/Clay (self RSS). Coarse (per-cycle, not per-transition) and gated on a
  *uniquely resolvable* pid, so any ambiguity stays silent rather than guessing.
- **content-bug on the TUI, ImGui, and Clay** (`Y`): the TUI runner scans the
  settled VT grid; the instrumented ImGui/Clay runners scan the actual label
  strings the app draws (`ImGui::Text`/button labels, Clay text commands). All use
  the same artifact tokens as the DOM scanner (`[object Object]`, whole-word
  `undefined`/`null`/`NaN`, unrendered `{{...}}`/`${...}`), keyed by a stable
  position / widget id.
- **jank on ImGui/Clay** (`Y`): these are instrumented and render real frames, so
  per-frame durations are timed directly and fed the same jank/hang floors as the
  web runner. (Their leak is the coarse RSS path above.)
- **jank + hang on Tauri** (`Y`): Long Tasks is Chromium-only (so silent on Tauri's
  WebKit webview on mac/Linux). The same cross-engine `requestAnimationFrame`
  detector built for Firefox/WebKit is injected into the webview via `execute()`;
  Chromium/WebView2 keeps the precise Long Tasks path. Reuses the FP-validated
  classifier verbatim.
- **choice-anomaly on Electron and Tauri** (`Y`): the differential outlier rule and
  the global-layout measurement now live in ONE shared module
  (`runners/web/choice-oracle.mjs`), with a self-contained in-page pass that finds
  the page's choice components (native `<select>`, ARIA tab/radio groups, button-
  cluster pickers), exercises each option, and flags the one whose effect on the
  page outside the component is an outlier (>= 3x the sibling median and above the
  floor). Electron is Chromium, so it runs that pass over CDP (`page.evaluate`)
  exactly like Web; Tauri has no CDP, so the SAME pass is injected via WebDriver
  `executeAsync()`. It is non-destructive (each component is restored) and stays
  differential, so it inherits the no-false-positive bar verbatim. The unit test
  (`runners/web/choice-oracle.test.mjs`) drives the exact in-page function on a
  real Chromium fixture, covering all three ports' detector.
- **broken-route on Electron** (`Y`): Electron's renderer is Chromium with the same
  Playwright `response` events as Web, so the web runner's broken-route oracle ports
  directly: record the HTTP status of main-frame document navigations (origin pinned
  from the first document response), fire on 404/410/5xx, and run the same end-of-
  crawl two-stage link check (HEAD filter, then real-navigation verify) that avoids
  the SPA fetch-vs-navigation false positive. Excludes 401/403/429 (intentional auth
  gates / rate limits) exactly as on Web. Gated on an http(s) app origin: a packaged
  `file://` Electron app has no server status to read, so it stays silent there.

## Remaining gaps (why)

These are genuine platform limits, not unfinished work. Each is documented in-code
at the runner that would emit it.

- **leak on Firefox/WebKit browsers** (`gap`): the precise heap readout comes from
  the CDP `Runtime.getHeapUsage` domain, which is Chromium-only. The cross-engine
  fallback is `performance.memory`, a non-standard API that Firefox and WebKit do
  not implement, so the read returns nothing and NO `MEMORY:SAMPLE` is emitted (a
  quantized, leak-blind number would be worse than silence). Run `--soak` on
  Chromium for the heap-slope leak oracle; the other engines still get every other
  oracle.
- **broken-route on Tauri** (`gap`): broken-route needs the document's HTTP STATUS
  for a navigation, and the WebDriver surface Tauri drives over exposes no such
  stream -- there is no CDP `response`/`Network` domain, and Tauri serves its app
  over a custom protocol (`tauri://` / the asset protocol), not HTTP with a status
  the driver surfaces. An in-page `fetch().status` probe is exactly the
  fetch-vs-navigation false positive the web runner guards against with a real
  navigation that re-reads the status, and WebDriver `url()` navigates but cannot
  read the resulting status. So a 404/410/5xx cannot be told from a working route
  FP-free here; emitting a guessed signal would break the no-false-positive bar, so
  it stays silent. (choice-anomaly DID port to Tauri -- it runs entirely in-page, so
  it needs no status stream; broken-route on Electron ported too, since Electron's
  Chromium renderer surfaces real HTTP responses.) Native, Flutter, TUI, and
  ImGui/Clay surfaces have no URL-addressable routes (broken-route `n/a`) and, for
  the immediate-mode TUI/ImGui/Clay, no stable layout box to difference (choice
  `n/a`).
- **dead-end on Electron and React Native** (`~`): these runners drive taps but not
  text entry yet, so a screen whose only forward exit is through a form field reads
  as a false sink. Tracked as text-field driving for those runners; until it lands,
  treat a dead-end finding on Electron/RN as best-effort and confirm the screen has
  no typed-input escape. Web, Tauri, Flutter, and the desktop a11y drivers do drive
  fields, so their dead-end stays exact.
- **jank on accessibility trees, the TUI/PTY, and Flutter-headless** (`n/a`): jank
  is dropped frames, and an a11y tree or a VT character grid has no frame timeline;
  the headless Flutter tier runs on a fake clock. Nothing to read, so nothing is
  emitted. (Flutter sim reads a real per-frame manifest; ImGui/Clay are
  instrumented and now do emit jank.)
- **jank on iOS (Appium)** (`gap`): no sim-attributable frame source exists (tried
  against a real booted sim): xctrace's `Animation Hitches` template is unsupported
  on the simulator, `Metal System Trace` captures host-wide GPU (the sim app fuses
  into the host, so it is unattributable), and `xctrace --attach` cannot target an
  in-simulator process. (iOS *leak* is covered by the RSS sampler above; Android
  gets both via `dumpsys`.)
- **jank on Windows desktop (UIA)** (`gap`): a real signal exists but only
  in-process. Post-Win8.1 `DwmGetCompositionTimingInfo` accepts only `HWND=NULL` and
  returns desktop-global counters (another app's animation reads as your jank; your
  frozen app reads as clean), and the clean per-window `IDXGISwapChain::GetFrameStatistics`
  needs the app's own swapchain. So the out-of-process UIA driver cannot reach it
  FP-free; an in-process Windows agent could. (Hang is covered by `IsHungAppWindow`.)
- **pixel-flicker on Tauri** (part of `flicker`): the pixel tier needs CDP
  `Page.startScreencast` to diff presented frames; WebDriver exposes no
  presented-frame stream. Tauri keeps its DOM-based rerender-flicker, unaffected.
- **hang on macOS / Linux desktop** (`~`): a host-side wall-clock watchdog around
  the synchronous AX / AT-SPI round trip. It is host wall time, perturbable by
  scheduling, so a high 2000ms floor keeps it false-positive-free. Windows uses the
  OS `IsHungAppWindow` signal directly and has no such caveat.
- **overflow on TUI / Clay** (`gap`): a VT grid clips rather than overflowing, and
  Clay's render-command stream is flat with no parent linkage, so a child-exceeds-
  parent check would need version-fragile parentage reconstruction. ImGui overflow
  is `n/a` (immediate-mode clips/auto-sizes, no stable container box).

## Determinism bar

Every oracle keys its finding off structure (element ids, roles, keys, bounds),
never visible text, uses coarse far-apart thresholds, and degrades to silence
when the signal channel is absent. The same seed reproduces the same finding id
on replay across every backend, which is what makes a finding shrinkable and a
regression test stable.
