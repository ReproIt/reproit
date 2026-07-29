# Windows UIA exact native evidence

This directory retains the canonical Windows UIA native gate result from a
clean detached Windows checkout at commit
`8cd2e7f63dd86740f1686d59e047cd8def7eeae1`.

The collector ran with:

```sh
COMMIT=8cd2e7f63dd86740f1686d59e047cd8def7eeae1
REPROIT_GATE_OUTPUT_DIR="target/reproit-validation/windows-8cd2e7f63dd8" \
  ./validation/causal/run-windows-remote.sh "$COMMIT"
```

The collector cloned and detached the native Windows checkout at the exact
commit, confirmed a clean worktree, ran the elevated interactive
`windows-uia` gate, tested the returned ZIP archive, and extracted the result.
An independent second invocation of
`validation/release/check-native-evidence.py` accepted the retained result for
the exact source commit and x86_64 architecture.

The full log records passing WPF, Avalonia, and WinUI journeys. It contains all
required state, edge, journey completion, and final pass markers, with no
`EXPLORE:EXCEPTION` marker.

The remote cleanup audit found zero exact-checkout paths, evidence staging
paths, archives, batch files, completion markers, logs, scheduled tasks,
fixture processes, Cargo or rustc processes, and PID-isolated Cargo target
directories. The backend runner's reusable
`%TEMP%\reproit-windows-backends` directory remained after the collector
finished. Its file timestamps matched this exact run, so the explicit owned
directory was removed and its absence was reverified.

The files in this directory are byte-identical to the validator-accepted
copies under the ignored target evidence directory.

Artifact SHA-256 values:

- `windows-uia.json`:
  `93d45b97475264873fd2783d4df637d20d1ad097e64cc146bc0876fbe533341a`
- `windows-uia.log`:
  `0ab59ad85972898bf71ae087191791f42e07c63059ef321be7fa10314c1c92dc`
- `validated-summary.json`:
  `bc1fb9178827e1a9ef1fb259f5746849d668c6c317b1d503d6035f09e74f97f6`
