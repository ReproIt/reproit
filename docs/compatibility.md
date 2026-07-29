# Compatibility and promotion

`validation/support-manifest.json` is the canonical atomic compatibility
contract. `validation/compatibility/check.py` validates it and generates the
[current status](../validation/compatibility/STATUS.md). Documentation cannot
promote a target.

The generated [all-target stability plan](../validation/compatibility/STABILITY_PLAN.md)
turns every current blocker and native gate into a per-target worklist.

## Maturity

- Stable targets are covered by the 1.x compatibility promise and have complete
  native and independent-application evidence.
- Preview targets ship for evaluation and pass their named integration gates,
  but lack one or more field-promotion requirements.
- Experimental targets are explicitly selected specialist surfaces.

An atomic target becomes Stable only when it has:

1. exact-commit evidence for every owned native gate;
2. two independent real applications with pinned affected and fixed revisions;
3. three clean exact affected reproductions per application;
4. three clean fixed controls that reach the same observation point;
5. a minimized trigger that preserves the identity;
6. a passing neighboring-behavior control;
7. no confirmed false positive in the clean and adversarial corpus;
8. retained runtime, architecture, reset, cleanup, and artifact digests; and
9. a confirmed manual review.

Targets on the schema-3 promotion standard additionally require a clean
corpus, an adversarial corpus, a clean installation of the distributed package,
and a confirmed manual review. The four targets promoted before that standard
existed are recorded as `schema-2` in the manifest; that set is frozen and can
only shrink.

Families do not promote as a unit. Browsers, operating systems, desktop
toolkits, mobile frameworks, and webview hosts qualify independently.

## Production-to-local qualification

`maturity` and `productionToLocal` are independent fields. Stable is an atomic
compatibility claim. `productionToLocal` is the separate, stronger designation
that a real production occurrence on that target reproduces locally, and it
moves through Unqualified, FixtureQualified, and IndependentQualified.

The manifest stores this state as an evidence binding, not a label. An
Unqualified target has no evidence path. Every qualified target must cite a
retained schema-2 production-chain record that matches the target and level,
binds exact CLI, SDK, and application revisions, distinguishes fixture from
independent origin, names the Cloud project and occurrence, identifies the
trusted local provider, and verifies commands, assertions, reset, cleanup, and
artifact hashes. The compatibility validator rejects a state change when any
binding is absent or inconsistent.

## Current promotion state

<!-- generated:promotion-state -->

Stable atomic targets: 9. Preview: 12. Experimental: 0.

| Target | Maturity | Standard | OS | Architectures | Blockers |
|---|---|---|---|---|---|
| Backend contracts | Stable | schema-3 | linux | x86_64 | 0 |
| Jetpack Compose Android | Stable | schema-3 | android-emulator | x86_64 | 0 |
| Electron Linux | Stable | schema-3 | linux | x86_64 | 0 |
| Flutter Android | Preview | schema-3 | android-emulator | x86_64 | 3 |
| Flutter iOS | Stable | schema-3 | ios-simulator | arm64 | 0 |
| Linux GTK | Preview | schema-3 | linux-container | x86_64 | 3 |
| Linux Qt Quick/QML | Preview | schema-3 | linux-container | x86_64 | 2 |
| Linux Qt Widgets | Preview | schema-3 | linux-container | x86_64 | 2 |
| Linux wxWidgets | Preview | schema-3 | linux-container | x86_64 | 2 |
| macOS Accessibility | Preview | schema-3 | macos | arm64 | 3 |
| React Native Android | Preview | schema-3 | android-emulator | x86_64 | 2 |
| React Native iOS | Preview | schema-3 | ios-simulator | arm64 | 2 |
| SwiftUI iOS | Preview | schema-3 | ios-simulator | arm64 | 2 |
| Tauri Linux | Preview | schema-3 | linux | x86_64 | 2 |
| Terminal UI | Stable | schema-3 | linux | x86_64 | 0 |
| Web Chromium | Stable | schema-3 | linux | x86_64 | 0 |
| Web Firefox | Stable | schema-3 | linux | x86_64 | 0 |
| Web WebKit | Stable | schema-3 | linux | x86_64 | 0 |
| Windows Avalonia | Preview | schema-3 | windows-x86_64-interactive | x86_64 | 3 |
| Windows WinUI 3 | Preview | schema-3 | windows-x86_64-interactive | x86_64 | 3 |
| Windows WPF | Stable | schema-3 | windows-x86_64-interactive | x86_64 | 0 |

Every blocker, with its typed code and exact detail, is listed in
[the generated status](../validation/compatibility/STATUS.md).

<!-- /generated:promotion-state -->


## Stable 1.x surface

For Stable targets, 1.x preserves documented flags, exit behavior, JSON field
meaning, persisted formats, event protocol version 1, release archives, and the
published SDK source APIs. Patch releases may add optional fields but do not
remove fields, reinterpret results, or broaden a finding predicate.

Preview and Experimental adapters, specialist oracles, hidden diagnostics,
advanced causal reduction, and unpublished registry coordinates remain outside
that promise. They must fail closed and cannot silently create a regression
guard.

## Host requirements

- Node.js for browser-backed runners.
- Rust for source builds.
- The exact SDK, driver, simulator, and VM pins in
  `validation/native/toolchains.json`.
- The repairs reported by `reproit doctor` for the selected target.

Release archives cover macOS, Linux, and Windows on arm64 and x86_64. Native
behavior evidence records the architecture actually exercised.
