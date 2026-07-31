# Apple simulator exact native evidence

This directory retains the canonical `swiftui-ios` and `react-native-ios`
native release gates for commit
`414c1a14c7d60e1127a7738834e995020366fed0`.

- Executor: native macOS arm64 host
- Device: disposable iPhone 16 Pro simulator on the iOS 18.5 runtime, created
  and deleted per gate through `xcrun simctl`
- Automation: Appium 3.5.2 with the XCUITest driver
- Results: `swiftui-ios.json`, `react-native-ios.json`
- Captured logs: `swiftui-ios.log`, `react-native-ios.log`
- Validated summary: `validated-summary.json`
- Log SHA-256:
  - `swiftui-ios.log`:
    `599e32a6040d2616c28d3231dd35258c161ae7599966562a0b6dd8820db74144`
  - `react-native-ios.log`:
    `4d6768f92fa2c1131b7325ce78f75048db662aa166915d0fb4dcf9715a217f95`
- Validated summary SHA-256:
  `21c38355d6fe06c9fa75d2309e6fb207a0a7bb77592a03fc31464dfe7ec9ead1`

Both gates recorded the simulator reset marker, explored states and edges
through the real Appium product path, finished the journey, passed every
assertion, and recorded the simulator deletion marker.

Native fixture success does not promote either target. SwiftUI iOS and React
Native iOS still need their two-independent-application field campaigns and
per-target corpus records.
