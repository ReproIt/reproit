# In-process operability agents (graph 1)

These are **in-process** agents: they run INSIDE the target app and read the
real native widget tree + wired handlers (graph 1 = the operability ground
truth), then join that, by object identity, against the toolkit's accessibility
projection (graph 2). The diff is the operability/accessibility gap the engine
scores.

Contrast with the EXTERNAL runners one directory up:
`runners/macos-ax.swift` and `runners/linux-atspi.py` read the a11y tree of
*another* process. That is **graph 2 only**: they can see what the app published
to accessibility, but not the ground truth of what is actually wired up. An
in-process agent sees both, so it can detect an element that is genuinely
operable (a click handler is attached) yet missing from / mislabeled in the a11y
tree. That element is invisible to the external runner by construction.

## The marker (single-line JSON, parsed by `crates/reproit/src/model/map.rs`)

    EXPLORE:GROUNDTRUTH {"sig":"<sig>","focusTrap":false,"elements":[
      {"id":"<id>","operable":true,"gestureKind":"button",
       "a11y":{"rolePresent":true,"namePresent":false,"focusable":false,
               "inTabOrder":false,"keyboardActivatable":false}}]}

Engine rule (`gaps_from_groundtruth`): an `operable:true` element is a gap when
any of `keyboardActivatable` / `inTabOrder` / `rolePresent` is `false`
(missing a11y dims default to `true`, so only confirmed failures count).
- `keyboardActivatable:false` -> `pointer_only`
- `inTabOrder:false`          -> `keyboard_unreachable`
- `rolePresent:false`         -> `no_role`

`id` is the developer identifier (`accessibilityIdentifier` / `objectName` /
GtkWidget name), else a structural `role:<role>#<idx>` fallback, identical to the
other runners.

## Per-toolkit status

| Toolkit | Source | Toolchain on this machine | Built | Run | Gap detected |
|---|---|---|---|---|---|
| AppKit (macOS) | `appkit-agent/main.swift` | Swift 6.2.3 present | YES | YES | YES |
| Qt (C++) | `qt-agent/qt_agent.cpp` | absent on host; built in Docker (Debian, Qt 6.8.2) | YES | YES | YES |
| GTK (C) | `gtk-agent/gtk_agent.c` | absent on host; built in Docker (Debian, GTK 4.18.6) | YES | YES | YES |
| WPF (.NET) | `wpf-agent/Program.cs` | dotnet 8.0.422 on the QEMU Windows 11 VM | YES | YES | YES |

### AppKit — built + run + verified

    ./appkit-agent/build-and-run.sh          # swiftc -O main.swift && run, headless

Builds a window IN-PROCESS (no window server interaction; never enters the run
loop) with three controls and walks them:

- **realButton** — a real `NSButton` (target-action). Operable AND full a11y
  (button role, focusable, in tab order, keyboard-activatable) -> **OK**.
- **fakeButton** — a custom `NSView` (`FakeButton`) with an `NSClickGesture`
  recognizer + handler and NO accessibility role. Operable in graph 1 but
  `rolePresent:false`, `inTabOrder:false`, `keyboardActivatable:false` ->
  **GAP(NO_ROLE, KEYBOARD_UNREACHABLE, POINTER_ONLY)**. This is the motivating
  finding: an external AX runner cannot see it as a button at all.
- **goodCustom** — a custom `NSView` that DOES adopt `.button` role + is
  focusable + keyboard-activatable -> **OK** (proves the agent does not
  false-positive on every custom view).

Verified end to end: the agent's real `EXPLORE:GROUNDTRUTH` line is embedded in
`crates/reproit/src/model/map.rs`
(`appkit_in_process_agent_groundtruth_detects_fake_button_gap`) and asserted to
parse to `no_role=1, keyboard_unreachable=1, pointer_only=1` by the engine.

**Key AppKit gotcha found while building this:** for standard `NSControl`s the
accessibility role lives on the control's **cell**, not the view.
`NSButton.accessibilityRole()` returns `AXUnknown` while
`NSButton.cell.accessibilityRole()` returns `AXButton`. An in-process graph-2
reader MUST consult the cell for cell-backed controls (`resolvedAXRole` in
`main.swift`), or it would wrongly flag every standard button as role-less.

### Qt / GTK

Both sources implement the identical design (graph 1 from the live object tree +
wired signals/gestures, graph 2 from `QAccessibleInterface` / GtkAccessible,
joined by object identity) and carry a `#ifdef …_DEMO_MAIN` proof window
mirroring the AppKit one (real button + fake button + accessible control).

**Qt (Qt 6.8.2):** the agent emits a marker whose signature (`3854aea0`) matches
the AppKit agent's (identical three-control descriptor). `key:fakeButton` is
`operable:true, rolePresent:false` and fails all three a11y dims
(`GAP(NO_ROLE, KEYBOARD_UNREACHABLE, POINTER_ONLY)`); `key:realButton` and
`key:goodCustom` are clean.

**GTK (GTK 4.18.6):** signature `44602d5a`, deterministic across runs. The fake
button (a GtkBox with a GtkGestureClick + handler) comes out `operable:true,
rolePresent:false` = `GAP(NO_ROLE, KEYBOARD_UNREACHABLE, POINTER_ONLY)`; the real
GtkButton and the good button are clean. GTK4 additionally surfaces the
application window's built-in click gesture (`role:group#0`, a focusless operable
element, so also keyboard-unreachable/pointer-only) and the buttons' inner
GtkLabel children (`operable:false`, never gaps) — artifacts of GTK4's
widget/scene model, not false positives on the controls.

**Minimal source fixes made to compile against Qt 6.8 (design intact):**
- `QObject::isSignalConnected` is `protected`; reach it through a thin
  same-layout `SignalConnectedAccessor` cast instead of calling it directly.
- `FakeButton` carries no `Q_OBJECT` (no moc step in the single-file build), so
  detect the custom-clickable subclass with `dynamic_cast` rather than
  `qobject_cast`.
- Added `#include <QMouseEvent>` (used by the `mousePressEvent` override).

The GTK source compiled unmodified against GTK4.

Both captured `EXPLORE:GROUNDTRUTH` lines are embedded VERBATIM in
`crates/reproit/src/model/map.rs` as engine contract tests
(`qt_in_process_agent_groundtruth_detects_fake_button_gap`,
`gtk_in_process_agent_groundtruth_detects_fake_button_gap`) and asserted to parse
to the expected gap counts — the same proof pattern as the AppKit and WPF tests.

## Windows: canonical backend vs. hand-maintained ports

The canonical Windows desktop backend is the in-process Rust runner
`crates/reproit/src/backends/uia.rs` (UI Automation), which REUSES the canonical
signature/oracle core directly, so it cannot drift from the engine. The former
`runners/test_signature.py` parity gate, run against the WPF agent on the QEMU
Windows 11 VM (the WPF row above), was retired for exactly that reason: an
in-process Rust backend has nothing to keep in sync (see the `signature-parity`
job note in `.github/workflows/ci.yml`).

Any remaining hand-maintained Windows port (the WPF `.NET` operability agent, the
`sdk/reproit-windows` SDK, or a VM-run script) is a SECONDARY artifact: it must be
parity-checked against `uia.rs` or retired. Once the ARM64 binary ships
(`aarch64-pc-windows-msvc`, now in the release matrix and cargo-checked in
`ci.yml`'s `windows-build` job), the native x64 Rust backend covers Windows on
both architectures, so a drifted VM Python port earns its keep only by proving a
gap the Rust backend cannot, and otherwise should be dropped rather than kept
limping.
