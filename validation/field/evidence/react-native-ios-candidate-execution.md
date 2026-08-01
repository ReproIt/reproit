# React Native iOS candidate execution

Companion prose for `react-native-ios-candidate-execution.json`. Both qualified
applications, BlueWallet and Joplin, were attempted for the first time on
darwin/arm64 with Xcode 26.2 (17C52), Node 26.5.0 and CocoaPods.

## Why this record exists

`validation/field/qualification/react-native-ios.json` recorded nine candidate
defects across two applications, every one of them `selected-not-executed` and
every `simulatorBuildWithoutSigning` an inspection rather than an executed
build. That blocker is now answered, and the answer is negative for both
applications.

## BlueWallet: excluded, not merely unbuilt

BlueWallet's `package.json` pins

    "rn-qr-generator": "https://github.com/BlueWallet/rn-qr-generator.git#731ed8eb445f65f3a659632232e18ff7e1ce56d6"

and `package-lock.json` resolves the same pin over `git+ssh`. The first install
attempt failed with a public-key denial, which looks like a local credential
problem. Rewriting ssh to https for that process alone, through
`GIT_CONFIG_COUNT` rather than the user's global config, produced the real
answer: `remote: Repository not found`. Both
`https://github.com/BlueWallet/rn-qr-generator` and the API endpoint for it
return 404. The repository has been removed upstream.

That dependency is present across the entire candidate range, so all five
BlueWallet candidates fall together. No working tree can be produced at any of
them without substituting a dependency the application pins by exact commit,
and substituting it would change the subject under test. This is a hard
exclusion rather than a cost.

## Joplin: one specific input away from an archive

Joplin gets much further. `yarn install` builds the whole workspace,
`pod install` resolves, and every native pod and every application source file
compiles. The build fails in the last phase, `Bundle React Native code and
images`:

    Error: Failed to get the SHA-1 for: .../packages/app-mobile/index.js.
      1) The file is not watched.

The phase script addresses the entry file through `/tmp` while Metro's haste map
holds the resolved `/private/tmp` path, so the entry file is not in Metro's
watched set. The working tree was under the `/tmp` symlink. The remaining input
is a build root that is not reached through a symlink; nothing about the
application, the revision pair, or the toolchain is implicated.

## Why the target still cannot be promoted even if Joplin builds

A campaign needs two independent applications. With BlueWallet excluded
outright, the qualified pool contains exactly one buildable application, so
Joplin alone cannot carry the target. The qualification pool has to be reopened
with at least one more React Native iOS application before a benchmark is
possible.

## Exact missing input

1. A build root outside the `/tmp` symlink, which unblocks the Joplin archive at
   both revisions of `joplin-note-row-touch-target-15972`.
2. A second independent React Native iOS application with a verified
   affected and fixed revision pair, to replace BlueWallet.
