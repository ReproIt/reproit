# Release contract

Reproit 1.x releases use two Git tags with separate responsibilities:

- `v1.0.0`, `v1.0.1`, and later immutable tags identify exact CLI and SDK releases.
- `v1` is the moving GitHub Action tag. The release workflow moves it only after every binary,
  checksum, installer, and installed-version gate passes.

The release workflow builds and installer-smokes native archives for macOS arm64 and x86_64, Linux
arm64 and x86_64, and Windows arm64 and x86_64. Installers require SHA-256 sidecars and reject a
binary whose reported version differs from the requested tag.

Publication also requires successful `ci.yml` and `native-gates.yml` runs for
the exact commit being released. A success from another commit is not accepted.
The native workflow includes Linux host and container gates, a reset Android
emulator, and iOS simulators. Native Windows UIA runs through the private
interactive x86_64 VM chain, and macOS AX runs in a permissioned desktop
session. Publication requires each gate's SHA-256 result and the exact Windows
evidence commit. The digests are shipped as `reproit-native-evidence.json` in
the release.

The CI command lines used by the composite Action and reusable workflow are recorded in
`validation/release/ci-invocations.txt`. The Rust test suite parses every entry with the production
CLI schema and checks the workflow-owned flags against that schema. Confirmation and minimization
are part of `reproit fuzz` by default; CI must not pass a separate compatibility flag for them.

`validation/release/check-version-contract.sh` keeps every owned, published CLI, runner, and SDK
manifest on the requested release version. The release workflow runs it before starting builds.

To validate a release candidate without publishing it:

```sh
gh workflow run release.yml -f version=1.0.0 -f publish=false
```

Run the permissioned gates and retain their result fields:

```sh
python3 validation/backends/gate.py macos-ax
python3 validation/backends/gate.py windows-uia
```

After every gate succeeds for the same commit, rerun with `publish=true` and
pass `windows_uia_commit`, `windows_uia_log_sha256`, and
`macos_ax_log_sha256`. Publishing creates the immutable version tag,
uploads the checksummed assets, marks the release latest, and then moves `v1` to the same commit.
