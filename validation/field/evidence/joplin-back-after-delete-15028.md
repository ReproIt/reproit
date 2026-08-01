# Joplin: the hardware back key leaves the application after a notebook is deleted

Companion prose for `joplin-back-after-delete-15028.json`. This is the first
executed React Native Android application campaign, run on the zgx gateway's
strix host inside the pinned worker image with Docker network mode none.

- Application: `laurent22/joplin`, `packages/app-mobile`, React Native 0.81.6.
- Issue: https://github.com/laurent22/joplin/issues/15004, fixed by pull
  request 15028.
- Affected `de6378473fa261e495b4709672471613235b493a`, fixed
  `623da377db98dbc8576651aa066ef4000fbf2116`. The fix commit is a squash whose
  sole parent is the affected revision.
- Identity:
  `react-native-navigation:hardware-back-exits-app-after-deleted-notebook`.

## Why this defect belongs to this target

The fault is pure JavaScript. `packages/app-mobile/utils/appReducer.ts` keeps
the mobile navigation history in a Redux reducer, and when the route being left
is a deleted folder it removes the last history entry without pushing a
replacement, so the history can empty. The fix is four lines: push
`DEFAULT_ROUTE` when the history would otherwise be empty.

The trigger and the observation are both native. The trigger is `KEYCODE_BACK`,
delivered by UiAutomator2 as a real platform event; the observation is which
Android package owns the foreground afterwards. Neither half reproduces this
alone, which is exactly the bridge the react-native-android adapter owns.

## What the affected build actually does

Not what the qualification prose recorded. It does not strand the user on the
settings screen: with the history empty, nothing claims the back event, so
Android's default handling takes it and the activity finishes. All three
affected runs end with `com.google.android.apps.nexuslauncher` in the
foreground and the application gone. That is the identity, and a run that ended
stranded on settings would be reported as a different state rather than folded
under the same name.

## Runs

Every run installs the `assembleProfileable` APK onto a `pixel_6` AVD on
`system-images;android-36;google_apis;x86_64` recreated per run and booted with
`-wipe-data -no-snapshot`, so the Welcome notebook the trigger deletes is
genuinely first-run content.

| run | revision | foreground after back | note list | identity |
| --- | --- | --- | --- | --- |
| 1 | affected | launcher | no | the identity |
| 2 | affected | launcher | no | the identity |
| 3 | affected | launcher | no | the identity |
| 1 | fixed | joplin | yes | none |
| 2 | fixed | joplin | yes | none |
| 3 | fixed | joplin | yes | none |

Three affected reproductions, one exact identity, no drift. Three fixed
controls, every one reaching the same observation point and returning to the
note list instead.

## Minimized trigger

Long-press the Welcome notebook in the sidebar, delete it, confirm, open
Configuration, and send one `KEYCODE_BACK`. Nothing else is touched: no note is
opened, no setting is changed, no second notebook is created.

## Neighboring legal behavior

The same session without the deletion: open the sidebar, open Configuration,
send the same `KEYCODE_BACK`. On both revisions the application stays in the
foreground and the note list returns. This is the boundary the fix draws, and
it holds on the defective build, so the oracle is not simply reporting that
back sometimes exits an application.

## Containment and cleanup

Docker network mode none with only loopback in the container, plus Android
airplane mode on and Wi-Fi and mobile data disabled, asserted per run. The
cleanup audit passed: the run-scoped AVD directory is gone, no ADB devices
remain, and every owned emulator and Appium process is proven gone by comparing
its start clock ticks rather than only its PID.
