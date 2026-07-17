# ReproIt TUI SDK for TypeScript

This SDK connects a JavaScript or TypeScript terminal application to ReproIt. It records structural
screen transitions and crash paths so a production failure can be replayed by the local TUI runner.

## Capture frames

Pass the rendered frame after each settled update:

```ts
import { Reporter, ScreenContents } from "reproit-tui";

const reporter = new Reporter({
  appId: "my-tui",
  endpoint: process.env.REPROIT_ENDPOINT,
});

reporter.installCrashHandler();
reporter.observe(
  ScreenContents.fromText(frame, [cursorRow, cursorColumn]),
  "key:Down",
);
```

Ink applications can pass their rendered frame string directly. Cell-buffer renderers can use
`ScreenContents.fromRows`. The SDK does not depend on Ink or another rendering framework.

An edge is recorded only when the structural signature changes. Text labels are not part of the
signature, so translated interfaces retain the same structural identity. The implementation is
checked against the same golden vectors as the Rust runner.

## Reporter API

```ts
reporter.observeText(frame, [0, 0], "render");
reporter.observeRows(rows, [0, 0], "render");
reporter.recordError(error);
reporter.flush();
```

`recordError` includes the current signature and path. The crash handler records uncaught
exceptions, unhandled rejections, and termination signals, flushes the batch, and preserves the
application exit behavior.

## Report invariants

```ts
reporter.invariant("cart-total-nonnegative", () => cart.total >= 0);

reporter.invariant("one-pane-focused", () => {
  const count = panes.filter((pane) => pane.focused).length;
  if (count !== 1) throw new Error(`${count} panes focused`);
  return true;
});
```

An invariant becomes a finding only when it fails and the same violation reproduces.

## Declare authentication fields

Terminal cells do not expose input semantics. Declare each authentication field while it is present:

```ts
import { authInput } from "reproit-tui";

authInput("phone", "account-phone");
authInput("otp", "verification-code");
```

ReproIt follows stable purposes and screen transitions. It does not inspect visible labels.

## Validate

```sh
node --test sdk/reproit-tui-ts/test/parity.test.ts
```

The tests check signatures, fingerprints, value classes, cell rendering, and the event batch
contract against the shared golden vectors.
