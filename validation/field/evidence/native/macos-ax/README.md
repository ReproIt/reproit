# macOS AX exact native evidence

This directory retains the canonical `macos-ax` native release gate for the
clean commit `414c1a14c7d60e1127a7738834e995020366fed0`.

- Executor: native macOS arm64
- Toolchain: Rust 1.88.0 and the pinned Xcode installation
- Source mode: exact, clean commit match
- Fixture: repository-owned SwiftUI application
- Result: `macos-ax.json`
- Captured log: `macos-ax.log`
- Validated summary: `validated-summary.json`
- Result SHA-256:
  `341ceb3d29ca579fd1dfc13d1899a0c8fa759b5ece5a93fce6ed9feb27bdcb2f`
- Log SHA-256:
  `075740b0fb99cc174ecfad5513213588f6a647b5dda2bbab905cb16482f0e700`
- Validated summary SHA-256:
  `123a25817da17c2545c6ca25870f9c2b1490efdeb071ec2acc0600c8a3763e84`

The gate read the native Accessibility tree, executed three actions, observed
three structural states and three edges, and emitted every required completion
marker. The fixture process was absent after the gate's cleanup trap completed.
The host already holds the granted Accessibility permission the gate needs.

The gate's automation mode is still `permissioned-self-hosted`, so macOS
Accessibility cannot reach Stable on this evidence alone: a Stable target must
release-gate every owned fixture through required CI.
