# Universal authentication metadata contract

Authentication discovery never reads visible words. Every backend normalizes native metadata to an
`inputPurpose` on `EXPLORE:STATE.elements`:

`username`, `email`, `phone`, `password`, `new-password`, `otp`, `passkey`, or `recovery-code`.

Native sources include HTML autocomplete/type, iOS content types, Android autofill hints, Flutter
autofill/secure-entry semantics, Windows UIA input metadata, macOS AX secure fields, and AT-SPI
object attributes.

Immediate-mode apps use the same identifier through `REPROIT_INPUT_PURPOSE("otp", "verification")`.
Terminal apps have no retained widget tree, so the TypeScript SDK exposes
`authInput("otp", "verification")`. The terminal runner transports that declaration through a
private temporary file; it never renders metadata into the PTY and is a no-op outside ReproIt
execution.

When a surface cannot expose native purpose metadata, use the universal stable identifier
convention:

```text
reproit-purpose-<purpose>--<developer-id>
```

Examples:

```text
reproit-purpose-email--account
reproit-purpose-password--password
reproit-purpose-otp--verification
```

The identifier is structural and locale independent. Web `data-testid`, native accessibility/test
identifiers, Flutter keys, desktop AutomationId/AXIdentifier, and instrumented widget ids all carry
the same convention. A backend that has neither native metadata nor an annotation must stay silent
and report a missing capability; localized label heuristics are forbidden.

Discovery follows structural state transitions: fill the purpose present on the current state, take
the transition whose destination exposes the next required purpose, and accept the generated
account journey only after a clean replay. Multi-screen phone → OTP and email → password flows
therefore use the same algorithm regardless of language, script, layout, or platform.
