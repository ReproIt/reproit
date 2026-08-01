# flutter-android: is a release APK observable through the Dart VM service?

The `unsupported-capability` blocker on `flutter-android` asserted that a Flutter
release APK is AOT compiled with the Dart VM service removed. That was an
assertion about the product, not a measurement, so it was measured.

## What was measured

One pinned revision of one real campaign application, LocalSend at
`3ec2d77875fc31dab21548ae4966ca693e8b2733`, built twice with Flutter 3.35.6 and
run twice on a reset `ReproitValidation_API36` AVD. Each run is: uninstall,
install, clear logcat, launch, wait, read that launch's own logcat window.

The upstream FOSS preparation script `scripts/remove_proprietary_dependencies.sh`
uses GNU `sed -i`. Run on macOS with BSD sed it deletes `purchase_provider.dart`
and silently fails to comment out the importing call sites, so the build fails
with undefined-name errors that look like a version problem and are not. It was
run inside `debian:bookworm-slim` instead.

## Result

| mode | APK builds | app starts | Dart VM service |
|---|---|---|---|
| profile | yes | yes, pid 4589 | yes, see the announcement below |
| release | yes, throwaway keystore | yes, pid 4917 | no announcement at all |

The profile launch announced:

```
I flutter : The Dart VM service is listening on http://127.0.0.1:38635/1X8DUVPoS6I=/
```

The release APK is not a build failure and not a crash. It runs. It simply
publishes no observation channel, so the declared runtime bound does not exist
in it.

## Consequence for the support claim

The honest resolution is both halves, not a choice between them:

1. A `flutter-android` campaign observes **profile-mode** builds.
2. The runtime bound is restated to say so, so no reader can take
   "Dart VM service" to mean that a shipped release APK is observable.

Recording only the first, and campaigning on profile builds while the manifest
still implies release support, is the failure this measurement exists to
prevent.

## Not measured here

The architecture. Every Android system image installed on this host is
`arm64-v8a`, and the target's arch bound is derived from the `flutter-android`
native gate, which declares `x86_64`. The build mode result is independent of
the architecture, but a campaign that wants to satisfy the declared arch bound
still needs an x86_64 image, which this host does not have.
