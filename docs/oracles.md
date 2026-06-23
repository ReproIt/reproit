# Oracles by backend

An *oracle* is one named class of bug the fuzzer can catch. reproit ships eight,
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
| overflow | `EXPLORE:OVERFLOW` | a child element spilling out of its container |
| dead-end | `EXPLORE:STATE` + `EXPLORE:EDGE` (graph) | a non-terminal screen with no way out |
| flicker | `EXPLORE:RERENDER` / `EXPLORE:FLICKER` | a wasteful repaint / transient visual divergence |
| content-bug | `EXPLORE:CONTENTBUG` | `[object Object]`, `undefined`, `{{unrendered}}`, NaN on screen |
| jank | `EXPLORE:JANK` / sim frame manifest | a transition that drops frames |
| hang | `EXPLORE:HANG` | an action that freezes the UI |
| leak | `MEMORY:SAMPLE` (`--soak`) | memory that grows and never comes back |
| layout-shift | `EXPLORE:SHIFT` | a tap that makes the page reflow and jump (Web Chromium only) |

The layout-shift oracle is distinct from overflow: overflow is static ("content
does not fit its box"), layout-shift is *motion on interaction* ("a tap made the
page jump"). It keys off the browser's Layout Instability API (the source of
Google's CLS), summing the shift score during each action's settle window and
flagging a transition whose score passes the CLS "poor" floor (0.25) — so a
jarring whole-page jump (e.g. a code-block language tab that resizes and pushes
content) is caught while a small local expansion is not. Chromium-only (like
Long Tasks jank), so silent on firefox/webkit and the non-web backends.

## Coverage matrix

`Y` = fires. `Y*` = fires, but coarse (session/process-level, not per-transition):
leak via process-RSS sampling under `--soak`. `~` = best-effort with a documented
caveat. `gap` = the platform does not expose the signal; not emitted (never faked).
`n/a` = the bug class cannot exist on that surface.

| Backend (driver) | crash | overflow | dead-end | flicker | content-bug | jank | hang | leak |
|---|---|---|---|---|---|---|---|---|
| Web Chromium (CDP) | Y | Y | Y | Y (+pixel) | Y | Y | Y | Y |
| Web Firefox/WebKit | Y | Y | Y | Y | Y | Y | Y | Y |
| Electron (CDP) | Y | Y | Y | Y (+pixel) | Y | Y | Y | Y |
| Tauri (WebDriver) | Y | Y | Y | Y (DOM) | Y | Y | Y | Y* |
| Flutter sim | Y | Y | Y | Y | Y | Y | Y | Y |
| Flutter headless | Y | Y | Y | gap | Y | n/a | n/a | ~ |
| RN / native Android (Appium) | Y | Y | Y | gap | Y | Y | Y | Y |
| RN / native iOS (Appium) | Y | Y | Y | gap | Y | gap | Y | Y* |
| Desktop macOS (AX) | Y | Y | Y | gap | Y | n/a | ~ | Y |
| Desktop Windows (UIA) | Y | Y | Y | gap | Y | gap | Y | Y |
| Desktop Linux (AT-SPI) | Y | Y | Y | gap | Y | n/a | ~ | Y |
| TUI (PTY) | Y | gap | Y | Y | Y | n/a | Y | Y* |
| Dear ImGui / Clay (instrumented) | Y | gap | Y | gap | Y | Y | gap | Y* |

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

## Remaining gaps (why)

These are genuine platform limits, not unfinished work. Each is documented in-code
at the runner that would emit it.

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
