# Saber externally deleted Recent Notes field evidence

## Identity and revisions

- Issue: `saber-notes/saber#1603`
- Affected revision: `a77740bf4b55a962838940f77f763041544bc901`
- Fixed revision: `ed4fe66fc5908a55d2e20806e9cb01fc11ad5d78`
- Bundle: `com.adilhanney.saber`
- Failure identity:
  `recent-notes:externally-deleted-note-still-visible:No preview available\n26-07-29 Untitled`

The issue and fix are exact. The fixed revision adds removal of recent-file
references whose corresponding files no longer exist.

## Native environment

- Fresh disposable iPhone 16 Pro simulator:
  `0518894C-7758-4472-ABCF-D2DDB2DF8202`
- Runtime: iOS 18.5, build `22F77`, arm64
- Xcode: 26.2, build `17C52`
- Flutter: 3.41.6, framework revision `db50e20168`
- Appium: 3.5.2
- XCUITest driver: 11.16.2
- Signing: disabled, simulator build

The application was uninstalled and reinstalled before every reproduction.
Appium was bound to host loopback. Each note file mutation was confined to the
application's disposable simulator data container.

## Build and artifact identity

Both revisions were built with:

```sh
flutter pub get
flutter build ios --simulator --debug --no-codesign
```

The affected `App.framework/App` SHA-256 is
`813850d9c508c6c42bfb41ff669d3c15222869475d7486c2c512e6f8673f3a44`.
Its kernel blob SHA-256 is
`e7390a90e1ea4ee11b9ef0efa3481fd4e885a84c96b5c5646afa56007d2b0642`.

The fixed `App.framework/App` SHA-256 is
`290df12ce820987b2882805af68f8a1a82e1441d57125c73000c9ca455559378`.
Its kernel blob SHA-256 is
`a0e6eb73fedc519b05de5b4e676944810c88d3be8f94f55f814784b6b15c2d17`.

## Minimized trigger and observation

The driver creates two notes using real XCUITest taps and one drawing stroke
per note. It moves only the first note's `.sbn2` and `.sbn2.p` pair outside the
application container, terminates the application, relaunches it, dismisses
the bounded launch dialog, and reads the native accessibility tree.

The affected build exposes a stale Recent Notes row whose accessibility text
is `No preview available\n26-07-29 Untitled`. The fixed build reaches the same
Recent Notes observation without that row. The second note,
`26-07-29 Untitled (2)`, remains visible in all six runs and is the neighboring
legal behavior control.

All three affected runs reproduced the exact identity. All three fixed runs
reached the observation without reproducing it. Every run reported an empty
exception list.

## Cleanup

The deleted note pairs were retained under the campaign's temporary quarantine
while the evidence was assembled. The LocalSend campaign reused the simulator
only after uninstalling Saber. The final campaign cleanup shuts down and
deletes the disposable simulator, moves its temporary worktree and build
artifacts to the Trash, and stops the loopback Appium server.
