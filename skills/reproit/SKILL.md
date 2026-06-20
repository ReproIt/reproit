---
name: reproit
description: >-
  Use when finding, reproducing, or fixing UI bugs in an app under test with
  reproit. Drives the find -> reproduce -> localize -> fix -> prove loop and
  knows how to read repros, oracles, and fault-localization output. Trigger on
  "find bugs", "this UI is broken", "reproduce this crash", "fix the failing
  check", or any reproit repro id.
---

# Fixing bugs with reproit

reproit drives the app like a user, finds bugs, and hands back a **replayable
repro**: a seed + exact action sequence that fails the same way every run,
addressed by a content hash. Your job is the loop between its commands, not the
finding itself.

## The loop

1. **Map** (once, or when the app changes): `reproit map`. It detects the
   platform, scaffolds config on first run, and crawls the running app into the
   structural graph. Skip if a map already exists.
2. **Find**: `reproit fuzz [target]`. Each finding prints a content-hash id.
   `target` concentrates the hunt on a screen/flow; omit it to fuzz the whole
   app. All oracles run by default (see `references/oracles.md`).
3. **Reproduce before touching code**: `reproit check <id>`. Exit codes:
   `0` pass, `1` fail, `2` flaky, `3` stale. Never start fixing a finding you
   have not confirmed reproduces. If it is flaky (2), the bug is a race or a
   visual flicker, treat the flake itself as the bug, do not retry until green.
4. **Localize**: `reproit why <id>` ranks suspect files by Ochiai fault
   localization. Open the top-ranked file first. See `references/why.md`.
5. **Fix** the code.
6. **Prove**: re-run `reproit check <id>`. `0` means the fix holds. Re-run twice
   if it was originally flaky, to confirm the flake is gone.
7. **Guard**: `reproit keep <id> [--as name]` saves it as a permanent
   regression guard (quarantined/non-blocking until it next passes, then
   promoted to required). `keep` is not a git commit; it writes a local guard.

## Rules

- A repro is seed + action sequence, identical across machines. Trust the id,
  not your memory of the steps.
- Confirm with `check` before fixing and after fixing. No exceptions.
- Screens are keyed **structurally** (roles + dev keys, text excluded), so the
  graph is locale-invariant. Do not assume a screen changed just because text
  did.
- `reproit repros` lists saved guards + last status. `reproit watch <id>`
  opens the recorded video for a finding.

## Going deeper

- Oracle catalog and how to read each failure: `references/oracles.md`
- Fault localization (`why`) interpretation: `references/why.md`
- Reproducing a real production crash from the cloud: `references/cloud.md`
- Authoring multi-user / scripted test journeys: use the `reproit-journeys`
  skill instead, that is a different task.
