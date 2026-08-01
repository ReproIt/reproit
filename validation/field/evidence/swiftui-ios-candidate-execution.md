# SwiftUI iOS candidate execution

Companion prose for `swiftui-ios-candidate-execution.json`. Recorded at CLI commit
`79e178175a4de92f07eb4b7cb3c6714b0ec2f824` on darwin/arm64 with Xcode 26.2 (17C52),
Appium 3.5.2 and the XCUITest driver 11.16.2, against disposable simulators on the two
installed runtimes, iOS 26.2 (23C54) and iOS 18.5 (22F77).

## Why this record exists

`validation/field/qualification/swiftui-ios.json` recorded ten candidate defects across
six applications, every one of them with `qualification: selected-not-executed` and
`simulatorBuildWithoutSigning.verified: false`. This session executed four of those
applications for the first time. None of them yielded an affected observation, so no
application campaign was started and nothing was written into
`validation/field/swiftui-ios.json`. The target stays Preview.

## What was executed

### damus, composer cursor 3461

The revision pair is exact: `4941b502` is the direct child of `f506f9cf` and the diff is
the five files of the fix. The unsigned simulator recipe installs but the process exits
with SIGTRAP before any interface appears; an ad-hoc signed build that keeps the project
entitlements, with the team blanked and no provisioning profile, launches normally.

The whole upstream trigger was driven end to end: sign in with the public test nsec that
`damusUITests` itself uses, clear the onboarding sheets, open the composer from the plus
control, focus the composer text view and type `Hello`. The upstream oracle is a hard
string equality on the text view value. On the affected revision the value read back is
exactly `Hello` in all three configurations tried: one character every 0.6 seconds on
iOS 26.2, a single burst on iOS 26.2, and a single burst on iOS 18.5. There is no
affected observation, so there is nothing to reproduce three times.

### Aidoku, iPad backup export 980

Builds and launches unsigned on an iPad simulator with no account, no network setup and
no manual backup creation, because the application writes an automatic backup on first
launch. Long pressing that row and tapping Export on the **affected** revision presents
`UIActivityViewController` on both installed runtimes. The defect was that the popover
presentation controller received a source view with no source rect; on these runtimes
that no longer suppresses presentation, so the recorded observable does not separate the
revisions.

### dimeApp, number pad delete 72

The qualification record called out one open question for this application: whether it
launches at all on an entitlement-free build, given that `DataController` installs
`NSPersistentCloudKitContainerOptions` unconditionally and calls `fatalError` when the
store fails to load. The answer is now recorded. An unsigned build installs and dies with
SIGTRAP on launch. An ad-hoc signed build that keeps the project entitlements launches
and reaches the welcome sheet. The trigger surface was not reached: the first-run category
setup step between the welcome sheet and the transaction number pad was not driven, so the
delete-key observable was never read on either revision. This application remains the
cheapest unexecuted candidate for this target.

### IceCubesApp, restore defaults 1859

Cannot be materialized on this toolchain. With `IceCubesApp.xcconfig` synthesized from the
committed template and an empty team, the build fails inside the RevenueCat package pinned
at these January 2024 revisions, which does not compile under the Xcode 26.2 Swift
compiler. Repinning an application's own dependencies would change the subject, so the
candidate is dropped rather than forced.

## What was not executed

Both kiwix-apple candidates. That project commits no xcodeproj: XcodeGen, `localizations.py`
and the per-revision libkiwix xcframework fetch all have to run before a build is possible,
and none of that was done here.

## Exact missing input

Two independent SwiftUI applications whose recorded defect separates the affected and fixed
revisions on an installed iOS simulator runtime. Of the six qualified applications, damus
and Aidoku are now empirically excluded, IceCubesApp cannot be built, dimeApp has a proven
build and launch recipe but no executed trigger, and kiwix-apple has never been generated.
