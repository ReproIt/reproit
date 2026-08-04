# Reproit Debug for VS Code

This extension is a thin client for the versioned local Reproit debug-session
protocol. It does not execute reproduction plans or evaluate verdicts.

Open a session with:

```bash
reproit debug occ_123 --ide vscode
```

The generated `.code-workspace` points `reproit.debugSession` at the private,
gitignored descriptor. Use the command palette to mark debugger attachment,
replay the captured trigger, stop and clean up, inspect evidence, or launch a
fresh authoritative verification without the debugger.

The extension accepts only loopback control endpoints and bounded version-1
descriptors. Diagnostic sessions remain non-authoritative.
