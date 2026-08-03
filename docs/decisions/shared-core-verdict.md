# Shared native core: no

`plan-simplification.md` step 1.3 said to revisit the idea of embedding one
implementation in every SDK only after the behavior vectors landed, because the
vectors capture most of the benefit at a fraction of the risk. They landed
(`sdk/capture-behavior-v1.json`, nine SDKs wired). This is the verdict.

**Decision: do not build a shared native core. Keep the ports, keep the vectors
as the enforcer.** Revisit only under the conditions at the end of this file.

## What is actually duplicated

The exchange, replay and instrument modules across the eight backend SDKs:

| SDK | lines |
| --- | ---: |
| go | 1,409 |
| dotnet | 1,124 |
| python | 1,238 |
| java | 853 |
| php | 780 |
| node | 749 |
| ruby | 628 |
| rust | 615 |
| total | ~7,400 |

That is the ceiling on what a shared core could remove, out of roughly 160k
production lines: **under 5 percent**, and not all of it, because each port
still needs the language's own boundary (a `RoundTripper`, a `DelegatingHandler`,
a `URLProtocol`, a module prepend) whatever the core is written in.

## Why the answer is no

1. **The vectors already bought the safety.** The reason duplication hurt was
   that one defect had to be found eleven times. Four instances of one class
   landed in a single day. The vectors now pin bounds, header determinism,
   redaction and folding, matching, the divergence marker, trigger tokens and
   encoding, and three of those four defects were proven to fail the vectors
   when reintroduced. The pain the core was meant to cure is treated.
2. **It would break the posture the SDKs are sold on.** Node and Python are
   dependency free today; a native core makes them ship a binary, which is the
   single most common objection to adding an SDK to a production service.
3. **PHP effectively cannot consume it.** A native core needs FFI or an
   extension, and the SDK deliberately stays stdlib only.
4. **Mobile multiplies the build matrix.** Android and iOS would each need per
   architecture builds of the core, adding release surface to two SDKs that
   already carry the full support promise.
5. **The saving is in the wrong place.** Under 5 percent of production code, in
   exchange for a build and distribution problem in every language at once.

## What to do instead, and it is already done

Extend the vectors when a new behavior needs pinning, which costs one JSON edit
plus a small hookup per SDK, and gate every SDK in CI so a regression is caught
on the push that causes it (`sdk/INVENTORY.json`, `docs/decisions/sdk-support.md`, the
`sdk-backend-reference` and `sdk-backend-ports` jobs).

## When to revisit

- if the vectors stop being sufficient, meaning a defect class lands that they
  structurally cannot express
- if the SDK count grows well past eleven, changing the arithmetic
- if the set of shipped SDKs shrinks to languages that can all consume a native
  library cheaply, which today would mean dropping PHP, Ruby, and mobile
  entirely, not demoting them: there is no tier to demote into

None of those hold now.
