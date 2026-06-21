# Agent-verification loop (codex + reproit)

A self-contained harness that proves reproit product goal #2:

> Your agent writes the fix; reproit proves the UI works.

It runs the full inner loop against a tiny buggy web app:

```
reproit fuzz            find the bug, save a replayable repro (content-hash id)
reproit check <id>      reproduce it deterministically            -> FAIL (exit 1)
codex exec              the coding agent writes the fix
reproit check <id>      replay the SAME repro on the fixed code    -> PASS (exit 0)
```

A green `check` is deterministic, so the agent does not guess that it fixed the
bug, reproit proves it.

## The bug

`app/app.js` is a counter app. The "Reset" button's click handler calls
`state.reset()`, but `state` has no `reset` method, so every Reset click throws
an uncaught `TypeError`. reproit's **crash oracle** catches it via the page's
`pageerror` event. The bug is deterministic: it fires on every Reset click.

The fix is one line: add a `reset() { this.count = 0; }` method to `state`.

## Run it

From anywhere:

```sh
cd reproit-cli
cargo build                       # once: builds target/debug/reproit
cd runners/web && npm install     # once: Playwright deps (Chromium already installed)

examples/agent-loop/run-loop.sh
```

The script:

1. serves `app/` with `python3 -m http.server` (port 8731),
2. runs the four loop stages, printing a per-stage `PASS`/`FAIL` line,
3. restores the buggy fixture on exit so the demo is repeatable.

### Options (env vars)

| var | effect |
|---|---|
| `FIXER=auto` (default) | use real codex if authenticated, else the scripted fixer |
| `FIXER=codex` | force the real `codex exec` path |
| `FIXER=scripted` | force the scripted fallback (applies the known fix programmatically) |
| `KEEP_FIX=1` | leave the fix in place instead of restoring the buggy fixture |
| `PORT=8731` | fixture server port |

The fixer path actually taken is printed in the summary, so it is always clear
whether **real codex** or the **scripted fallback** wrote the fix.

## What it proves

- reproit's oracle finds a *real* UI crash with no test written for it.
- The finding is a **replayable repro** addressed by a content hash, so
  `check <id>` reproduces the identical failure deterministically (exit 1).
- After the agent's fix, replaying the **same repro** flips to PASS (exit 0).
  The exit codes are the CI contract (0 pass / 1 fail / 2 flaky / 3 stale).

## Files

- `app/index.html`, `app/app.js` - the buggy fixture (static, no build).
- `reproit.yaml` - web-playwright config so map/fuzz/check share the fixture URL
  (zero-config `fuzz <url>` does not persist a config, so a follow-up `check`
  could not resolve the app; an explicit config makes the loop replayable).
- `run-loop.sh` - the harness.

## Notes

- This uses an explicit `reproit.yaml` rather than zero-config `fuzz <url>` only
  because the loop needs `check <id>` to resolve the app *after* fuzz. Zero-config
  is great for a one-shot `fuzz https://app.com`; the loop wants a persisted config.
- The harness never modifies reproit source or runs git. The only file it edits
  is `app/app.js`, and it restores it on exit (unless `KEEP_FIX=1`).
