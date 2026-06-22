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

## The eight oracles

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

## Coverage matrix

`Y` = fires. `~` = best-effort with a documented caveat. `gap` = the platform
does not expose the signal; not emitted (never faked). `n/a` = the bug class
cannot exist on that surface.

| Backend (driver) | crash | overflow | dead-end | flicker | content-bug | jank | hang | leak |
|---|---|---|---|---|---|---|---|---|
| Web Chromium (CDP) | Y | Y | Y | Y (+pixel) | Y | Y | Y | Y |
| Web Firefox/WebKit | Y | Y | Y | Y | Y | ~ | ~ | Y |
| Electron (CDP) | Y | Y | Y | Y (+pixel) | Y | Y | Y | Y |
| Tauri (WebDriver) | Y | Y | Y | Y (DOM) | Y | ~ | ~ | ~ |
| Flutter sim | Y | Y | Y | Y | Y | Y | Y | Y |
| Flutter headless | Y | Y | Y | gap | Y | n/a | n/a | ~ |
| RN / native Android (Appium) | Y | Y | Y | gap | Y | Y | Y | Y |
| RN / native iOS (Appium) | Y | Y | Y | gap | Y | gap | Y | gap |
| Desktop macOS (AX) | Y | Y | Y | gap | Y | n/a | ~ | Y |
| Desktop Windows (UIA) | Y | Y | Y | gap | Y | n/a | Y | Y |
| Desktop Linux (AT-SPI) | Y | Y | Y | gap | Y | n/a | ~ | Y |
| TUI (PTY) | Y | gap | Y | Y | gap | n/a | Y | n/a |
| Dear ImGui / Clay (instrumented) | Y | gap | Y | gap | gap | n/a | gap | n/a |

## Why the gaps exist

These are platform limits, not unfinished work. Each is documented in-code at the
runner that would emit it.

- **jank on accessibility trees and the TUI** (`n/a`): jank is dropped frames, and
  an a11y tree or a VT character grid has no GPU frame timeline to read. The web
  oracle keys jank off the browser's Long Tasks API, which has no desktop-a11y or
  terminal analogue. Flutter sim reads a real per-frame manifest; the headless
  tier runs on a fake clock, so timing oracles (jank, hang) are sim-only there.
- **jank + leak on iOS (Appium)** (`gap`): XCUITest exposes neither a per-transition
  frame trace nor a heap/footprint readout over the session. The only source is
  Instruments/xctrace, which runs out-of-band and cannot be keyed to a single
  `(from, action)` transition. Android gets both via `dumpsys gfxinfo` /
  `dumpsys meminfo`, which Appium reaches through `mobile: shell`.
- **jank + hang on Firefox/WebKit, jank on Tauri** (`~`): the Long Tasks trace is
  Chromium-only, so these degrade to silence on non-Chromium engines rather than
  guess. Same honest fallback either way.
- **leak on Tauri** (`~`): WebDriver has no CDP, so there is no precise V8 heap. It
  falls back to `performance.memory`, which is quantized on WebView2/Chromium and
  absent on WKWebView. Electron and Chromium use the precise
  `Runtime.getHeapUsage` + forced-GC path.
- **pixel-flicker on Tauri** (part of `flicker`): the pixel tier needs CDP
  `Page.startScreencast` to diff presented frames; WebDriver exposes no
  presented-frame stream. Tauri keeps its DOM-based rerender-flicker, which is
  unaffected.
- **hang on macOS / Linux desktop** (`~`): implemented as a host-side wall-clock
  watchdog around the synchronous AX / AT-SPI round trip (which blocks on the
  target's main loop, so a freeze spikes it). It is host wall time, perturbable by
  scheduling, so less deterministic than a frame trace; a high 2000ms floor keeps
  it false-positive-free. Windows uses the OS `IsHungAppWindow` signal directly
  and has no such caveat.
- **overflow on TUI / Clay** (`gap`): a VT grid clips rather than overflowing, and
  Clay does not expose parent/child geometry in its command stream (and its struct
  layout shifts between versions), so a reliable child-exceeds-parent check is not
  available.
- **content-bug on TUI / ImGui** (`gap`): no semantic label tree to scan for broken
  template output the way a DOM, a Flutter semantics tree, or an a11y tree can be
  scanned.

## Determinism bar

Every oracle keys its finding off structure (element ids, roles, keys, bounds),
never visible text, uses coarse far-apart thresholds, and degrades to silence
when the signal channel is absent. The same seed reproduces the same finding id
on replay across every backend, which is what makes a finding shrinkable and a
regression test stable.
