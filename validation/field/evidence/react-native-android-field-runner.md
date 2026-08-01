# React Native Android field campaign runner

`validation/field/android/react_native_android_campaign.py` and its container
entry point `run_react_native_android_field_driver.sh` have existed for both
qualified applications, joplin and music, with no committed caller at all: a
repository-wide search for `REPROIT_FIELD_APPLICATION` matched the entry point
and nothing else. The campaign therefore could not be started by anyone, on any
host, without hand-assembling a Docker invocation.

This change commits the missing caller as two files, modelled directly on
`validation/release/run-android-x86-remote.sh` and its remote worker:

- `validation/field/android/run-react-native-android-field-remote.sh`, the host
  half. It validates the application, the two application archives and the run
  count, ships the tracked tree plus a payload archive holding
  `affected.apk`, `fixed.apk` and, for the music campaign, the media fixture
  directory, then collects the evidence archive into
  `target/reproit-validation/react-native-android-field/<application>`.
- `validation/field/android/react-native-android-field-worker.sh`, the remote
  half. It runs on the x86_64 host reached through the zgx gateway, verifies
  every uploaded digest including each application archive independently,
  refuses a pair whose two archives are identical, asserts the host and the
  Docker engine are native x86_64 with KVM, builds the same pinned worker image
  the release lane builds, and runs the campaign in one bounded named container
  with Docker network mode none.

## Why the remote route is not optional

The react-native-android bound is `android-emulator/x86_64`. Every Android
system image installed on the development machine is `arm64-v8a`, so this
campaign cannot run locally at all. The zgx gateway to strix is the only route
to a native x86_64 emulator with KVM, and it is the same route the Android
release gates already take, so the worker image, the SDK root and the download
cache are shared rather than duplicated.

## Bounds

- The source archive and the payload archive are each capped at 512 MiB, and so
  is the returned evidence archive.
- Each application archive is capped at 512 MiB before it is packed.
- `--runs` accepts 1 to 3, matching the campaign module's own bound.
- The remote command runs under `timeout 15000`.
- The owned remote directory name is matched against a fixed pattern before
  anything is written to it or removed from it.
- Evidence archive members are checked for absolute paths and `..` before
  extraction.
- Container, AVD, and remote run-directory cleanup all run from traps.

## Exact missing input

Four application archives: `affected.apk` and `fixed.apk` for joplin, built at
`de6378473fa261e495b4709672471613235b493a` and
`623da377db98dbc8576651aa066ef4000fbf2116`, and the same pair for
MissingCore/Music at `cdd2305aa0ae3bb5dcefe0691090a1d57cf53cb3` and
`5c86ff15ee99ac8f77abc19b9e58b98a705a9951`. Both applications are React Native
projects whose release Gradle build has to run somewhere with an Android SDK and
a JDK; neither archive was produced in this session, so no campaign run has been
executed and the runner is committed unexercised. That is stated here rather
than implied: this change ships the caller, not the campaign.
