# Compatibility and promotion

`validation/support-manifest.json` is the canonical atomic compatibility
contract. `validation/compatibility/check.py` validates it and generates the
[current status](../validation/compatibility/STATUS.md). Documentation cannot
promote a target.

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

Families do not promote as a unit. Browsers, operating systems, desktop
toolkits, mobile frameworks, and webview hosts qualify independently.

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
