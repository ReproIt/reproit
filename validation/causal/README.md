# Causal cross-framework validation

Run `./validation/causal/run-all.sh` from the repository root. The script is deliberately strict:
every toolchain must be installed and every adapter must pass. Platform-specific compilation
commands used in CI may extend this local matrix, but may not replace its behavioral capture/replay
assertions.

`./validation/causal/run-native.sh` is the platform-native gate. It requires a booted Android
emulator and iPhone simulator, then proves live capture and offline replay inside both apps and runs
the Linux transports in x86_64 Docker.

`./validation/causal/run-windows-remote.sh` packages only the Windows SDK and gate, transfers it to
the OpenSSH alias in `REPROIT_WINDOWS_HOST`, and executes the native PowerShell gate there. Keep
usernames, keys, ports, and proxy routing in SSH configuration outside the repository.
`run-windows.ps1` can also be invoked directly in an existing Windows checkout.
