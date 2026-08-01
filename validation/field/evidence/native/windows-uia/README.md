# Windows UIA exact native evidence

This directory retains the canonical Windows UIA native gate result from a
clean detached Windows checkout at commit
`79e178175a4de92f07eb4b7cb3c6714b0ec2f824`.

The collector ran with:

```sh
COMMIT=79e178175a4de92f07eb4b7cb3c6714b0ec2f824
REPROIT_GATE_OUTPUT_DIR="target/reproit-validation/windows-79e178175a4d" \
  ./validation/causal/run-windows-remote.sh "$COMMIT"
```

This rerun replaces the `8cd2e7f63dd86740f1686d59e047cd8def7eeae1` evidence,
which predates commit `24f00b8`. Before that fix the harness spelled the
runner `reproit __uia`, which clap refuses, so the gate had been red since
`d957df5`. The fix is an ancestor of the commit above, so this is the first
exact-commit evidence taken from a pushed tree that contains it.

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
  `ab0c8c6e55db54e26f3205e0f5831c23897a3909bdc265854f867d766793db1b`
- `windows-uia.log`:
  `ecd107c8f6b8318db98f9a9f3b566edd3f2245f7bc767ee9c6565ac2dc948b49`
- `validated-summary.json`:
  `ea58e5822a5f4e2272380f806f474114ec548af5d74b8169cbc6984d4b0e97fa`
