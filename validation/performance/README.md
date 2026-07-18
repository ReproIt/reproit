# CLI hot-path performance evidence

`run-cli-hotpaths.sh` exercises production graph traversal, runner parsing,
map merging, batch guidance, permission analysis, snapshot persistence, and
warm source fingerprinting.

Each JSON line reports median, minimum, and maximum nanoseconds per operation,
plus allocations, allocated bytes, and peak live bytes for one operation. Run
it from a quiet machine against a release build:

```sh
bash validation/performance/run-cli-hotpaths.sh
```

For comparisons, build each revision in an isolated worktree and pass its
`reproit-perf` binary as the first argument. Use the same machine, toolchain,
power state, workload matrix, and sample count for both revisions.

The first checked-in comparison and its correctness tradeoffs are recorded in
[`RESULTS-2026-07-17.md`](RESULTS-2026-07-17.md).
