# Accessibility audit

ReproIt finds controls that a pointer can operate but a keyboard or assistive technology cannot.
Each confirmed gap maps to a specific WCAG rule and includes the runtime evidence needed to
reproduce it.

You run it with one command:

```sh
reproit debug map accessibility
```

ReproIt refreshes its internal app model before the audit. Your AI agent gets the same data through
`reproit_accessibility` without a model-management step.

## What it tells you

For each screen, it lists every operable control the keyboard / accessibility layer is missing,
what's wrong with it, and where to fix it:

```
accessibility diff for shop (1 screen with gaps)

  Refund review [s_e23a98b3]
    route: /refund
    reach: tap:key:testid:open-refund
    key:testid:finding-row  ->  pointer_only, no_role
        at src/RefundList.tsx:42
```

Each gap is one of four kinds:

| Kind                   | What it means                                                    | WCAG  |
| ---------------------- | ---------------------------------------------------------------- | ----- |
| `pointer_only`         | You can click it with a mouse, but Enter/Space don't activate it | 2.1.1 |
| `keyboard_unreachable` | You can't even Tab to it                                         | 2.1.1 |
| `no_role`              | It works, but exposes no role/name to assistive tech             | 4.1.2 |
| `focus_trap`           | Keyboard focus gets stuck on this screen                         | 2.1.2 |

Every finding is **grounded**: it carries the control's selector, the dimension(s) it fails, a
source `file:line` to fix it, and (where the map knows it) the route and the action path to reach
that screen. So the workflow is:

1. `reproit debug map accessibility` finds the gap and points at the file.
2. You (or your agent) fix the control.
3. `reproit check` confirms the gap is gone.

Filter when you want to focus:

```sh
reproit debug map accessibility --state Refund        # one screen
reproit debug map accessibility --kind pointer_only   # one dimension
reproit debug map accessibility --json                # machine-readable
```

## How it works

ReproIt compares two runtime views of each screen:

- **Graph 1, ground truth:** everything a sighted pointer user can actually operate, found without
  trusting accessibility labels at all (real click handlers, hit-testable widgets, native controls).
- **Graph 2, accessibility:** the subset that's also reachable and operable by keyboard and
  assistive tech (correct role and name, focusable, in the Tab order, activatable by Enter/Space, no
  traps).

An operable element in the first view that fails the corresponding keyboard or accessibility checks
is a gap. The pointer view does not depend on accessibility labels, so missing or malformed
accessibility metadata does not hide controls from the comparison.

## Where it works

The same comparison runs on every platform; only the way graph 1 is gathered differs per framework.

| Platform                      | How graph 1 is found                                                                                     |
| ----------------------------- | -------------------------------------------------------------------------------------------------------- |
| Web / Electron                | Real event listeners (via CDP), native elements, `cursor:pointer`, and a real Tab traversal              |
| Tauri                         | In-page native + cursor + handler inspection (no CDP), structural keyboard check                         |
| Flutter                       | The element / render tree (gesture detectors, hit-testable) vs the semantics tree                        |
| Native (Qt, WPF, AppKit, GTK) | An in-process agent walks the real widget tree and joins it to its accessibility peer by object identity |
| React Native                  | The JS handler tree vs the exported accessibility props                                                  |
| Terminal UIs                  | Keyboard-vs-mouse operability walk over the screen grid                                                  |

## Design notes

This section is for contributors; you don't need it to use the audit.

**The contract.** Every backend emits one extra marker per element keyed by reproit's normal
selector grammar:

```
EXPLORE:GROUNDTRUTH { sig, elements: [{
  id, operable, gestureKind,
  a11y: { rolePresent, namePresent, focusable, inTabOrder, keyboardActivatable }
}], focusTrap }
```

The Rust engine (`map.rs`) re-derives the diff itself (it never trusts the runner) into the stored
`OperabilityGaps`: per-screen counts plus `items: [{selector, kinds}]`, the per-element detail that
makes the report actionable. The CLI view and the MCP tool are pure read-outs of that stored data.

**Capture must be non-destructive.** Measuring whether a control is keyboard-activatable must never
_activate_ it. Pressing Enter/Space (or dispatching a synthetic key) would fire the app's real
handler as a side effect, maybe a navigation or a destructive action, which would pollute the crash
oracle and corrupt exploration. So `keyboardActivatable` is derived from structure: a
native-activating control, or one that carries a real key handler (read via the browser's
`getEventListeners` on web/electron), counts; a focusable click-only element with no key handler is
keyboard-dead, which is exactly the `pointer_only` gap. Other backends read it from the widget tree
the same way, never by activating anything.

**Why not just analyze the source code?** What's actually on screen is undecidable from source
(conditional rendering, runtime state, dynamic handlers); a static graph would describe screens that
never exist. Static analysis is used only for the helpful bits _after_ a gap is found at runtime:
attributing it to a source `file:line`, and optionally seeding routes to guide exploration. It is
never the graph builder.

## Status

Validated live on web against a real app (a `<div role=option tabindex=-1>` operable only through a
delegated click handler): the probe found 5 pointer-only controls that a 60-press Tab traversal
never reached, all confirmed deterministically. The engine contract and the web, Electron, Tauri,
Flutter, native (Qt/WPF/AppKit/GTK), React Native, and TUI emitters are in place, each validated on
a real app for that platform or covered by engine contract tests.
