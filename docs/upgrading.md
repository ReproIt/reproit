# Upgrading ReproIt

## Upgrade within 1.x

1. Commit or back up `reproit.yaml`, `.reproit/repros`, and authored journeys.
2. Read [CHANGELOG.md](../CHANGELOG.md) for behavior and prerequisite changes.
3. Run `reproit update`, or install an explicit immutable version.
4. Run `reproit doctor` in each configured application.
5. Run `reproit check` before accepting the upgrade in CI.

ReproIt refreshes regenerable map state when the CLI or application inputs
change. Do not delete saved repros to resolve an upgrade problem. Report a
compatibility defect with the prior and new CLI versions.

## Pinning

CI should pin an immutable `v1.x.y` release. The `v1` GitHub Action tag moves to
the latest validated 1.x release and is intended for teams that deliberately
accept compatible updates.

SDK source dependencies must use an immutable `v1.x.y` tag. Keep the CLI and SDK
on the same minor version when practical; the version 1 wire protocol permits
independent patch updates.
