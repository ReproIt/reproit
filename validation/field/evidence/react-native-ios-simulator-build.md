# React Native iOS simulator build, executed at both revisions

`react-native-ios-candidate-execution.md` left one build input outstanding: a
root outside the `/tmp` symlink, because the Joplin archive failed in the
`Bundle React Native code and images` phase with

    Error: Failed to get the SHA-1 for: .../packages/app-mobile/index.js.

That input is supplied and the blocker is closed by execution. Both revisions of
`joplin-note-row-touch-target-15972` now build a simulator application, install
on a disposable simulator, and launch to the screen the defect lives on.

## Recipe

Build root `/Users/obsidian/reproit-field-rn/wt/joplin-ios-<variant>`, a real
path with no symlink component, one Git worktree per revision. Per revision:
`corepack yarn install --inline-builds` at the workspace root, whose postinstall
gulp task runs `pod install` for `packages/app-mobile`, then a second explicit
`pod install`, then

    xcodebuild \
      -workspace packages/app-mobile/ios/Joplin.xcworkspace \
      -scheme Joplin -configuration Release \
      -sdk iphonesimulator -destination 'generic/platform=iOS Simulator' \
      -derivedDataPath <out> ONLY_ACTIVE_ARCH=NO \
      DEVELOPMENT_TEAM= CODE_SIGN_IDENTITY=- CODE_SIGN_STYLE=Manual \
      PROVISIONING_PROFILE_SPECIFIER= build

The ad-hoc identity is the reusable discovery from the SwiftUI work rather than
`CODE_SIGNING_ALLOWED=NO`: Joplin declares `aps-environment` and the
`group.net.cozic.joplin` application group, and every revision hardcodes
`DEVELOPMENT_TEAM = A9BXAFS6CT` in four configurations, so the team has to be
forced empty rather than assumed unset. Xcode logs
`note: Using codesigning identity override: -` and both builds end in
`** BUILD SUCCEEDED **`.

The `/tmp` diagnosis holds exactly. Under the new root the bundling phase
addresses the entry file as
`.../packages/app-mobile/ios/Pods/../../node_modules/react-native/../../index.js`,
which normalises to a path Metro's haste map already holds, and the phase runs
to completion. Nothing about the application, the revision pair, or the
toolchain was implicated, as the previous record predicted.

## Artifacts

Xcode 26.2 (17C52), Node 26.5.0, CocoaPods, React Native 0.81.6, macOS arm64.

| revision | product | main.jsbundle sha256 |
| --- | --- | --- |
| affected `7d90db0bf68c7ea2803227f9e6277bb3cf697fb3` | `Joplin.app` | `096820b589792eeede8034740deff0990e3a283d6628d0f55521eb9f2ccce72e` |
| fixed `2fa45a5a05daa597d52b73fce120e9242a6c6860` | `Joplin.app` | `31e0a4f181a78333a4b69f93019aa0fd1821918b7fdd9894d750f9f653a61c4e` |

The two bundles differ, which is the minimum evidence that the pair is under
test rather than one build twice.

## Launch

A disposable `iPhone 16 Pro` simulator on iOS 26.2 was created for this, booted,
used, and deleted. Both applications installed with `simctl install` and
launched with `simctl launch net.cozic.joplin`; both survived, with
`launchctl list` inside the simulator still showing
`UIKitApplication:net.cozic.joplin` after the launch settled, and both rendered
the same first-run note list, the `Welcome!` notebook with its five seeded
notes. There is no SIGTRAP: the ad-hoc identity is what avoids it, and the
entitlements are retained in the built product rather than stripped.

That note list is the exact surface `packages/app-mobile/components/NoteItem.tsx`
draws, so the trigger surface for this candidate is reachable offline on a
first-run container with no account.

## What is still missing, and what was attempted

An XCUITest probe of the discriminating tap was attempted and did not produce a
verdict, so no claim is made about the observable. Two things were learned and
are worth not rediscovering:

1. The first attempt failed with `Unable to start WebDriverAgent session.
   Original error: Request failed with status code 401`. Other simulators on
   this host were already booted, so WebDriverAgent's default port 8100 is
   contended; passing a distinct `appium:wdaLocalPort` and a campaign-owned
   `appium:derivedDataPath` gets a session.
2. With a session established, the predicate
   `type == 'XCUIElementTypeStaticText' AND label == '1. Welcome to Joplin!'`
   never matched within 90 seconds even though the row is visibly on screen, so
   the Joplin note row does not expose its title through `label`. The addressing
   for the row has to be settled against a real accessibility tree dump before
   any campaign is authored on it.

The candidate itself remains the strongest in the set. The fix moves
`paddingLeft`, `paddingRight`, `paddingTop` and `paddingBottom` off the outer
`selectionWrapper` view and onto the pressable, so the discriminating trigger is
a coordinate tap inside the padded band around a note title: dead on the
affected build, opening the note on the fixed one. Only the addressing is
unsettled, not the mechanism.

## Exact missing input

1. The accessibility attribute the Joplin note row does expose, so an XCUITest
   trigger can address the row and compute the padded-band coordinate from its
   frame; then three affected reproductions and three fixed controls.
2. A second independent React Native iOS application with a verified affected
   and fixed revision pair. BlueWallet remains excluded outright, so the
   qualified pool still holds exactly one buildable application and the target
   cannot be promoted on Joplin alone. `streetwriters/notesnook` is the nearest
   unexplored candidate: it is already qualified as offline for the Android
   target, `apps/mobile/ios/Notesnook.xcodeproj` and its `Podfile` are committed
   at `14f727d6e630f60299f1ceae42e48685e87cba8f`, and the defect at pull request
   10053 is a persisted-state divergence that survives a screen transition. It
   has not been built and is not claimed here.
