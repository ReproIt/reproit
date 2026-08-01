# LocalSend receive-page link classification field evidence

LocalSend issue 2904 was replayed at affected revision
`3ec2d77875fc31dab21548ae4966ca693e8b2733` and fixed revision
`9e4a5985b5fd1377f7c4c1fa9127a00b8fc9abff`.

Both application archives are Flutter **profile** APKs for `android-x64`, built
inside the pinned lane worker image on the native x86_64 executor. Release APKs
expose no Dart VM service at all, which is why the manifest scope names profile
builds; that was measured, not assumed.

## Observation channel

The profile build registers the tree-dump RPCs but they carry nothing, because
the diagnostics behind them are compiled out outside debug mode:
`ext.flutter.debugDumpApp` answered only `WidgetsFlutterBinding - PROFILE MODE`,
`ext.flutter.debugDumpRenderTree` answered an empty string, and `evaluate` is
refused in AOT mode. The channel that carries the observable is the platform
accessibility hierarchy: driving the device through UiAutomator2 attaches a
UiAutomation, which is an assistive technology as far as the platform is
concerned, so Flutter starts producing semantics and the receive-page subtitle
appears as a `content-desc`. The Dart VM service is still connected on every
run, because it is the declared runtime bound and it proves the profile isolate
is live, but it is not the read path. Each run retains its service URI,
protocol version, and isolate id.

## Trigger

One `POST /api/localsend/v2/prepare-upload` on loopback carrying exactly one
file of `fileType` `text` whose `preview` is the message. The application does
not answer that request until the receive page is accepted or declined, so the
request is deliberately left pending on its own thread; the pending request is
the state under test, and every run asserts it was still pending when the
subtitle was read. There is no second device and no network: the container ran
with Docker network mode `none`.

## Result

With the message `https://example.com some extra text`, all three affected runs
showed `sent you a link:` and the open-link button, landing on
`flutter-receive:trailing-text-message-classified-as-link`. All three fixed
controls reached the same observation point and showed `sent you a message:`
with no open-link button.

Neighboring legal behavior: with the bare URL `https://example.com` and nothing
else, both revisions classify the message as a link, so the link path itself is
untouched and only the message that merely starts with a URL moves. Those two
runs are also the target's adversarial corpus subjects, because on the fixed
revision the bare URL legitimately produces exactly the surface the defect
produces.

Every observation used a newly created API 36 x86_64 AVD, emulator
`-wipe-data`, snapshots disabled, and a fresh Appium 3.5.2 UiAutomator2 8.0.0
session. `setupSeconds` in the benchmark is the measured reset-emulator
provisioning time of the first affected run; `replaySecondsP95` is the slowest
affected run, measured from uninstall to the subtitle being read. Memory is
recorded as unavailable rather than invented: the lane has no measured Dart
heap figure for a profile build.

APK digests, source hashes, screenshot hashes, reset evidence, Appium
capabilities and session ids, and the exact CLI commit are in the JSON record.
Representative affected and fixed screenshots were manually reviewed and agree
with the structured result.
