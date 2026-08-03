# Examples

Copy-paste starting points. Everything here is read by a test, so it tracks the schema instead of
drifting from it.

| file | what it is |
| --- | --- |
| [reproit.yaml](reproit.yaml) | an annotated configuration showing every section |
| [configs/](configs) | a minimal `reproit.yaml` per framework: web, android, flutter, react-native, electron, tauri, swift-ios, swift-macos, winui, desktop-toolkit, tui (11 files) |
| [journeys/marketing.yaml](journeys/marketing.yaml) | a scripted journey, the starting point for store screenshots |
| [appmap.json](appmap.json) | the app-model shape `reproit` builds while exploring |

`all_example_configs_load` parses every file in `configs/` and fails the build if one stops
loading, which is what keeps these honest as the schema evolves.

These are examples, meant to be read. The tiny apps CI drives to prove each platform works are
fixtures, not examples, and live in [`fixtures/`](../../fixtures).
