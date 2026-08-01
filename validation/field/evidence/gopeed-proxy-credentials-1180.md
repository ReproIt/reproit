# gopeed proxy-credential persistence field evidence

gopeed issue 1180 was replayed at affected revision
`f7189668fd014696c9716bf7687ecc48fb91cd3b` and fixed revision
`5bb85413854f2f4202bdc8e6026a3a856358b4d4`.

Both application archives are Flutter **profile** APKs for `android-x64`, built
inside the pinned lane worker image on the native x86_64 executor. gopeed links
its Go download core as a gomobile archive, so the build provisions a Go
toolchain and builds `libgopeed.aar` for `android/amd64` before Flutter runs.
Three lane inputs had to be added for it: `build-tools;30.0.3` and
`platforms;android-31` and `platforms;android-33` in the shared lane SDK, which
Gradle cannot install itself because that SDK is mounted read-only.

## Observation channel

The same channel the LocalSend campaign uses: the platform accessibility
hierarchy through UiAutomator2. The Dart VM service is attached on every run
for liveness and the isolate id, and each run retains its service URI and
protocol version, but a profile build's dumps carry no tree.

gopeed renders each settings entry as a single merged semantics node, so the
proxy card's inner mode dropdown is not addressable by label and is reached by
a point inside the merged node instead. Once the card is expanded, the four
proxy fields are exposed as ordinary editable elements and their values are
readable. One trap is recorded in the driver: uiautomator escapes the newline
inside a merged label, so a predicate that tests the raw XML text for
`Settings\nTab 3 of 3` never matches; every predicate matches the decoded
attribute.

## Trigger

Select the custom proxy mode, type server `127.0.0.1`, port `1080`, username
`reproituser` and password `reproitpass`, wait for the application's debounced
configuration save, restart the process, reopen the card, and read the four
fields back. No download task is ever started and the container ran with
Docker network mode `none`.

## Result

All three affected runs restored `['127.0.0.1', '1080', '', '']`: the address
survived and both credentials were gone, landing on
`flutter-settings:proxy-credentials-dropped-on-restart`. All three fixed
controls reached the same observation point and restored
`['127.0.0.1', '1080', 'reproituser', '•••••••••••']`. The password renders as
bullets on the fixed revision because the same change adds `obscureText`, which
is why the username is the observable.

Neighboring legal behavior: typing only the server and port, with no
credentials, restores the address on both revisions, so the restart itself is
not what loses state and only the credential fields move. That run on the
affected revision is also a corpus subject, because it is the build carrying
the defect behaving legally.

Every observation used a newly created API 36 x86_64 AVD, emulator
`-wipe-data`, snapshots disabled, and a fresh Appium 3.5.2 UiAutomator2 8.0.0
session. `setupSeconds` is the measured reset-emulator provisioning time of the
first affected run and `replaySecondsP95` is the slowest affected run. Memory
is recorded as unavailable rather than invented.

APK digests, source hashes, screenshot hashes, reset evidence, Appium
capabilities and session ids, and the exact CLI commit are in the JSON record.
