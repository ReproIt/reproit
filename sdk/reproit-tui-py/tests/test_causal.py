import io
import json
import os
import tempfile
import urllib.request

from reproit_tui_py.causal import _redact, install_causal_urllib


def test_explicit_secret_keys_redact_without_hiding_ordinary_keys():
    safe = _redact({
        "apiKey": "raw-api", "publishable-key": "raw-pub", "private_key": "raw-private",
        "access.key": "raw-access", "signing key": "raw-signing",
        "keyboardLayout": "dvorak", "key": "ordinary",
    })
    assert safe["keyboardLayout"] == "dvorak" and safe["key"] == "ordinary"
    assert not any(raw in json.dumps(safe) for raw in ["raw-api", "raw-pub", "raw-private", "raw-access", "raw-signing"])


class FakeResponse:
    status = 200
    headers = {"Content-Type": "application/json"}

    def read(self):
        return json.dumps({"profile": {"email": "a@example.com"}, "ok": True}).encode()

    def close(self):
        pass


def test_capture_uses_side_files_and_redacts_before_persistence():
    with tempfile.TemporaryDirectory() as directory:
        network = os.path.join(directory, "network.ndjson")
        action = os.path.join(directory, "action.txt")
        capabilities = os.path.join(directory, "capabilities.json")
        open(network, "w").close()
        with open(action, "w") as handle:
            handle.write("3")
        with open(capabilities, "w") as handle:
            handle.write("{}")
        prior = urllib.request.urlopen
        urllib.request.urlopen = lambda *_a, **_k: FakeResponse()
        os.environ.update({
            "REPROIT_NETWORK_FILE": network,
            "REPROIT_ACTION_FILE": action,
            "REPROIT_CAPABILITIES_FILE": capabilities,
            "REPROIT_DEVICE": "b",
        })
        restore = install_causal_urllib()
        try:
            request = urllib.request.Request(
                "https://app.test/feed",
                data=json.dumps({"token": "raw"}).encode(),
                headers={"Authorization": "raw", "Content-Type": "application/json"},
                method="POST",
            )
            assert urllib.request.urlopen(request).status == 200
            with open(network) as handle:
                exchange = json.loads(handle.read())
            assert exchange["actor"] == "b" and exchange["actionIndex"] == 3
            assert exchange["requestBody"]["token"] == "<reproit:string:length=3>"
            assert exchange["responseBody"]["profile"]["email"] == "<reproit:string:length=13>"
            with open(capabilities) as handle:
                assert json.load(handle)["http"]["status"] == "captured"
        finally:
            restore()
            urllib.request.urlopen = prior
            for key in ["REPROIT_NETWORK_FILE", "REPROIT_ACTION_FILE", "REPROIT_CAPABILITIES_FILE", "REPROIT_DEVICE"]:
                os.environ.pop(key, None)
