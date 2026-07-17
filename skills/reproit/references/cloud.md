# Reproducing a production crash from the cloud

The headline cloud use case: a real user session crashed in production, the SDK captured it, and you
replay it **locally and deterministically**.

```sh
reproit login
reproit bugs                            # browse what production reported
reproit <bkt>
```

The direct bucket command pulls the captured session (seed + actions + environment) and runs it
through the same deterministic engine as a local repro, so a production "cannot reproduce" becomes a
repro you can `check`, `why`, and fix exactly like any other.

## Loop

1. `reproit <bkt>` to pull and replay the crash.
2. It lands as a local repro id, from here it is the normal loop: `check` to confirm, `why` to
   localize, fix, `check` to prove.
3. `reproit bugs` ranks confirmed bugs by impact so you know what to fix first.
4. `reproit triage <bkt>` reads or updates its workflow state.
5. `reproit timeline <bkt>` shows the bug history, and `reproit resolution-events` shows resolution
   evidence across the selected project.

## Notes

- Hosted Cloud requires `login`; self-hosted Cloud uses the same CLI contract.
- Bucket ids resolve across every project the signed-in account may access.
