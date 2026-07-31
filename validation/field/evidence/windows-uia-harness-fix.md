# Windows UI Automation harness-fix diagnostic

This record proves that the `windows-uia` native gate passes once the harness
names a reachable internal command. It is diagnostic evidence, not exact-commit
promotion evidence, because the checkout was patched before the run.

## What was red

Running the canonical collector at commit
`414c1a14c7d60e1127a7738834e995020366fed0` failed:

```
WARNING: error: unrecognized subcommand '__uia'
WPF did not emit EXPLORE:STATE
Windows UIA gate failed with exit 1
```

`validation/backends/run-windows-desktop.ps1` started the runner with
`$start.Arguments = "__uia"`. The minimal CLI vocabulary (`d957df5`) moved every
internal command under one `internal` multiplex, so clap exited 2 before any
fixture ran. The gate has been red since that commit, which is why the retained
exact-commit Windows evidence names `8cd2e7f6`.

## What passes

The same collector, with the single-line harness patch applied on the guest
after checkout (`PATCHED:  M validation/backends/run-windows-desktop.ps1`, one
file), ran the gate to completion:

- Native worker: Windows x86_64 QEMU/KVM guest reached through
  `black@zgx-5a09.local`, `strix`, then `reproit@localhost:2223`
- Executor: `windows` / `amd64`, interactive session through
  `schtasks /it /rl highest`
- Harness: `validation/backends/run-windows-desktop.ps1`
- Result: passed, exit code 0
- Required output check
  `Windows DesktopUia backend passed WPF, Avalonia, and WinUI`: true
- Per-fixture markers: `WPF UI Automation runtime passed`,
  `Avalonia UI Automation runtime passed`,
  `WinUI UI Automation runtime passed`
- Retained log: `validation/field/evidence/windows-uia-harness-fix.log`
- Log SHA-256:
  `95df1a80ccf0d1fe851b94790a0dc8af542f7211c744aa3a16f5ebdec4c4c2c6`
- Guest cleanup: scheduled task unregistered, checkout, evidence, archive,
  batch, and marker paths removed by the collector's `finally` block

The collector stamps the fetched commit into its result record, so the retained
guest result claims `414c1a14...` while the tree that executed carried the
patch. That record is deliberately not retained under
`validation/field/evidence/native/`, and it must never be passed to
`check-native-evidence.py` as exact-commit evidence.

## What this does not settle

The harness fix is committed in this repository, but the collector clones the
exact commit from the public remote. Exact-commit Windows evidence therefore
needs one rerun of
`validation/causal/run-windows-remote.sh <commit>` at a pushed commit that
contains the fix. Until that rerun:

- Windows WinUI 3 keeps its `incomplete-evidence` native-gate blocker.
- Windows WPF and Windows Avalonia keep Stable on evidence collected at
  `8cd2e7f6`, before the harness broke; their evidence should be refreshed by
  the same rerun.
