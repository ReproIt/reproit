# Reproducing a production crash from the cloud

The headline cloud use case: a real user session crashed in production, the SDK
captured it, and you replay it **locally and deterministically**.

```sh
reproit login
reproit cloud buckets --app <app>      # browse what production reported
reproit <bkt>
```

The direct bucket command pulls the captured session (seed + actions + environment)
and runs it through the same deterministic engine as a local repro, so a production
"cannot reproduce" becomes a repro you can `check`, `why`, and fix exactly like
any other.

## Loop

1. `reproit <bkt>` to pull and replay the crash.
2. It lands as a local repro id, from here it is the normal loop: `check` to
   confirm, `why` to localize, fix, `check` to prove.
3. `reproit cloud blast-radius` shows how many sessions/users a given crash
   signature affects, use it to prioritize which bucket to reproduce first.

## Notes

- Cloud is the paid/proprietary tier; `login` is required.
- The same `reproit` binary runs locally and in the cloud fleet, so a repro
  that passes locally passes in CI and vice versa.
