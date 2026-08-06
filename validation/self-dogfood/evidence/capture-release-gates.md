# Capture release gate evidence

GitHub Actions runs `31112776047`, `31114046982`, and `31114647749` tested the
capture release commits.

The `signature-parity` job ran the new eight-SDK producer conformance check.
The first run did not install .NET. The next run installed .NET, but the test
forced a path that did not match the setup action. The final test resolves the
executable through `DOTNET_ROOT`. Its shared producer conformance step passed
for all eight backend SDKs.

The final `sdk-backend-ports` job found a stale PHP PSR-15 assertion. The test
expected the adapter to queue failures without an effect-completeness proof.
The SDK correctly rejected those incomplete operations. The test now verifies
that rejection, then verifies upload eligibility when the adapter supplies an
effect-completeness proof and a captured dependency exchange.

These follow-up changes correct release gate configuration and test
expectations. They do not change product behavior. The capture ownership fixes
remain covered by the protocol, SDK, framework, native, and Cloud test suites.
