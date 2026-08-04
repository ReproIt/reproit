# Release contract

Immutable `v1.x.y` tags identify CLI archives and the matching SDK source tree.
The moving `v1` GitHub Action tag advances only after the exact release commit
passes publication.

Publication requires:

- green `ci.yml` and exact-commit native evidence;
- checksummed installer smoke tests on macOS, Linux, and Windows for arm64 and
  x86_64;
- manifest, captured-log digest, reset, cleanup, and required-output validation;
- a matching version across every published CLI, runner, and SDK manifest; and
- successful parsing of every workflow command by the production CLI schema.

`reproit-protocol` has an independent semantic version. Its `contract.json`
wire ledger must match the compiled constants and package manifest, and
`cargo package --locked -p reproit-protocol` must pass. Publishing that crate is
a separate, explicitly authorized release action. CLI and Cloud release
versions do not imply protocol compatibility.

The native evidence bundle covers all release-owned browser, mobile, desktop,
terminal, webview, and backend gates. Permissioned macOS AX and Windows UIA
evidence must come from their registered interactive environments.

Release availability and field evidence are separate. Native evidence proves an
integration works at one commit. The independent affected-versus-fixed
campaigns recorded in [compatibility.md](compatibility.md) prove it finds real
defects in real applications.

Validate without publishing:

```sh
gh workflow run release.yml -f version=1.0.0 -f publish=false
```

Run permissioned gates through `native-gates.yml`. The release workflow rejects
evidence from another commit, unexpected archive members, failed checks,
manifest drift, or mismatched log bytes. After every gate succeeds for the same
commit, rerun with `publish=true`.
