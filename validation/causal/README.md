# Causal cross-framework validation

Run `./validation/causal/run-all.sh` from the repository root. The script is deliberately strict:
every toolchain must be installed and every adapter must pass. Platform-specific compilation
commands used in CI may extend this local matrix, but may not replace its behavioral capture/replay
assertions.

`./validation/causal/run-native.sh` is the platform-native gate. It requires a booted Android
emulator and iPhone simulator, then proves live capture and offline replay inside both apps and runs
the Linux transports in x86_64 Docker.

Run `./validation/causal/run-windows.ps1` directly from an existing native Windows checkout.
