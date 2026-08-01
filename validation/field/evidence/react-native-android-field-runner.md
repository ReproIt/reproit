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

### joplin, second executed run: the wrong node was being pressed

With the addressing fallback in place the joplin campaign got past the long
press and failed later, waiting for the notebook action sheet. The retained
`joplin-affected-1-notebook-actions-wait-failure.xml` shows the sidebar wide
open, with Notebooks, Welcome!, All notes, Trash, Tags, Configuration and
Synchronize all present, and no action sheet. Reading the ancestry of every
node matching `Welcome!` explains both failures at once:

| node | class | clickable | bounds |
| --- | --- | --- | --- |
| `text="👋 Welcome!"` | `View` | false | `[163,128][817,264]` |
| `content-desc="👋, Welcome!"` | `Button` | **true** | `[0,438][651,533]` |
| `text="Welcome!"` | `TextView` | false | `[136,456][612,513]` |

The first is the note list header, and it precedes the sidebar in document
order, so `contains=True` matched the header rather than the notebook. In the
first run that header had no clickable ancestor anywhere above it, which is
what sent the walk to the boundless root; in the second the fallback made the
header addressable and the long press landed on a non-interactive title. The
sidebar row's own `TextView` has `text` exactly `Welcome!` and does have a
clickable `Button` ancestor, so the row is addressed exactly now, and the
post-deletion wait keys on `content-desc="Welcome!"`, an attribute only the
sidebar row carries. The action sheet labels the campaign waits for are correct
and were confirmed against `side-menu-content.tsx` at the affected revision:
the prompt title is `Notebook: %s`, the destructive item is `Delete`, and the
confirmation reads `Move notebook "%s" to the trash?` with an `OK`.

### joplin, third executed run: the sheet opens, its buttons shout

Addressing the row exactly opened the notebook action sheet, and the campaign
then timed out waiting for `Delete` in it. The retained dump from that run
contains exactly `['CANCEL', 'DELETE', 'EDIT', 'Notebook: Welcome!']`.
`side-menu-content.tsx` pushes menu items labelled `Edit`, `Delete` and
`Cancel`, and the Android `AlertDialog` these become renders its buttons with
`textAllCaps`, so the accessibility tree carries the shouted forms. The wait
and the tap use `DELETE` now. The confirmation's `OK` is already uppercase, so
that step was never affected.

### joplin, fourth executed run: two defects in the campaign's own predicates

The sheet opened, `DELETE` was tapped, the confirmation was accepted, the
sidebar updated and `Configuration` was tapped. The campaign then timed out
waiting for the settings screen, and the retained dump shows two separate
problems, one of which was silently letting a run continue in a wrong state.

`Synchronisation` is never drawn. `Setting.ts` asks for it with the British
spelling, and the shipped en_US catalogue renders `Synchronization`. The dump
of the settings screen contains Appearance, General, Editor, Markdown, Note,
Note History, Plugins, Tools, Import and Export, More information and
`Synchronization`. The predicate accepts either spelling now.

Worse, the same dump still lists the Welcome notebook. The
`-welcome-deleted` wait had passed anyway, because it tested the raw dump for
the exact attribute `content-desc="Welcome!"`, and by then UiAutomator2 was
rendering the re-drawn row as `Welcome!  (level 1)`. The exact string was
absent, so a check meant to prove the notebook was gone passed while it was
still there. Presence is now read from the parsed tree, by prefix, against the
one string only the sidebar row carries, and five unit cases guard it: the row
is seen with and without a nesting suffix, a genuinely deleted notebook is
absent, either spelling of the sync section counts as the settings screen, and
the sidebar alone does not.

The emoji matters here. The raw dump escapes it as a numeric entity, so a raw
substring search for the comma form silently never matches; the check has to
run against the parsed tree, which is also why the long press is matched there.

### joplin, fifth executed run: the defect does not do what was written down

The whole trigger executed. The notebook was deleted, settings opened, and the
hardware back key was sent. The run then reported that it had not reached an
observation, and the retained `joplin-affected-1-source.xml` says why: the
foreground package is `com.google.android.apps.nexuslauncher`. The screen is
the Android launcher, with Chrome, Gmail, Messages, Phone, Photos and YouTube.

The affected build does not strand the user on settings. It **leaves the
application**. That follows directly from the fix: `appReducer.ts` empties the
navigation history when the current route is a deleted folder, and the fix
pushes `DEFAULT_ROUTE` so there is always a back target. With the history empty
nothing claims the hardware back event, so Android's default handling takes it
and the activity finishes.

The oracle was written against the recorded prose rather than the behaviour,
and it is corrected to the behaviour: the identity is that the platform back
gesture left the application, and it is renamed to
`react-native-navigation:hardware-back-exits-app-after-deleted-notebook`. A run
that ends stranded on settings is a genuinely different state, so it is
recorded as such and fails the expected identity loudly rather than being
folded into the same name. Each run now retains the foreground package set, the
note list visibility and the settings visibility, so the three outcomes are
distinguishable in the evidence rather than collapsed into one boolean.

### joplin: complete

With the addressing, the shouted labels, the presence check and the observable
all corrected, the joplin campaign runs end to end and is written up in
`joplin-back-after-delete-15028.json` and its companion prose. Three affected
reproductions on one identity, three fixed controls reaching the same
observation, neighbouring legal behaviour holding on both revisions, and a
passing cleanup audit.

### music: MediaStore holds the fixtures, the application does not see them

The music campaign now seeds correctly. `scan_file` returns a real row per
fixture rather than the silence the old broadcast returned, `scan_volume`
completes, and the query-back assertion confirms all four files are indexed
before the application is launched. The campaign still stops, one step later
than before, in the new wait for the application's own track count to leave
zero. The retained `music-affected-1-scan-wait-failure.xml` shows the Home
screen after the media permission was granted, with `content-desc="0, Tracks"`
still on the card and `You haven't played anything yet!` below it, for the full
300 second bound.

So the fixtures are in MediaStore and the application's library is empty. The
remaining question is what makes MissingCore/Music ingest an already-populated
volume: whether its onboarding scan runs before the permission grant lands and
is never retried, or whether it needs its own rescan action driven from
settings. That is an application-behaviour question with a definite answer, not
a harness bound, and it is the one thing standing between this target and its
second application.

## The second application: Music is out, Notesnook is the replacement

MissingCore/Music is discarded rather than retried. The measurement above is a
property of that application: with all four fixtures confirmed present in
MediaStore before launch and the media permission granted, its library reports
zero tracks for the full 300 second bound. Making it ingest an already
populated volume is not harness work and it is not this target's job.

`streetwriters/notesnook` replaces it. It is already qualified in this record
as offline with a skippable signup, its release `signingConfig` uses the
committed `debug.keystore` so it needs no secret, and its selected defect
`notesnook-unlink-notebook-10053` has a verified pair, affected `14f727d6` and
fixed `7c3fdab6`, whose diff is confined to
`apps/mobile/app/screens/link-notebooks/index.tsx`: in single-select mode the
fix diffs the initial selection against the current one and writes an explicit
`deselected` state, so on the affected build a notebook the user removed stays
linked. Both revisions are bootstrapped, `npm install` and the workspace
bootstrap complete, and the iOS pods resolve at both.

## The host is stalling process launches, and it is blocking the builds

The Android build of Notesnook has not completed. It is not failing; it stalls,
in the same way the WebDriverAgent build stalled, and for the same reason.
`syspolicyd` on this host sits pinned near a full core, and processes spawned by
Gradle sleep in `_dyld_start` without executing a single instruction:

- `/usr/bin/env node .../@react-native-community/cli/build/bin.js config`, the
  React Native autolinking command, during the settings phase;
- `/usr/bin/env node ./node_modules/.bin/vite --version`, during the editor
  bundle step.

The same command runs instantly from an interactive shell, so this is not the
binary and not the script. Two workarounds are in place and both are recorded
rather than hidden: the autolink config is generated once from a shell and fed
to Gradle with `autolinkLibrariesFromCommand(["/bin/cat", <file>])`, which
produces byte-identical autolinking without spawning node, and the build is
driven by a bounded retry loop, because Gradle's up-to-date checks mean each
attempt resumes where the last stalled. That combination has carried the build
from the settings phase through configuration and into compilation.

This is the exact remaining input for the second application, and it is a build
problem on one host rather than anything about the application, the revision
pair or the trigger.

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

## Notesnook: both APKs built, and the pair discriminates

The previous section left the Notesnook Android build stalling on the
development Mac. It is built now, at both revisions, on the strix x86_64 Linux
worker that already hosts this lane, and the pair has been driven through the
trigger on a real x86_64 emulator. Neither result depended on the Mac.

| archive | revision | sha256 |
| --- | --- | --- |
| notesnook affected, `assembleRelease` x86_64 | `14f727d6e630f60299f1ceae42e48685e87cba8f` | `0e4cc6f1e804b5a40da4e7e798a3201c9c88bfa2c1e8babc5c5e3b42a2d24805` |
| notesnook fixed, `assembleRelease` x86_64 | `7c3fdab6eec0c083ef4c3b12ff16ba0d2f8aff2c` | `06302f8e554df5460d2f870a76809dc4c2b8374e269b0ad509d83397f6dc09d8` |

Three build inputs were missing, and each was found by a failed execution
rather than by inspection.

**Gradle 9 needs JDK 17, not 21.** The wrapper pins Gradle 9.0.0. Under JDK 21
the build dies during settings evaluation with `Class
org.gradle.jvm.toolchain.JvmVendorSpec does not have member field
'org.gradle.jvm.toolchain.JvmVendorSpec IBM_SEMERU'`, before a single project
is configured. Under JDK 17 the identical tree configures and builds.

**The workspace packages have to be built before the bundle.** `npm ci
--ignore-scripts` followed by `npm run bootstrap -- --scope=mobile` installs and
rebuilds native dependencies; it never builds any workspace package's `dist`.
Two full builds were spent on the symptom rather than the cause, because the
symptom is silent: `:app:createBundleReleaseJsAndAssets` fails with `hermesc
finished with non-zero exit value 5` and, above it, only

    CLIError: ENOENT: no such file or directory, open
    '.../apps/mobile/build/generated/android/index.bundle'

Repack's CLI exits 0 in that state and prints no compilation error at all, and
`--stats errors-warnings` prints nothing either. Running the application's own
`rspack.config.js` directly, with `config.name` set to the platform the way the
repack CLI sets it, reports what the CLI swallowed: `compiled with 402 errors`,
every one of them `Cannot find module '@notesnook/intl'` or a sibling workspace
import. Nothing is emitted, so the copy step opens a file that was never
written. `npm run tx mobile:build` builds that graph through the project's own
task runner, which is what its release workflows rely on, and with it the
bundle, hermesc and `assembleRelease` all succeed.

The first attempt at this diagnosis blamed `npm install --legacy-peer-deps`
resolving a graph the lockfile does not describe. That was wrong: switching to
`npm ci --ignore-scripts`, which is what the project's own workflow runs,
produced exactly the same failure. The correction is on the record rather than
quietly replaced, and `npm ci` is kept because it is the project's recipe.

**The emulator cannot be driven from the host.** Outside the worker image it
segfaults about a minute into startup with `-gpu off`, and the host has no Xvfb
for `-gpu swiftshader_indirect`. Inside the pinned worker image it still
segfaults under `swiftshader_indirect`, and it is stable only under the flags
the committed campaign already uses: `-gpu host -feature -Vulkan -no-metrics`
with Xvfb on `:99`. Two hours of emulator deaths were read as session teardown
before the core dump was read; the harness had the answer in it all along.

### The trigger, and the executed discrimination check

`streetwriters/notesnook` issue 7348, "X-ing a Notebook from 'linked notebook'
doesn't unlink notes from this Notebook", fixed by 7c3fdab6. The diff is one
file, `apps/mobile/app/screens/link-notebooks/index.tsx`. In single-select mode
the notebook row's `onPress` builds its next selection with `for (const key in
keys)`, iterating the array's indices rather than its notebook ids, so the
previously selected notebook is dropped from the selection map entirely instead
of being marked. `onSave` then iterates only that map, so nothing ever unlinks
it. The fix diffs the initial selection against the current one and writes an
explicit `deselected` state for anything that was selected and no longer is.

The trigger is the shortest sequence that reaches it, and every step is
addressed by a `testID` the application's own Detox suite already uses:

1. skip the signup, which the application allows offline;
2. create notebooks Alpha and Beta from the sidebar (`left`, `tab-notebooks`,
   `sidebar-add-button`, `title`, `yes`);
3. create one note titled `TriggerNote`, typed into `editor-title`, which the
   editor WebView does expose to the accessibility tree;
4. link it to Alpha (`listitem.menu`, `icon-notebooks`, the notebook row,
   `floating-save-button`). One relation means `multiSelect` is false;
5. reopen the same screen, tap Beta, and save.

The observable needs no navigation: the note row carries its linked notebooks
in its own `content-desc`. Executed on a `pixel_6` AVD on
`system-images;android-36;google_apis;x86_64` inside the pinned worker image,
the same five steps on the two builds read

| revision | note row after the relink |
| --- | --- |
| affected `14f727d6` | `02:15 PM, TriggerNote, <book>, Alpha, <book>, Beta` |
| fixed `7c3fdab6` | `02:28 PM, TriggerNote, <book>, Beta` |

`<book>` stands for the notebook glyph the row draws, which the dump escapes
as the private-use codepoint `U+F0B64`. It is spelled out rather than pasted,
because a private-use glyph does not survive every reader.

The affected build keeps the note in Alpha as well as Beta; the fixed build
unlinks Alpha. The pair discriminates, on the observable an Android adapter
owns, before any run of the benchmark is spent on it.

Three addressing lessons came out of driving it and belong in the campaign
module rather than being rediscovered:

- the header `left` button is the drawer toggle on a root screen and a back
  arrow inside a notebook, and creating a notebook navigates into it, so the
  sidebar has to be reopened from a root screen rather than assumed;
- notebook rows are addressed by `notebook-item-<depth>-<index>` and sort
  newest first, so Beta is index 0 and Alpha index 1 when Alpha is created
  first;
- every step needs a bounded wait for its node rather than one dump, because
  two of the three failed passes were taps sent into a screen that had not
  finished rendering.

### The observer, and what driving it through Appium corrected

`notesnook_link_notebooks.py` is the observer. It is its own module because the
campaign module was already within seventy lines of this repository's 1000 line
ceiling, and it drives the trigger above through Appium rather than adb, which
is what the benchmark runs on.

Three things the hand-driven pass had not met came out of writing it, each
found by running it rather than by reading it:

- creating a notebook raises a "Notebook added" toast whose own "Add notes"
  button is drawn directly over the sidebar add button, at
  `[608,2163][864,2270]` against `[671,2218][763,2309]`. Creating the second
  notebook immediately after the first therefore taps the toast and lands on
  the notebook's own add-notes screen. The observer waits for `toast.button` to
  go away between creations. The earlier hand-driven pass never saw this,
  because a human types the next step slowly enough for the toast to clear;
- the editor toolbar publishes buttons with `[0,0][0,0]` bounds, so addressing
  a node by testID has to require a rectangle with area or a tap lands at the
  top left corner of the screen;
- the previous note about `left` and `notebook-item-<depth>-<index>` is
  superseded by what the tree actually shows. Creating a notebook from the
  sidebar does NOT navigate into it, so the sidebar stays where it is, and the
  rows are addressed by their own `content-desc` prefix, `Alpha,`, rather than
  by an index that depends on creation order. The index form is still what the
  application sets, but the name is what the run means.

The observable is read from the note row's `content-desc`, parsed, as the set
of comma separated parts that are notebook names. Waiting for that set to stop
being the one the screen opened with is neutral between the two revisions:
they disagree about which set follows the relink, and agree that it is not the
one before it.

The three corpus subjects were driven by hand on the affected build before the
observer encoded them, on the same AVD:

| subject | note row after the trigger |
| --- | --- |
| first link only | `TriggerNote, <book>, Alpha` |
| selection reverted with the header restore button | `TriggerNote, <book>, Alpha` |
| multi-select relink from Alpha and Beta, adding Gamma | `TriggerNote, <book>, Alpha, <book>, Beta, <book>, Gamma` |

The restore button carries no testID: it is an `IconButton` whose only stable
attribute is the private-use codepoint of its own glyph, `U+F099B`, and it
exists only while the selection differs from the one the screen opened with.
Restoring makes those equal again, which is the same condition that draws the
floating save button, so the save button goes away with it and the only way off
the screen is back. That is what the subject does.

### The runner could not ship the checkout it was started from

The first attempt to run the notesnook benchmark never reached the worker. The
host half packed `.git` alongside the tracked files, which is a directory in an
ordinary checkout and a FILE naming an absolute path in a linked git worktree,
so the worker unpacked a pointer to a path that does not exist on it and its
first command answered `fatal: not a git repository: (null)`, followed by
`source commit mismatch`. Every campaign started from a worktree, which is how
this repository is worked in, would have stopped there.

A depth-1 clone fixes the shape but not the size: this repository's history is
about 800 MiB against a 512 MiB archive bound, and `git clone` from this
checkout did not finish inside ten minutes on the development Mac in any case.
What the runner stages instead is a one-commit repository: the objects
`git rev-list --objects --no-walk HEAD` names, packed, a `shallow` marker, a
ref at the commit, and the files. The worker's own checks then mean more than
they did, not less. index-pack verifies those objects against the commit, and
the emptiness of `git status` proves the files it received really are that
commit's tree rather than merely being labelled with its number.

### What the first executed notesnook benchmark found

The first benchmark run of the observer reproduced the identity on affected
runs 1 and 2 and then stopped in affected run 3, in `create_note`, with the
note list empty and no note to link. The retained dump is the ordinary Notes
screen with `buttons.add` and no `note-item-0` at all, which is what the
application shows when the note was never created.

The cause is the one step of the trigger that is not addressed by a testID the
application publishes for it: the title is typed into the editor WebView with
`input text` after a tap on `editor-title`, and a tap sent before that view has
taken focus types into nothing. The note is then empty, the editor discards an
empty note on the way out, and the run reaches a note list with nothing on it.
It is a race, which is why the same code passed two runs before it. The
observer now reads the title back out of the editor and retries the tap and the
typing until it is there, and fails with that sentence if it never is.

A one-run smoke of the committed campaign module, driven through Appium inside
the pinned worker image with a recreated `pixel_6` AVD per observation, was run
before the benchmark: affected reproduced
`react-native-state:single-select-relink-keeps-previous-notebook` with the note
in Alpha and Beta, fixed did not reproduce with the note in Beta alone, and the
neighbouring first link left the note in Alpha on both revisions. That is one
run per revision and is recorded as exactly that, not as a benchmark.

### The executed benchmark

The campaign then ran complete, in exact mode from a committed tree, as eleven
observations each on its own recreated `pixel_6` AVD inside one bounded
container with Docker network mode none: three affected reproductions, three
fixed controls, the neighbouring first link on both revisions, and the three
corpus subjects. Every affected run landed on
`react-native-state:single-select-relink-keeps-previous-notebook` with the note
in Alpha and Beta, every fixed run reached the same observation with the note
in Beta alone, the neighbouring first link left the note in Alpha on both
revisions, and the three corpus subjects reported nothing. The cleanup audit
passed. It is written up in
`validation/field/evidence/notesnook-relink-keeps-notebook-7348.md` and
`validation/field/corpus/react-native-android.json`, and it is what promotes
this target.

One observation, fixed run 1, took two infrastructure attempts: the first
Appium session was refused with `adb: device offline` while the driver was
clearing the hidden API policy, before any step of the trigger ran, and the
bounded retry recreated the AVD and ran the whole observation again. The reason
string is retained verbatim in the runs file.
