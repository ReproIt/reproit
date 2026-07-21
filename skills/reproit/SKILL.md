---
name: reproit
description: >-
  Configure reproit or use it to find, reproduce, and fix bugs in an app under
  test. Drives contract discovery, the find, reproduce, localize, fix, and
  prove loop, plus interpretation of repros, oracles, and fault-localization
  output. Trigger on "set up reproit", "find bugs", "this UI is broken",
  "reproduce this crash", "fix the failing check", access-policy checks, or any
  reproit repro id.
---

# Configure and use reproit

reproit drives the app like a user, finds bugs, and hands back a **replayable repro**: a seed +
exact action sequence that fails the same way every run, addressed by a content hash. Your job is
the loop between its commands, not the finding itself.

## The loop

1. **Scan** (the default "what's wrong here"): `reproit scan [target]`. One coverage crawl that
   visits every reachable screen once and reports the STATE-PRESENT bugs simply visible on each
   (overflow / broken content / a11y / choice-anomaly), one finding per (screen x issue). `target`
   is a URL (zero- config against a deployed app) or an alias/node to scope. Deterministic and
   exhaustive per screen, this is the first pass for auditing an app. Reproit maintains and
   refreshes its internal app model automatically; never ask the user to build or refresh a graph.
2. **Fuzz** (the DEEP search): `reproit fuzz [target]`. Combinatorially permutes action sequences to
   provoke the SEQUENCE-dependent bugs (crash / jank / hang / leak) that only appear after the right
   actions in the right order. Each finding prints a content-hash id. All oracles run by default
   (see `references/oracles.md`).
3. **Reproduce before touching code**: `reproit <id>`. Exit codes: `0` pass, `1` fail, `2` flaky,
   `3` stale. Never start fixing a finding you have not confirmed reproduces. If it is flaky (2),
   the bug is a race or a visual flicker, treat the flake itself as the bug, do not retry until
   green.
4. **Localize**: `reproit repro why <id>` ranks suspect files by Ochiai fault localization. Open the
   top-ranked file first. See `references/why.md`.
5. **Fix** the code.
6. **Prove**: re-run `reproit <id>`. `0` means the fix holds. Re-run twice if it was originally
   flaky, to confirm the flake is gone.
7. **Guard**: `reproit keep <id> [--as name]` saves it as a permanent regression guard
   (quarantined/non-blocking until it next passes, then promoted to required). `keep` is not a git
   commit; it writes a local guard.

For video evidence, use the explicit video option:

- `reproit scan --record-video` saves quick audit clips for visible, boxable scan findings into
  `.reproit/recordings/scan/`.
- `reproit <id> --record-video` runs one confirmed fuzz or kept repro and produces the shareable
  evidence video (paced action HUD plus a red box on the bug's effect).

Human-authored reports use a separate workflow. `reproit create` opens an interactive demonstration
session, and `reproit push cap_...` publishes the resulting immutable original after review. Agents
must not invoke `create` because it requires a person at an interactive terminal.

## Configuration authority

When asked to configure ReproIt, act as a contract authoring assistant. Inspect the application and
build an authority ledger before editing configuration. Classify every proposed rule as:

- `declared`: explicit user policy, an application-owned assertion, or an existing test.
- `derived`: a mechanical fact from a route table, schema, middleware, SDK registration, or runtime
  structure. It can support only the fact it directly proves.
- `suggested`: model inference, naming convention, visible copy, or an expected product convention.

Only put declared policy and safely derived mechanical facts in `reproit.yaml`.
Never activate a suggested rule. Present suggested rules for review and ask for policy when the
missing decision would change pass/fail behavior. A user's explicit approval turns that exact
suggestion into a declared rule; do not broaden it.

After the config diff, run `reproit doctor`, execute each narrow contract family, and report
`SATISFIED`, `VIOLATION`, `ABSTAIN`, and uncovered policy separately. ReproIt, not the model, owns
the verdict. See `references/configuration.md` for the complete authoring workflow, activation
rules, output format, and browser route-access example.

## Rules

- A repro is seed + action sequence, identical across machines. Trust the id, not your memory of the
  steps.
- Confirm with `check` before fixing and after fixing. No exceptions.
- Screens are keyed **structurally** (roles + dev keys, text excluded), so the graph is
  locale-invariant. Do not assume a screen changed just because text did.
- `reproit repros` lists saved guards + last status. `reproit watch <id>` opens the recorded video
  for a finding (make one with `reproit <id> --record-video`).
- `reproit repro simplify <id> --to '[...]'` adopts a shorter action sequence that reproit verifies
  still reproduces; `reproit repro why <id>` localizes.

## Going deeper

- Oracle catalog and how to read each failure: `references/oracles.md`
- Fault localization (`why`) interpretation: `references/why.md`
- Reproducing a real production crash from the cloud: `references/cloud.md`
- Authoring authoritative configuration and route access: `references/configuration.md`
- Authoring multi-user / scripted test journeys: use the `reproit-journeys` skill instead, that is a
  different task.
