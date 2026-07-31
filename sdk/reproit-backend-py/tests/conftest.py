"""Keep the capture tests hermetic against the CI environment.

`Capture.resolve_commit` deliberately falls back to REPROIT_COMMIT and then
GITHUB_SHA, which is correct behavior: a deployment should carry its code
identity without being told twice. But it means a test asserting an exact
`deployment` shape passes on a laptop and fails on a GitHub runner, where
GITHUB_SHA is always set. That is exactly what happened: test_capture.py and
test_e2e.py both pin `deployment == {"version": ...}` and both broke in CI while
staying green locally, so the failure looked like an SDK regression rather than
an ambient variable.

Clearing the two variables for every test makes the environment an input the
suite states rather than one it inherits. The fallback itself is proven with
explicit env dicts in test_instrument.py, and end to end in test_capture.py's
CI-identity test.
"""

import os

import pytest

AMBIENT_CODE_IDENTITY = ("REPROIT_COMMIT", "GITHUB_SHA")


@pytest.fixture(autouse=True)
def _no_ambient_code_identity(monkeypatch):
    for name in AMBIENT_CODE_IDENTITY:
        monkeypatch.delenv(name, raising=False)
    assert not any(name in os.environ for name in AMBIENT_CODE_IDENTITY)
