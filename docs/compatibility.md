# Reproit 1.x compatibility

Reproit separates release availability from Stable compatibility.

- Stable targets are covered by the 1.x compatibility promise and pass native,
  independent-application, affected-versus-fixed, minimization, and clean-corpus
  gates.
- Preview targets ship for evaluation and pass their named owned fixture gates,
  but remain outside the field-compatibility promise.
- Experimental targets are explicitly invoked specialist surfaces.

`validation/support-manifest.json` is the canonical atomic contract.
`validation/compatibility/check.py` validates it and generates the complete
[qualification status](../validation/compatibility/STATUS.md). Documentation
cannot promote a target.

## Stable qualification

An atomic target becomes Stable only when the exact release commit proves:

1. Every owned native fixture gate passed in required CI.
2. At least two independent real applications have pinned affected and fixed
   revisions.
3. Each affected revision reproduced the exact identity in three clean runs.
4. Each fixed revision reached the observation point without that identity in
   three clean runs.
5. Minimization preserved the exact identity.
6. Neighboring legal behavior remained functional.
7. The clean and adversarial corpora produced no confirmed false positives.
8. Evidence retained the commit, architecture, runtime, reset, cleanup, and
   artifact digests.
9. Independent affected and fixed application campaigns used distinct campaign
   identities and the exact pinned revisions.
10. The result reached every cumulative recall stage: observed, captured,
    eligible, executed, exact, minimized, fixed, and guarded.

Broad families never become Stable from one narrow fixture. Firefox and WebKit,
mobile operating systems, Windows toolkits, Linux toolkits, Electron hosts, and
Tauri webviews qualify as atomic targets before a family-level claim can roll
them up.

## Current status

Chromium web is Stable. All exact Preview scopes and their remaining promotion
blockers are generated in the
[atomic status](../validation/compatibility/STATUS.md).

The public-issue ledger in `docs/issue-reproduction-audit.md` remains negative
evidence. Reviewed reports do not become field cases until the application was
executed, the exact identity reproduced, the fixed control passed, and the
retained artifacts validated.

## Host prerequisites

- Node.js 18 or later for the web runner.
- Current stable Rust for source builds.
- The pinned platform SDKs, drivers, and simulators in
  `validation/native/toolchains.json`.
- The actionable repairs named by `reproit doctor` for the selected target.
- PostgreSQL for Reproit Cloud. SQLite and MySQL are not supported Cloud stores.

Release archives are built and installer-smoked for macOS arm64 and x86_64,
Linux arm64 and x86_64, and Windows arm64 and x86_64. Native behavior evidence
records the exact architecture it exercised. Architecture-independent APIs may
use one documented behavior architecture only when archive and installer gates
still cover every shipped architecture.
