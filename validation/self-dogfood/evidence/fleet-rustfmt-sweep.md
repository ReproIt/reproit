# Rustfmt sweep after the 2026-08-01 leveling fleet

The flaky-CI wedge branch landed `test_trigger.rs` and touched `verify.rs`,
`check.rs`, and `backend_headless/mod.rs` without a final `cargo fmt` pass, and
the CI format check caught it on main (run 30727918855, jobs `rust` and
`dogfood-policy`). This change is the mechanical `cargo fmt --all` result, no
semantic change: `cargo test -p reproit --lib` passes identically before and
after, and `cargo fmt --all -- --check` is clean after.

The same push repairs the declaration gap the policy named on 6732d15b7cc9:
this commit carries the `Reproit-Dogfood` trailer with this record.

Completion note: the first sweep commit staged only four files by explicit
path while `cargo fmt --all` had also reformatted `internal_dispatch.rs`,
`process_capsule/anchor.rs`, `process_capsule/tests.rs`, and three
`sdk/reproit-backend-rs` files (`capture.rs`, `ci.rs`, `tests/ci_harness.rs`);
this second commit lands the remainder, after which `cargo fmt --all --
--check` passes twice in a row over the whole workspace at HEAD.
