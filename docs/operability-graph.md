# The accessibility audit

reproit can tell you which controls in your app a **mouse user can operate but a
keyboard or screen-reader user cannot**. Those gaps are real accessibility bugs
(they map to specific WCAG rules), and they're the kind a static linter can't
find, because they only show up when you actually drive the app.

You run it with one command:

```sh
reproit map accessibility
```

This reads the app graph you already built with `reproit map`, so build the map
first. (Your AI agent can get the same data over MCP with the
`reproit_accessibility` tool.)

## What it tells you

For each screen, it lists every operable control the keyboard / accessibility
layer is missing, what's wrong with it, and where to fix it:

```
accessibility diff for shop (1 screen with gaps)

  Refund review [s_e23a98b3]
    route: /refund
    reach: tap:key:testid:open-refund
    key:testid:finding-row  ->  pointer_only, no_role
        at src/RefundList.tsx:42
```

Each gap is one of four kinds:

| Kind | What it means | WCAG |
|---|---|---|
| `pointer_only` | You can click it with a mouse, but Enter/Space don't activate it | 2.1.1 |
| `keyboard_unreachable` | You can't even Tab to it | 2.1.1 |
| `no_role` | It works, but exposes no role/name to assistive tech | 4.1.2 |
| `focus_trap` | Keyboard focus gets stuck on this screen | 2.1.2 |

Every finding is **grounded**: it carries the control's selector, the
dimension(s) it fails, a source `file:line` to fix it, and (where the map knows
it) the route and the action path to reach that screen. So the workflow is:

1. `reproit map accessibility` finds the gap and points at the file.
2. You (or your agent) fix the control.
3. `reproit check` confirms the gap is gone.

Filter when you want to focus:

```sh
reproit map accessibility --state Refund        # one screen
reproit map accessibility --kind pointer_only   # one dimension
reproit map accessibility --json                # machine-readable
```

## How it works

The trick is to build **two pictures of your app and compare them**:

- **Graph 1, ground truth:** everything a sighted pointer user can actually
  operate, found without trusting accessibility labels at all (real click
  handlers, hit-testable widgets, native controls).
- **Graph 2, accessibility:** the subset that's also reachable and operable by
  keyboard and assistive tech (correct role and name, focusable, in the Tab
  order, activatable by Enter/Space, no traps).

Whatever is in graph 1 but missing from graph 2 is an accessibility gap. That's
the whole idea, and it's why this finds problems a linter can't: a linter checks
a static snapshot, while reproit *compares what's operable two different ways*.

This also has a useful side effect. Because graph 1 doesn't depend on
accessibility labels, the audit works even on apps with terrible accessibility,
which are exactly the apps that most need it. And it doubles as a detector for
"an AI built a control that looks clickable but isn't wired up."

## Where it works

The same comparison runs on every platform; only the way graph 1 is gathered
differs per framework.

| Platform | How graph 1 is found |
|---|---|
| Web / Electron | Real event listeners (via CDP), native elements, `cursor:pointer`, and a real Tab traversal |
| Tauri | In-page native + cursor + handler inspection (no CDP), structural keyboard check |
| Flutter | The element / render tree (gesture detectors, hit-testable) vs the semantics tree |
| Native (Qt, WPF, AppKit, GTK) | An in-process agent walks the real widget tree and joins it to its accessibility peer by object identity |
| React Native | The JS handler tree vs the exported accessibility props |
| Terminal UIs | Unlabeled-region detection from the screen grid; optional keyboard-vs-mouse walk |
| Dear ImGui / Clay | The per-frame widget list (accessibility is empty here by construction, so the whole surface is the gap) |

## Design notes

This section is for contributors; you don't need it to use the audit.

**The contract.** Every backend emits one extra marker per element keyed by
reproit's normal selector grammar:

```
EXPLORE:GROUNDTRUTH { sig, elements: [{
  id, operable, gestureKind,
  a11y: { rolePresent, namePresent, focusable, inTabOrder, keyboardActivatable }
}], focusTrap }
```

The Rust engine (`map.rs`) re-derives the diff itself (it never trusts the
runner) into the stored `OperabilityGaps`: per-screen counts plus
`items: [{selector, kinds}]`, the per-element detail that makes the report
actionable. The CLI view and the MCP tool are pure read-outs of that stored data.

**Capture must be non-destructive.** Measuring whether a control is
keyboard-activatable must never *activate* it. Pressing Enter/Space (or
dispatching a synthetic key) would fire the app's real handler as a side effect,
maybe a navigation or a destructive action, which would pollute the crash oracle
and corrupt exploration. So `keyboardActivatable` is derived from structure: a
native-activating control, or one that carries a real key handler (read via the
browser's `getEventListeners` on web/electron), counts; a focusable click-only
element with no key handler is keyboard-dead, which is exactly the
`pointer_only` gap. Other backends read it from the widget tree the same way,
never by activating anything.

**Why not just analyze the source code?** What's actually on screen is undecidable
from source (conditional rendering, runtime state, dynamic handlers); a static
graph would describe screens that never exist. Static analysis is used only for
the helpful bits *after* a gap is found at runtime: attributing it to a source
`file:line`, and optionally seeding routes to guide exploration. It is never the
graph builder.

## Status

Validated live on web against a real app (a `<div role=option tabindex=-1>`
operable only through a delegated click handler): the probe found 5 pointer-only
controls that a 60-press Tab traversal never reached, all confirmed
deterministically. The engine contract and the web, Electron, Tauri, Flutter,
native (Qt/WPF/AppKit/GTK), React Native, TUI, and ImGui/Clay emitters are in
place, each validated on a real app for that platform or covered by engine
contract tests.
