# React Native Android field campaign runner

`validation/field/android/react_native_android_campaign.py` and its container
entry point `run_react_native_android_field_driver.sh` have existed for both
qualified applications, joplin and music, with no committed caller at all: a
repository-wide search for `REPROIT_FIELD_APPLICATION` matched the entry point
and nothing else. The campaign therefore could not be started by anyone, on any
host, without hand-assembling a Docker invocation.

The caller is committed as two files, modelled directly on
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

## What the first executed run found

The runner shipped unexercised, and the first real invocation of it, a music
campaign against APKs built at both selected revisions, did not reach a single
reproduction. It stopped in `validate_runtime` before the first emulator boot.
Three defects in the committed harness are answered here, each of which was
invisible to inspection and could only be found by running it:

1. `react_native_android_runtime.validate_runtime` required the environment
   variable `REPROIT_WORKER_IMAGE`, and no caller in the repository ever set
   it: a repository-wide search matched the assertion and nothing else. Every
   campaign would have aborted with `worker image provenance was None`. The
   worker now passes the identity of the image it actually built.
2. The same assertion compared that variable against a hardcoded content
   digest. The worker builds the image locally from
   `validation/release/android-x86/Dockerfile` and never pushes it, so its
   digest is a per-host build artifact and cannot be reproduced on another
   machine; the strix build produced
   `sha256:501c60972ec2f47beca5f6a6f609f0e3d60f1b21dc2117e3ab5262b5112744b5`
   against a pin of `sha256:8868695e...`. The pin is now the image NAME, which
   already embeds the first 20 hex of the Dockerfile digest and therefore pins
   the recipe exactly, and the content digest is retained as evidence instead of
   guessed in advance.
3. `REPROIT_FIELD_AVD_HOME` was the bind mount `/android-avd` itself.
   `Device.stop` removes its AVD home with `shutil.rmtree`, which cannot remove
   a mount point, so `cleanup_audit` reported `avdDirectoryExists: True` and the
   campaign failed its own cleanup audit unconditionally. The AVD home is now
   `/android-avd/run`, a directory inside the mount that the campaign creates
   and can therefore delete, which is also what the audit is trying to assert.

The first run is retained as the diagnostic it is: it proves the route, the
uploads, the digest checks, the x86_64 and KVM assertions and the worker image
build all work, and that the harness could never have got past its own preflight.

## What the second and third executed runs found

With the preflight answered, both campaigns were run for real on a fresh
`pixel_6` AVD on `system-images;android-36;google_apis;x86_64`, booted with
`-wipe-data -no-snapshot` inside the pinned worker image with Docker network
mode none. Both reached a live application and both then stopped inside the
authored trigger, on the application UI rather than on the harness plumbing.
Neither campaign produced a reproduction, so neither is written up as one.

The four application archives were built for this and are the ones under test:

| archive | revision | sha256 |
| --- | --- | --- |
| joplin affected, `assembleProfileable` | `de6378473fa261e495b4709672471613235b493a` | `ada9aa8bc691a2a09e115e18d4b5f12ea37d9c1b58463429afc86221f9ee0211` |
| joplin fixed, `assembleProfileable` | `623da377db98dbc8576651aa066ef4000fbf2116` | `61e244d5a48b841684ed78049b6f592776971ab12d1c63e339fc23809ac49835` |
| music affected, `assembleRelease` x86_64 split | `cdd2305aa0ae3bb5dcefe0691090a1d57cf53cb3` | `b3cdf7d696796a83cce5b24507a32c48ece661390b890bffc9ed2f6f7e28466e` |
| music fixed, `assembleRelease` x86_64 split | `5c86ff15ee99ac8f77abc19b9e58b98a705a9951` | `a4647bf41d7ac5cad72a545f6e6a18e1a409d252d01aad1172560a8f248e1dc4` |

Two build inputs are recorded because they are not the upstream default. The
Joplin profileable manifest reads `${usesCleartextTraffic}`, which no build type
defines, so `joplin_profileable_init.gradle` has to be passed with
`--init-script` or the manifest merger fails; that file was committed with no
caller and this is the first recorded use of it. The Music release build runs a
`sentry-cli` upload task that needs an upstream auth token, so it was built with
`SENTRY_DISABLE_AUTO_UPLOAD=true`, and it emits per-ABI splits rather than one
universal APK, so the campaign uses `app-x86_64-release.apk`.

### music: executed and disqualified on the authored navigation

`generate_react_native_music_fixtures.sh` had no caller either. It pins ffmpeg
7.1.5 and asserts four exact hashes; the development machine has ffmpeg 8.1.2,
strix has 7.1.5, and generating there reproduced all four committed hashes
byte-exactly, so the fixture set is confirmed reproducible.

The campaign then installed the affected APK, pushed all four fixtures, granted
the media permission and reached the application's own Home screen. It timed out
after 180 seconds in `capture_music_ui`, whose first wait requires both `HOME`
and `ARTISTS` in the accessibility tree. The retained
`music-affected-1-home-wait-failure.xml` shows the live screen at
`cdd2305aa0ae3bb5dcefe0691090a1d57cf53cb3`: the visible tabs are `HOME`,
`FOLDERS`, `PLAYLISTS` and `TRACKS` in a band at `y` 2150 to 2285, followed by
the `Search` and `Settings` actions, and the track count still reads `0`.

Two independent causes, and the first reading of this dump got one of them
wrong. The correction is on the record rather than quietly replaced.

**The ARTISTS tab is not missing; it is off screen.** `(main)/_layout.tsx`
renders the bar as a horizontal `FlatList` over `index` plus every displayed
tab, and `UserPreferences.ts` defaults `tabsOrder` to
`["folder", "playlist", "track", "album", "artist"]` with all of them visible.
Six entries do not fit 1080 device pixels, so `ALBUMS` and `ARTISTS` start
beyond the right edge, and a UiAutomator2 dump only reports visible nodes.
Waiting for `ARTISTS` without scrolling the bar could never have succeeded on
any revision. The first record said the tab did not exist at this revision;
that was wrong about the mechanism, and the candidate was never in doubt.

**Nothing was ever indexed.** `seed_music` announced each pushed file with
`ACTION_MEDIA_SCANNER_SCAN_FILE`. That is a protected broadcast MediaProvider
stopped honouring long before API 36, so it silently indexed nothing and the
application had no media to scan, which is why the track count read `0` rather
than `4`. Seeding now calls MediaStore's `scan_file` per fixture and
`scan_volume` for `external_primary` through the content provider, queries the
audio volume back, and refuses to continue unless every fixture is present.

### joplin: executed and disqualified on an unaddressable node

The joplin campaign got further. It installed the profileable APK, launched
`net.cozic.joplin/.MainActivity`, opened the Sidebar and found the `Welcome!`
notebook row. It failed in `long_press_text` with
`UI node has no usable bounds: 'Welcome!'`. `find_node` walks up from the
matching node until it finds an ancestor with `clickable="true"`, and when no
such ancestor exists it stops at the root, which carries no `bounds` attribute.
The Joplin sidebar row is a React Native touchable that does not set
`clickable`, so the walk always reaches the root and the long press can never be
addressed. This is a defect in the campaign module's node resolution, not in the
application: the row is present, named, and on screen.

Both runs passed their cleanup audit with `avdDirectoryExists: false`, no
remaining ADB devices, and every owned emulator and Appium process proven gone
by start-clock-tick identity, so the ownership and reset half of the runner is
now executed rather than asserted.

## Both trigger defects answered

- `find_node` still prefers a clickable ancestor, and now falls back to the
  matching node itself, rising only as far as the nearest ancestor with bounds a
  pointer action can target. Three unit cases guard the three outcomes.
- The Music trigger waits for the home screen, then waits for the application's
  own track count to leave zero, then scrolls the navigation bar within its own
  measured band until `ARTISTS` is addressable, retaining a dump if eight swipes
  do not reveal it.
- Seeding proves the index rather than requesting it, and each run record
  retains the MediaStore rows it saw.

`react_native_android_campaign.py` reached this repository's 1000 line ceiling
with these changes, so the accessibility-tree addressing and the gestures both
applications share moved into `react_native_android_ui.py`.
