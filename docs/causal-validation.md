# Causal adapter validation matrix

This matrix is the completion checklist for causal capture. `captured` means the adapter can
distinguish a traffic-free run from an uninstrumented run. A network-dependent finding is never
confirmed when any required row reports `unsupported` or `unavailable`.

| Platform IDs                                 | Adapter                                         |     Bootstrap |      Replay | Local evidence                                               |
| -------------------------------------------- | ----------------------------------------------- | ------------: | ----------: | ------------------------------------------------------------ |
| `web`                                        | Playwright context HTTP, JSON WS/SSE            |           yes | fail-closed | `runners/web/capsule.test.mjs`                               |
| `electron`                                   | Electron browser-context HTTP, JSON WS/SSE      |           yes | fail-closed | `runners/electron-capsule.test.mjs`                          |
| `tauri`                                      | Tauri v2 document-start fetch/XHR plugin        |           yes | fail-closed | `sdk/reproit-tauri/test/init.test.mjs`, `cargo check`        |
| `flutter`                                    | zone-wide `package:http` client                 |           yes | fail-closed | `sdk/reproit_flutter/test/causal_test.dart`                  |
| `react-native`                               | global fetch/XHR plus autolinked capsule module |           yes | fail-closed | 82 Jest tests, TypeScript build, podspec and ObjC checks     |
| `swift-ios`, `swift-macos`                   | Foundation `URLProtocol`                        |           yes | fail-closed | 69 Swift tests plus native simulator capture/offline replay  |
| `android`                                    | dependency-free `ReproIt.causalHttp`            |     when used | fail-closed | 47 host tests plus native emulator capture/offline replay    |
| `winui`                                      | `.NET` delegating handler                       |     when used | fail-closed | 19 tests plus native Windows x64 VM build and offline replay |
| `gtk`, `qt` on Linux                         | process-wide Python `urllib`                    |           yes | fail-closed | Linux causal + 25-vector + GTK/Qt mapping tests              |
| `qt`, `avalonia`, `wxwidgets` on other hosts | .NET, Swift, Python, or shared C host transport |     when used | fail-closed | host fixture, capability gate, and `runners/test_causal.c`   |
| `tui` TypeScript/Go/Python/Rust              | SDK transports                                  | SDK-dependent | fail-closed | language SDK causal tests                                    |

## Universal assertions

Every adapter is expected to prove all of these, not merely compile:

1. bootstrap inputs use action index `0`; user actions are actor-local and 1-based;
2. canonical request identity includes actor, action, method and normalized URL;
3. credentials and identity-shaped JSON fields are replaced before persistence;
4. an unmatched required request produces `CAPSULE:MISS` and never reaches live infrastructure;
5. capability files/markers explicitly distinguish captured, unsupported and unavailable;
6. the Rust host validates and redacts again, then performs joint action, exchange and JSON
   reduction followed by a final clean confirmation;
7. multi-actor capability is aggregated by the least capable actor.

Rows that say “when used” are intentionally not reported as automatic by `reproit doctor`; their
application-side client constructor/enable call is the proof that all relevant traffic is inside the
adapter.

`validation/causal/run-all.sh` is the repeatable local audit entry point. It fails on a missing tool
instead of silently skipping a framework. `validation/causal/run-native.sh` adds real Android/iOS
simulator capture and offline replay plus x86_64 Linux containers. Validate Windows by running
`validation/causal/run-windows.ps1` directly in a native Windows checkout.
