# macOS AX exact native evidence

This directory retains the canonical `macos-ax` native release gate for the
clean commit `0067bc1084498a3816147810a8169bebcbaed670`.

- Executor: native macOS arm64
- Toolchain: Rust 1.88.0 and the pinned Xcode installation
- Source mode: exact, clean commit match
- Fixture: repository-owned SwiftUI application
- Result: `macos-ax.json`
- Captured log: `macos-ax.log`
- Validated summary: `validated-summary.json`
- Result SHA-256:
  `8c430f87df9cf615156f1de9cb52c0448ca4900987863b05a1ef73fb57aa014d`
- Log SHA-256:
  `1382a55c7b14895b22b4441e85d88b704d9aa917e17a290dff6b6130d74e27dd`
- Validated summary SHA-256:
  `4ba6d9a151bab82acd92cec181764aa8c498233053bdfccca36f709b166551c2`

The gate read the native Accessibility tree, executed three actions, observed
three structural states and three edges, and emitted every required completion
marker. The fixture process was absent after the gate's cleanup trap completed.
