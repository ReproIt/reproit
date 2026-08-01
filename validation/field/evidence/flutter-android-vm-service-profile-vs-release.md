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

## Second round: the same question on the declared architecture

Every Android system image on the macOS host is `arm64-v8a`, so the first round
could not run on the declared `x86_64` bound. The second round used the lane's
own x86_64 executor: `black@zgx-5a09.local` then `strix`, native x86_64 with
KVM, inside the lane's worker image, with the AVD created and booted exactly the
way `validation/release/android-x86/run-isolated.sh` does it (Xvfb with a GL
renderer, emulator 36.2.12, `-wipe-data`, `-no-snapshot`, `-gpu host`, Vulkan
off).

Both revisions of LocalSend now build there in profile mode for `android-x64`:

| revision | role | APK sha256 |
|---|---|---|
| `3ec2d77875fc31dab21548ae4966ca693e8b2733` | affected | `15d50ed57cf714b4fcaa123bd62b13c657934ff00f510d555d6819f638ec6e6e` |
| `9e4a5985b5fd1377f7c4c1fa9127a00b8fc9abff` | fixed | `c093cebaccf080849953516b658baa32c7e883f3ae7ca589111cbfa8f020dfbc` |

Two build traps on that executor, both fixed in the staging script rather than
in the subject: the Gradle build wants `ndk;27.0.12077973`, which was installed
into the lane SDK, and cargokit looks for rustup under `$HOME/.cargo` while the
image keeps the toolchain in `/usr/local`, which fails as "rustup is not
installed at /root/.cargo".

## What the profile VM service can actually answer

Having the service is not the same as being able to observe the application, so
that was measured too, on the x86_64 emulator, against the affected profile APK:

```
BOOTED x86_64
ANNOUNCE: The Dart VM service is listening on http://127.0.0.1:45881/TGELBgvIYfE=/
VM_VERSION: 3.9.2 (stable) on "android_x64"
ISOLATES: 6
EXTENSION_COUNT: 26
HAS_INSPECTOR: False
EVALUATE: error 113 Expression compilation error, "Debugger is disabled in AOT mode."
```

The 26 registered extensions include `ext.flutter.debugDumpApp`,
`ext.flutter.debugDumpRenderTree`,
`ext.flutter.debugDumpSemanticsTreeInTraversalOrder`, and the `dart.io` HTTP and
socket profilers. They do not include `ext.flutter.inspector.*`.

So the profile channel is a **dump-and-profile** channel, not an
**evaluate-and-inspect** one. A flutter-android scenario must state its
observable as something a tree dump or an IO profile can show, such as which
widget the receive page built, and must not assume it can evaluate a Dart
expression or query the widget inspector. The runtime bound is written to say
exactly that.
