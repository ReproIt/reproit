# LocalSend compound URI field evidence

## Identity and revisions

- Issue: `localsend/localsend#2904`
- Affected revision: `3ec2d77875fc31dab21548ae4966ca693e8b2733`
- Fixed revision: `9e4a5985b5fd1377f7c4c1fa9127a00b8fc9abff`
- Bundle: `org.localsend.localsendApp`
- Failure identity:
  `receive-message:compound-uri-misclassified-as-link:open-button-visible`

The one-line fix requires a message to contain no whitespace before accepting
it as an absolute URI. The selected pair therefore isolates the reported
classification behavior.

## Native environment

- Fresh disposable iPhone 16 Pro simulator:
  `0518894C-7758-4472-ABCF-D2DDB2DF8202`
- Runtime: iOS 18.5, build `22F77`, arm64
- Xcode: 26.2, build `17C52`
- Flutter: 3.38.10, framework revision `c6f67dede3`
- Appium: 3.5.2
- XCUITest driver: 11.16.2
- Signing: disabled, simulator build

The application was uninstalled and reinstalled before every run. The app's
LocalSend v2 HTTPS server listened on its standard port and the harness reached
it through simulator loopback. The sender was seeded, local, and bounded to one
text preview per request. No multicast discovery or external sender was used.

## Build and artifact identity

Both revisions were built with:

```sh
flutter pub get
dart run build_runner build --delete-conflicting-outputs
flutter build ios --simulator --debug --no-codesign
```

All three commands used pinned Flutter 3.38.10. Flutter 3.41.6 was rejected
before build because its SDK-pinned test dependencies could not solve this
revision's lock constraints.

The affected `App.framework/App` SHA-256 is
`33e8df4149b1a59d51d21d3d56b50e0c9285907193c02feaa11bbf5a3cec01c1`.
Its kernel blob SHA-256 is
`dc211a4d3e930a0902d61f77c3ce151c08ba9493d66b717146ff44747ad8899b`.

The fixed `App.framework/App` SHA-256 is
`1cc556bf1fa1fb0268074555987666fdd0ebd07e36df70e657a77ea3f2a1534b`.
Its kernel blob SHA-256 is
`ec338c1116aac1e14fcc3cc519313ed87d8af1bb95556b67d82fa4bf2dc9cacb`.

## Minimized trigger and observation

The harness sends this single text preview through the application's real v2
`prepare-upload` endpoint:

```text
https://example.com followed by text
```

The affected build renders `sent you a link:` and exposes an `Open` button.
The fixed build renders the same preview as a message and does not expose the
button. XCUITest reads both observables from the native accessibility tree.

After closing the trigger request, every run sends `https://example.com` as the
neighboring legal behavior. Both revisions classify that exact URL as a link
and expose `Open`. All three affected runs reproduced the exact identity. All
three fixed runs reached the observation without reproducing it. Every request
closed with HTTP 204 and every run reported an empty exception list.

## Cleanup

Each request was accepted and closed before the next request. LocalSend was
uninstalled between repetitions. The final campaign cleanup shuts down and
deletes the disposable simulator, moves temporary source and build artifacts to
the Trash, and stops the loopback Appium server.
