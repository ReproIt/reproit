#!/usr/bin/env python3
import json
import os
import tempfile
import urllib.request

from reproit_linux.causal import _redact, install_causal_urllib


def main():
    safe = _redact({
        "apiKey": "raw-api", "publishable-key": "raw-pub", "private_key": "raw-private",
        "access.key": "raw-access", "signing key": "raw-signing",
        "keyboardLayout": "dvorak", "key": "ordinary",
    })
    assert safe["keyboardLayout"] == "dvorak" and safe["key"] == "ordinary"
    assert not any(raw in json.dumps(safe) for raw in ["raw-api", "raw-pub", "raw-private", "raw-access", "raw-signing"])
    with tempfile.TemporaryDirectory() as directory:
        capsule = os.path.join(directory, "capsule.json")
        action = os.path.join(directory, "action.txt")
        capabilities = os.path.join(directory, "capabilities.json")
        with open(capsule, "w", encoding="utf-8") as handle:
            json.dump({"exchanges": [{
                "id": "a-1-0", "actor": "a", "actionIndex": 1, "ordinal": 0,
                "protocol": "https", "method": "GET",
                "url": "https://example.test/api?a=1&b=2", "status": 200,
                "responseHeaders": {"content-type": "application/json"},
                "responseBody": {"ok": True}, "required": True,
            }]}, handle)
        with open(action, "w", encoding="utf-8") as handle:
            handle.write("1")
        with open(capabilities, "w", encoding="utf-8") as handle:
            handle.write("{}")
        old = dict(os.environ)
        try:
            os.environ.update({
                "REPROIT_CAPSULE": capsule,
                "REPROIT_ACTION_FILE": action,
                "REPROIT_CAPABILITIES_FILE": capabilities,
                "REPROIT_DEVICE": "a",
            })
            restore = install_causal_urllib()
            with urllib.request.urlopen("https://example.test/api?b=2&a=1") as response:
                assert json.loads(response.read()) == {"ok": True}
            try:
                urllib.request.urlopen("https://example.test/miss")
                raise AssertionError("unmatched request reached the network")
            except RuntimeError as error:
                assert "CAPSULE:MISS" in str(error)
            with open(capabilities, encoding="utf-8") as handle:
                assert json.load(handle)["http_replay"]["status"] == "captured"
            restore()
        finally:
            os.environ.clear()
            os.environ.update(old)


if __name__ == "__main__":
    main()
