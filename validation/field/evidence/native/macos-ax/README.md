# macOS AX exact native evidence

This directory retains the canonical `macos-ax` native release gate for the
clean commit `79e178175a4de92f07eb4b7cb3c6714b0ec2f824`.

- Executor: native macOS arm64
- Toolchain: Rust 1.88.0 and the pinned Xcode installation
- Source mode: exact, clean commit match
- Fixture: repository-owned SwiftUI application
- Result: `macos-ax.json`
- Captured log: `macos-ax.log`
- Validated summary: `validated-summary.json`
- Result SHA-256:
  `609109dc1382bcc5aecb04f1ecf4a355e2abc8fae81e242976962216ca7497a7`
- Log SHA-256:
  `cc54b8c0b157a6031e0784cdde9d397832a48ec783c5cbf533404fd44e188638`
- Validated summary SHA-256:
  `db27ae4e7b66f72e06587f64f7bc1568c8342808566899017c26b9431c168c5a`

The gate read the native Accessibility tree, executed three actions, observed
three structural states and three edges, and emitted every required completion
marker. The fixture process was absent after the gate's cleanup trap completed.
The host already holds the granted Accessibility permission the gate needs.

The gate's automation mode is still `permissioned-self-hosted`, so macOS
Accessibility cannot reach Stable on this evidence alone: a Stable target must
release-gate every owned fixture through required CI. That change needs a
macOS runner registered with the repository, on a host where a human has
granted Accessibility to the runner's process tree. The repository has zero
self-hosted runners registered, so the mode cannot be raised honestly yet.
