# Capture release gate evidence

GitHub Actions run `31112776047` tested capture commit
`f66f4af2136152fa10dbf017b28a3cc0141a300c`.

The `signature-parity` job ran the new eight-SDK producer conformance check.
Seven producers passed. The .NET check could not start because the job did not
install .NET, and `spawnSync` returned `ENOENT`. The dedicated .NET job already
uses `actions/setup-dotnet` with .NET 8. The parity job now installs the same
toolchain before it runs the shared producer check.

The `dogfood-policy` job also rejected the capture commit because its message
did not include a `Reproit-Dogfood` trailer. The product regressions in that
commit remain covered by the protocol, SDK, framework, native, and Cloud test
suites. This follow-up changes release gate configuration only.
