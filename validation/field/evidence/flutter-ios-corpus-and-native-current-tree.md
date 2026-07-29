# Flutter iOS corpus and native current-tree evidence

This record covers the Flutter iOS false-positive corpus and the owned native
gate. The corpus is retained promotion evidence. The native result is a
working-tree diagnostic and is not exact-commit promotion evidence.

## Corpus subject

- Application: Saber fixed for issue 1603
- Repository: `https://github.com/saber-notes/saber`
- Revision: `ed4fe66fc5908a55d2e20806e9cb01fc11ad5d78`
- App framework SHA-256:
  `290df12ce820987b2882805af68f8a1a82e1441d57125c73000c9ca455559378`
- Simulator: iPhone 16 Pro, iOS 18.5, arm64
- Simulator UDID: `137D0FED-11CF-4AA8-9521-700BB996E416`
- Appium: 3.5.2
- XCUITest driver: 11.16.2

The subject's HTTP, HTTPS, and all-proxy environment pointed non-loopback
traffic to the closed loopback port `127.0.0.1:9`. Loopback was exempt so
XCUITest and the Flutter VM-service control plane remained usable. Every case
confirmed the proxy environment in the application process and inspected its
IP sockets. The only observed endpoint was a loopback VM-service listener.
There were no external established connections.

The retained schema-1 record contains one clean case and two adversarial cases:

1. Two ordinary live notes.
2. A complete note pair removed outside the application.
3. A valid note whose optional preview sidecar is absent.

The third case deliberately renders `No preview available` while the note
still exists. The oracle returns no defect identity because the legal
underlying note is retained.

Validation:

```text
flutter-ios corpus: PASS (1 clean, 2 adversarial, 0 false positives)
```

The retained record is `validation/field/corpus/flutter-ios.json`. The final
corpus Appium log, including orderly session deletion and server shutdown, has
SHA-256
`6432b3d7475f8f8e2a375e6e65b649ac2e430d6a73bfb7803f1913e041d040cb`.

## Owned native gate

The canonical `flutter-ios` gate ran from the current candidate working tree
with Rust 1.88.0:

```text
RUSTUP_TOOLCHAIN=1.88.0 \
REPROIT_GATE_OUTPUT_DIR=target/reproit-validation/flutter-ios-current-tree \
python3 validation/backends/gate.py flutter-ios --architecture arm64
```

- Recorded `HEAD`: `f696236f3d89f24d08f454e6e2e741348dae4263`
- Simulator UDID: `D36E453A-3D76-4C81-8A21-700FB3B8BA96`
- Result: passed
- Architecture: arm64
- Log SHA-256:
  `d6bd66c23845419a0e098eca72532833271176fea5d2ca083b85bf5b53a22cb4`
- Required markers: simulator creation, `EXPLORE:STATE`, `EXPLORE:EDGE`,
  `JOURNEY DONE`, `All tests passed`, and simulator deletion
- Raw result and log:
  `target/reproit-validation/flutter-ios-current-tree/`

The release evidence validator accepted the result structure, architecture,
markers, and log digest. The repository had uncommitted candidate changes
during the run, so the result's recorded `HEAD` does not identify the exact
tree that executed. It must not be used for Stable promotion.

The native journey and the retained Appium corpus both passed on Xcode 26.2
build `17C52`. The `macos-flutter` and `macos-appium` profiles now pin that
exact installed version, and the hosted macOS workflow explicitly selects it
before preflight instead of relying on the runner image's default Xcode.

## Cleanup

The corpus simulator and the native-gate simulator were shut down and deleted.
The Appium server was stopped. One WebDriverAgent `xcodebuild` process tied to
the deleted corpus simulator was found and terminated. The corpus worktree,
quarantine, fixed application copy, and Appium log were moved to
`/Users/obsidian/.Trash/reproit-flutter-ios-corpus-final.RkaE2F`.

Three exploratory simulators used to inspect native network-isolation support
were also deleted:

- `27053C37-8956-436A-AD07-FAE059934B5C`
- `4B4BE914-79F4-426E-948A-7EA06FE36448`
- `3A1DAF58-D405-4332-811E-6F4C45B535FC`

Final checks found none of these simulator UDIDs, no owned temporary gate
directory, no corpus Appium listener, and no owned process referring to the
deleted simulators.
