// example/ink.tsx shows the Ink integration. It is illustrative (Ink is not a
// dependency of this SDK, so this file is not run by the parity gate); it
// documents the one-call embed pattern.
//
// The key idea: Ink renders its React component tree to a STRING each frame. You
// do NOT need any Ink internal: capture the frame string from your own render
// path (the source you already pass to <Text>, a layout you build, or Ink's
// stdout interception) and hand it to the reporter. One Ink integration covers
// the JS/TS TUI population, because Ink is React-for-terminals and is the
// dominant JS TUI framework (and Claude Code itself renders with a vendored Ink).
//
// No em dashes anywhere, per project rules.

import React, { useEffect, useState } from 'react';
import { render, Box, Text, useApp, useInput } from 'ink';
import { Reporter, ScreenContents } from 'reproit-tui';

const reporter = new Reporter({
  appId: 'my-ink-cli',
  endpoint: process.env.REPROIT_ENDPOINT || null,
  // optional static context attached to every batch
  ctx: { release: process.env.npm_package_version },
});
// Flush a crash event with the current screen signature before the process dies.
// Tolerant of a vendored/forked Ink: it only touches `process`, never an Ink
// internal, so it works under Claude Code's bundled Ink too.
reporter.installCrashHandler();

function Counter() {
  const { exit } = useApp();
  const [count, setCount] = useState(0);

  useInput((input, key) => {
    if (input === '+') setCount((c) => c + 1);
    if (input === '-') setCount((c) => c - 1);
    if (input === 'q' || key.escape) {
      reporter.flush();
      exit();
    }
  });

  // After each render, hand the SDK the frame text + the action that produced it.
  // The frame text here is built the same way Ink lays out these components; in a
  // real app you can also intercept Ink's stdout write to capture the exact frame
  // string. The cursor is [row, col]; pass your focused-field position if you
  // track one, else [0, 0].
  useEffect(() => {
    const frame = `Count: ${count}\n[+] inc  [-] dec  [q] quit`;
    reporter.observe(ScreenContents.fromText(frame, [0, 7]), 'render');
  }, [count]);

  return (
    <Box flexDirection="column">
      <Text>Count: {count}</Text>
      <Text dimColor>[+] inc [-] dec [q] quit</Text>
    </Box>
  );
}

render(<Counter />);
