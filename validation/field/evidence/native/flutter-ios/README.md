# Flutter iOS exact native evidence

This directory retains the canonical Flutter iOS native gate result from a
clean detached worktree at commit
`8cd2e7f63dd86740f1686d59e047cd8def7eeae1`.

The gate ran with:

```sh
COMMIT=8cd2e7f63dd86740f1686d59e047cd8def7eeae1
RUSTUP_TOOLCHAIN=1.88.0 \
REPROIT_GATE_OUTPUT_DIR="target/reproit-validation/flutter-ios-exact-$COMMIT" \
python3 validation/backends/gate.py flutter-ios --architecture arm64
```

`validation/release/check-native-evidence.py` accepted the result for the
exact source commit. The disposable simulator and generated application
directory were absent after cleanup.

Artifact SHA-256 values:

- `flutter-ios.json`:
  `60d39f488d5d9660b7ae2072ae3d2a4d98fe17da16cb1e63259c3d126ecf961f`
- `flutter-ios.log`:
  `83fed39b76f0a7b86625ce32cf7b6db3c29f4421325cff243211e8cd9e6a105a`
- `validated-summary.json`:
  `e555a04c11c1ef5b17a5b8b29d1b6e030eea4924500b22401cd4b2bd292ef1dd`
