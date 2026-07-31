# SDK tiers

Twenty SDK directories ship in this repository. Each one is a permanent parity
promise, a CI job, a release gate, and a place for the next divergence defect
to hide. This file says which promise each one actually carries, so the promise
is a mechanism rather than a hope.

The tier is declared in exactly one place, `sdk/TIERS.json`. `sdk/check-tiers.py`
fails when the declaration drifts from reality, including when a core SDK is not
named in `.github/workflows/ci.yml`.

## The tiers

| tier | promise | gating |
| --- | --- | --- |
| core | behavior parity is asserted on every push; a defect blocks a release | its own suite, the shared behavior vectors, and for Rust the feature gated hermetic path, all in `sdk-backend-core` |
| community | must pass the shared behavior vectors; a regression is reported, not merge blocking | the vectors run inside each SDK's own suite where one exists |
| unmaintained | shipped, not gated, no parity promise | nothing; listed so the cost stays visible |

`unmaintained` is deliberately not a euphemism for deleted. These SDKs exist and
cost something to carry, and naming them is how that cost stays countable.

## Why these four are core

Not usage, because there is none to measure: every backend SDK is version
`0.0.0`, marked private or unpublished, and absent from
`validation/release/package-platform-sdks.sh`. Choosing on usage signals would
be choosing on noise.

The basis is the product thesis instead. Hermetic backend replay is the thing
the roadmap calls the company, and Node, Rust, Python and Go cover the majority
of backend services a first customer is likely to run. Node and Rust are also
the reference implementations the other ports were written against, so a defect
in either propagates outward.

## What this changed, and why it was worth doing

Before this file, the gating was inverted from the thesis. The four SDKs the
product depends on had exactly one shared conformance test in CI
(`sdk/test/backend_batch_test.js`, which samples batch shape) and no suite of
their own, while eight UI SDKs each had a dedicated job. The behavior vectors
added in `plan-simplification.md` step 1.1 ran in nine SDKs locally and in zero
CI jobs.

Separately, `cargo clippy --workspace --all-targets` and `cargo test --workspace`
both build default features only, so `reproit-backend-rs`'s `instrument`
feature, which carries the entire hermetic capture and replay path, was never
compiled by CI at all. A feature CI never builds is a feature nothing gates.

## Changing a tier

Edit `sdk/TIERS.json` and nothing else. If you promote an SDK to core,
`check-tiers.py` will fail until a CI job names it, which is the intended
order: the promise cannot exist before the mechanism.
