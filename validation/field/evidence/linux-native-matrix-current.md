# Linux native matrix, current-tree diagnostic

This record is diagnostic evidence for the uncommitted shared worktree. It is
not exact-commit promotion evidence and must not be passed to
`check-native-evidence.py` as such.

## Source and execution lanes

- Base commit: `f696236f3d89f24d08f454e6e2e741348dae4263`.
- Local lane: arm64 Docker on macOS.
- Native x86_64 lane: `ssh black@zgx-5a09.local`, then `ssh strix`.
- Native x86_64 host: Fedora `x86_64`.
- Native x86_64 Docker engine: `x86_64/linux`.
- Remote collector:
  `validation/release/run-linux-x86-remote.sh --current-tree`.
- The collector bounds the source and result archives, verifies archive
  digests, verifies the Git base and both host architectures, owns a unique
  remote directory and gate-specific images, retains gate logs and result
  records, and removes its owned containers, images, and remote directory.
- Current collector result revisions are derived from the source archive
  digest. They deliberately do not equal the base Git commit.

## Retained results

| Gate | Architecture | Result | Retained directory |
| --- | --- | --- | --- |
| `backend-contract` | arm64 | PASS | `target/reproit-validation/linux-arm64-current` |
| `linux-atspi-gtk` | arm64 | PASS | `target/reproit-validation/linux-arm64-current` |
| `linux-atspi-toolkits` | arm64 | PASS | `target/reproit-validation/linux-arm64-current` |
| `web-engines` | x86_64 | PASS | `target/reproit-validation/linux-x86-web-engines-run` |
| `tauri` | x86_64 | PASS | `target/reproit-validation/linux-x86-containers-run` |
| `linux-atspi-gtk` | x86_64 | PASS | `target/reproit-validation/linux-x86-atspi-run-2` |
| `linux-atspi-toolkits` | x86_64 | PASS | `target/reproit-validation/linux-x86-atspi-run-2` |
| `electron` | x86_64 | PASS | `target/reproit-validation/linux-x86-electron-run-3` |

All PASS records have exit code zero and all required output checks set to
true. The combined toolkit gate exercises Qt Widgets, Qt Quick, and wxWidgets.
The earlier `web-engines` record predates the diagnostic-revision safeguard,
so its run metadata must accompany it and it must not be submitted as
exact-commit evidence.

The first Electron worker lacked `libgtk-3.so.0`. Launch-level diagnostics
identified that exact dependency. The reusable worker now installs the
Electron desktop runtime libraries explicitly, and the rerun completed every
required state, edge, overflow, and journey check.

## Additional observed outcomes

- The Chromium fixture completed with `All tests passed` on native x86_64.
- The TUI fixture completed with `All tests passed` on native x86_64.
- Their structured results were lost when an earlier collector revision was
  changed while it was running. They are not counted as retained evidence and
  must be rerun from the shared exact commit.
- The native x86_64 backend-contract fixture reached both expected scan
  observations, then its worker failed because `jq` was absent. The reusable
  worker now installs `jq`; the retained arm64 backend-contract result passed.

## Qualification gaps

- The shared worktree is dirty, so the collector's exact mode correctly
  refuses to run.
- Exact-commit results are still required after the shared changes are
  committed.
- Native fixture success does not replace the two-independent-application
  field campaigns or production-to-local qualification.
- The Stable web and TUI schema-3 ratchet now has retained offline clean and
  adversarial corpus records with zero false positives.
- No maturity, qualification level, or typed promotion blocker was changed by
  this diagnostic.
