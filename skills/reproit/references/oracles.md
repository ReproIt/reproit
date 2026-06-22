# Oracles

Oracles are the checks reproit runs while fuzzing. All are on by default. Narrow
with `--only` or exclude with `--no`:

```sh
reproit fuzz --only crash,jank        # just these
reproit fuzz --no visual,i18n         # everything except these
```

| Oracle | Catches | When a finding means |
|---|---|---|
| `crash` | Unhandled exceptions, native crashes, fatal asserts | A code path throws. Read the stack in the repro. |
| `jank` | Dropped frames (build or raster over the 16.7ms/60Hz budget) | A transition stutters. Profile the janky transition, not the whole app. |
| `leak` | Growing retained memory across repeated states | A state isn't releasing. Look at what the repeated action allocates. |
| `visual` | Pixel regression vs the committed baseline (tolerance-banded) | A screen rendered differently than the approved baseline. |
| `flicker` | Transient render glitch *within* a run: a frame that diverges then snaps back | A flash/unstyled frame/layout jump during a transition. Run `reproit check <id> --flicker`. No baseline needed. |
| `divergence` | Same flow behaving differently across engines/targets | Cross-engine bug (e.g. Chromium-fine, WebKit-broken). |
| `a11y` | Missing labels, contrast, focus-order problems | An accessibility defect on the reached screen. |
| `i18n` | Overflow, clipping, RTL breakage under other locales | Run with `--locale de,ar,ja` to surface these. |
| `overflow` | DOM/layout overflow: content clipped or overflowing its container/viewport (web) | A child wider than its parent, text truncated by `text-overflow`, or a horizontal scroll appearing. Deterministic structural measurement (not a pixel diff). |
| `content-bug` | A rendered label leaking a stringify/template artifact (web): `[object Object]`, a bare `undefined`/`null`/`NaN`, or an unrendered `{{...}}`/`${...}` | A binding/serialization bug put a raw value on screen. Built-in DOM/label scan (no custom invariant needed); deterministic, addressed by the element's stable key. |
| `hang` | A synchronous main-thread freeze (web): an action whose handler stops the app making progress past the hang floor | The app froze for the duration (an unbounded/very long synchronous task). Deterministic, keyed off the browser's Long Tasks trace, bucketed so timing jitter can't flip the verdict. |

## Visual oracle specifics

`--visual` (or the `visual` oracle) diffs the current capture against a
committed baseline with a per-pixel tolerance (absorbs antialiasing) and a
per-image percent threshold (ignores trivial diffs). Determinism is handled
upstream (pinned status bar, seeded data).

- First run / new screen: reported `NEW` (no baseline yet).
- Accept the current capture as the baseline: `reproit fuzz --visual` then the
  visual update flow (do this only when the new look is intended).
- A `visual` failure is not a crash: decide whether the pixel change is a
  regression or an intended redesign before touching code.

## Cross-target divergence

`--target ios,android` or `--target chromium,firefox,webkit` runs each and
diffs for divergence. Use this when a bug is reported on one platform only.
