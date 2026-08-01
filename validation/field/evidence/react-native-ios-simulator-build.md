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

## The note row, and the observable

The row is not a `StaticText`. A live element tree from a running session, the
thing the previous attempt guessed at instead of reading, shows it as

    <XCUIElementTypeButton name="1. Welcome to Joplin!"
      label="1. Welcome to Joplin!" x="16" y="127" width="370" height="20"/>

An `XCUIElementTypeButton` whose frame **is** the pressable, which is precisely
the view the fix changes. Measuring that frame on both built products:

| revision | note row hit area |
| --- | --- |
| affected `7d90db0b` | `x=16 y=127 w=370 h=20` |
| fixed `2fa45a5a` | `x=0 y=111 w=402 h=52` |

The hit area grows by exactly the 16pt padding on all four sides, and the two
revisions centre the row identically at `(201, 137)`. A tap a fixed distance
above that centre is therefore the same absolute point on either build, which
is what makes one coordinate a fair trigger for both.

Executed at offset 19, tap point `(201, 118)` on both:

| revision | still on the note list | note opened |
| --- | --- | --- |
| affected | yes | no |
| fixed | no | yes |

The pair discriminates, on the observable the adapter owns, before any run of
the benchmark was spent on it. The identity is
`react-native-layout:note-row-padding-outside-touch-target`.

A fourth lesson cost two attempts. Reusing one simulator and calling
`simctl erase` between runs does not work: on an erased device WebDriverAgent
installs, reports `Launching WebDriverAgent on the device`, and then never
answers its port, so the session request sits on `connect ECONNREFUSED` until
it times out. A screenshot of the device during that wait shows the springboard
with the application installed and nothing running. A device that has never
been erased behaves. Each run therefore gets a simulator created for it and
deleted after it, which is also the stricter reading of a disposable container.

A third lesson came from the first campaign attempt. Building WebDriverAgent
into a fresh per-campaign directory succeeded, and the runner then never
answered: Appium sat on `connect ECONNREFUSED 127.0.0.1:8191` for far longer
than the build itself had taken, with `syspolicyd` pinned at 97% of a core.
macOS rescans a newly produced bundle, and on a loaded host that scan outlasts
the launch timeout. WebDriverAgent is test infrastructure rather than the
subject under test, so its build is allowed to persist across campaigns; the
driver takes the derived data path as an argument and records the path it used.

Two WebDriverAgent lessons are worth not rediscovering. Port 8100 is contended
whenever a neighbouring simulator is booted, and that contention is what
produces `Unable to start WebDriverAgent session. Original error: Request
failed with status code 401`; a distinct `appium:wdaLocalPort` and a
campaign-owned `appium:derivedDataPath` fix it. And compiling WebDriverAgent
once into that owned derived data, then reusing one Appium server for the whole
campaign, is the difference between a per-run rebuild and a per-run reinstall.

## What is still missing, and what was attempted

The first probe predicated on
`type == 'XCUIElementTypeStaticText' AND label == '1. Welcome to Joplin!'` and
never matched within 90 seconds even though the row was visibly on screen. That
was a guess at the addressing rather than a reading of it, and the tree above is
what reading it produced: the row is a `Button`, and its title is carried by
`name` and `label` on that button rather than by any static text.

## The campaign driver cannot obtain a session, and the probe can

The driver is committed and the observable it drives is measured, but no
benchmark has been run with it: it has never obtained a WebDriverAgent session.
Every attempt reaches `Launching WebDriverAgent on the device` and then waits on
`connect ECONNREFUSED` to the runner port. A screenshot of the device during
that wait shows the springboard with the application installed and nothing
running, so the runner is installed and never serves.

Four configurations were executed and none of them changes the outcome:

1. one device erased between runs, WebDriverAgent built fresh per campaign;
2. one device erased between runs, WebDriverAgent built once and reused;
3. a device created and deleted per run;
4. one device created and booted once, with the application container reset by
   uninstall and reinstall, which is the shape the probe used.

One real cause was found and fixed on the way: three `xcodebuild` runners
survived earlier interrupted attempts, because terminating the Appium server
does not reap the process it spawned to host WebDriverAgent, and a surviving
runner holds the test session so every later attempt waits on a port nothing
will answer. The driver reaps them now, at startup and after every run, and
records how many survived. That was necessary and not sufficient: with the host
verifiably clean of runners, attempt four still hung.

What is not explained is why `probe2` and `probe3`, which measured the frames
and executed the discriminating tap on both revisions, obtain sessions
reliably against the same Appium, the same driver version, the same
WebDriverAgent derived data and the same application bundles. The difference
between those scripts and this driver has not been isolated, and guessing at it
has now cost four executions. The next attempt should bisect it directly:
start from the probe, which works, and move it toward the driver one change at
a time.

A second independent React Native iOS application is still required. BlueWallet
remains excluded outright, so the qualified pool holds exactly one application
that has been proven to build, and the target cannot be promoted on Joplin
alone.
