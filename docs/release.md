# Release contract

Reproit 1.x releases use two Git tags with separate responsibilities:

- `v1.0.0`, `v1.0.1`, and later immutable tags identify exact CLI archives and
  the matching versioned SDK source tree.
- `v1` is the moving GitHub Action tag. The release workflow moves it only after every binary,
  checksum, installer, and installed-version gate passes.

The release workflow builds and installer-smokes native archives for macOS arm64 and x86_64, Linux
arm64 and x86_64, and Windows arm64 and x86_64. Installers require SHA-256 sidecars and reject a
binary whose reported version differs from the requested tag.

Publication also requires successful `ci.yml` and native evidence for the exact
commit being released. A success from another commit is not accepted.
The native workflow includes Linux host and container gates, a reset Android
emulator, and iOS simulators. Publication recomputes the captured log digests
for every release-tier gate: Web Chromium, React Native Android, Flutter iOS,
macOS AX, and Windows UIA. Native Windows UIA runs through the private
interactive x86_64 VM chain, and macOS AX runs in a permissioned desktop
session. Publication downloads the macOS AX result and captured log from the
exact-commit workflow artifacts. It downloads the Windows result and captured log
from a short-lived private evidence bundle, validates both manifests against the
registered gates, and recomputes each log's SHA-256. The verified results are
shipped as `reproit-native-evidence.json` in the release.

The CI command lines used by the composite Action and reusable workflow are recorded in
`validation/release/ci-invocations.txt`. The Rust test suite parses every entry with the production
CLI schema and checks the workflow-owned flags against that schema. Confirmation and minimization
are part of `reproit fuzz` by default; CI must not pass a separate compatibility flag for them.

`validation/release/check-version-contract.sh` keeps every owned, published CLI, runner, and SDK
manifest on the requested release version. The release workflow runs it before starting builds.
Registry publication is a separate operation until a registry and its install
smoke are explicitly listed in `sdk/README.md`; a manifest version alone does
not claim that a package exists in a registry.

To validate a release candidate without publishing it:

```sh
gh workflow run release.yml -f version=1.0.0 -f publish=false
```

Run the permissioned macOS gate through `native-gates.yml` with
`run_macos_ax=true`. Run the Windows gate in the native interactive environment:

```sh
python3 validation/backends/gate.py windows-uia
```

Package exactly `windows-uia.json` and `windows-uia.log` into a ZIP, place it at
a short-lived private URL, and set that URL as the repository secret
`WINDOWS_UIA_EVIDENCE_URL`. The release workflow rejects extra archive members,
the wrong commit, failed checks, manifest drift, or a log whose bytes do not
match the recorded digest.

After every gate succeeds for the same commit, rerun with `publish=true`.
Publishing creates the immutable version tag, uploads the checksummed assets,
marks the release latest, and then moves `v1` to the same commit.
