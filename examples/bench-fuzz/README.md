# bench-fuzz: autonomous-fuzz recall benchmark (delegated-click SPA)

A self-contained benchmark that **measures reproit's autonomous-fuzz recall** and
validates the runner's keyed-pointer-operable **coverage fix** end to end. It is
the concrete metric for product goal #3:

> Point it at an app, get real repros.

The app under test is the hard case for a UI crawler: a multi-view SPA whose every
control is a non-interactive `<div role="option" tabindex="-1" data-testid="...">`
row, made operable by **one document-level delegated click listener**. There is no
native `<button>` or `<a>` anywhere. Before the coverage fix, reproit's explorer
tapped only elements matching its `interactive()` grammar (native controls,
`onclick`, `tabindex>=0`), so `tabindex=-1` delegated `<div>`s were never tapped:
the whole app collapsed to **~1 state / 0 transitions** and almost every bug was
unreachable.

## What the fix does (the thing being validated)

`runners/web/runner.mjs` now adds **keyed pointer-operable** controls to the
fuzzer's candidate set: an element with `cursor:pointer` (that introduces it, not
inherited), or an ARIA-interactive role (`option`, `menuitem`, ...), or a
focusable `tabindex>=0`, that `interactive()` would otherwise drop. Only **keyed**
ones are added (a stable `data-testid`/`id`/`name`), so the canonical signature
and existing `role:<role>#<idx>` selectors are untouched and a repro can address
them. This is the `pointerOperable(el)` predicate around `runner.mjs` line ~733
and the `extraTaps` append around line ~857.

## The corpus

`app/index.html` + `app/app.js`, static, no build step. Six views, all reached by
tapping delegated `<div role=option tabindex=-1 data-testid=...>` rows wired
through the single `document.addEventListener('click', ...)` in `app.js`:

| View | testid to reach it | notes |
|---|---|---|
| Home (menu) | (initial) | five nav rows |
| Profile | `nav-profile` | edit / save / cross-link to notifications |
| Notifications | `nav-notifications` | email / push / icon control / cross-link to profile |
| Appearance | `nav-appearance` | **no back, no nav** (the dead end) |
| About | `nav-about` | credits / cross-link; runs a locale-sensitive formatter |
| Danger zone | `nav-danger` | delete account / cross-link |

Profile / Notifications / About / Danger each have a real outgoing **action** edge
(a cross-navigation row), so the **only** view with no outgoing action edge is
**Appearance**, which makes the dead-end finding land squarely on it.

## The 5 seeded bugs

Each is deterministic and maps to a reproit oracle that fires **by default** (no
custom config), except bug 5 which needs `--locale de`.

| # | Oracle (invariant) | Trigger | Exact signature reproit reports |
|---|---|---|---|
| 1 | crash (`no-exception`) | Danger zone → tap **Delete account** (`danger-delete`): calls `account.purge()`, no such method | `account.purge is not a function` |
| 2 | crash (`no-exception`) | Profile → tap **Save profile** (`profile-save`): `document.querySelector('#nonexistent-form')` is `null`, then `.serialize()` | `Cannot read properties of null (reading 'serialize')` |
| 3 | dead-end (`no-dead-end`) | Navigate to **Appearance** (`nav-appearance`): the view renders no back row and no nav, so it has no outgoing action edge | `state … is a dead end … [Appearance, Settings, Theme follows the system setting.]` |
| 4 | a11y / operability (`all-labeled`) | **Notifications** view contains an icon-only control (`notif-sound`): glyph painted via a CSS `::before` (no DOM text node), no `aria-label`/`title` → no accessible name | `state … has 1 unlabeled tappable(s) … [Notifications, …]` |
| 5 | i18n (cross-locale diff) | Navigate to **About** (`nav-about`) **under German**: `intl.formatDE()` is undefined → throws only when `navigator.language` starts with `de` | `intl.formatDE is not a function … only in: de` |

Bugs 1, 2 are distinct crash signatures (deduped separately by `--all`). Bug 3's
finding is the graph oracle, bug 4 the operability oracle, bug 5 surfaces in the
`--locale en,de` cross-locale diff as a locale-specific finding.

## Run it

```sh
cd reproit-cli
cargo build                       # once: target/debug/reproit
cd runners/web && npm install     # once: Playwright deps (Chromium already present)

examples/bench-fuzz/run-recall.sh        # both arms (WITH then WITHOUT), prints the A/B table
ARM=with    examples/bench-fuzz/run-recall.sh   # only the WITH arm (no stash)
ARM=without examples/bench-fuzz/run-recall.sh   # only the WITHOUT arm (stash/pop)
```

The harness:

1. serves `app/` with `python3 -m http.server` (port 8741),
2. for each arm, runs `reproit fuzz --all --locale en,de --yes`,
3. computes **states mapped** (distinct `EXPLORE:STATE` signatures across the
   per-seed walks) and **recall** (how many of the 5 seeded bugs appear in the run
   artifacts),
4. for the WITHOUT arm, temporarily `git stash push -- runners/web/runner.mjs`,
   runs, then `git stash pop` to restore the (pre-existing, uncommitted) fix. A
   trap restores it even on error. It never git-commits.

Exact `reproit` invocation per arm:

```sh
REPROIT_WEB_RUNNER_DIR=<repo>/runners/web APP_URL=http://localhost:8741 \
  reproit fuzz --all --locale en,de --yes
```

## Measured results

Real numbers from this machine (Chromium, 3 seeds per arm). Raw output is in
`/tmp/benchfuzz-*.log`; run artifacts under `examples/bench-fuzz/.reproit/runs/`.

| Arm | States mapped | Recall | Bugs found |
|---|---|---|---|
| **WITHOUT** coverage fix | **1** | **0 / 5 (0%)** | none of the 5 (only the degenerate home-state dead-end, an artifact of the explorer being unable to operate any control) |
| **WITH** coverage fix | **6** | **5 / 5 (100%)** | 1 crash-purge, 2 crash-null, 3 dead-end-Appearance, 4 unlabeled-tappable, 5 i18n-formatDE |

**Goal #3 metric: recall WITH the fix = 5/5 (100%).**

### Why WITHOUT collapses

Without the fix the explorer reaches **1 state** (Home) and records **0 edges**: it
never taps a delegated `<div>`, so it never leaves Home. The only finding is the
graph oracle flagging Home itself as a dead end (no outgoing edge), because every
control on the page is, to the pre-fix explorer, untappable. That is not one of the
5 seeded reachable bugs, so recall is 0/5. This is the exact failure mode the fix
was written to remove.

### Why WITH recovers

With the fix the explorer taps the keyed delegated rows, maps all **6 views** (Home,
Profile, Notifications, Appearance, About, Danger zone), and exercises the actions
inside each, so all five oracles fire on the views they live in.

## Honesty notes / known limits

- **States count is the union of distinct `EXPLORE:STATE` signatures** across the 3
  seed walks, not a single seed's count (a single seed may explore 5 or 6 of the 6
  depending on its random walk; the union is the honest coverage).
- **There is no dedicated DOM overflow/clip oracle** in the web runner today, so the
  "i18n" bug is modeled as a **locale-specific crash** surfaced by the cross-locale
  diff (`--locale en,de`), not a layout-overflow detection. That is the i18n signal
  reproit actually computes (a finding present in some locales but not all), so the
  bug is caught by a real oracle rather than a fabricated one.
- **Fuzzing is seed-driven**, so the per-seed subset of dead-ends/findings varies
  run to run; recall is measured over the union of all seeds' artifacts, which is
  stable at 5/5 WITH the fix in repeated runs here.

## Files

- `app/index.html`, `app/app.js` — the delegated-click SPA fixture (static, no build).
- `reproit.yaml` — web-playwright config (same shape as `examples/agent-loop`).
- `run-recall.sh` — the recall + A/B harness (the only thing that stashes
  `runner.mjs`, and it always restores it).
