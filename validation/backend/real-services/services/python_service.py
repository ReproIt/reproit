import json
import os
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse

MODE = os.environ.get("REPROIT_FIXTURE_MODE", "clean")
PORT = int(os.environ.get("PORT", "19480"))
TAG = '"fixture-v1"'

class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_args):
        return

    def send_bytes(self, status, body, content_type="application/json", headers=None):
        self.send_response(status)
        if content_type is not None:
            self.send_header("Content-Type", content_type)
        for name, value in (headers or {}).items():
            self.send_header(name, value)
        self.end_headers()
        self.wfile.write(body)

    def send_json(self, status, value, headers=None):
        self.send_bytes(status, json.dumps(value, separators=(",", ":")).encode(), headers=headers)

    def do_GET(self):
        parsed = urlparse(self.path)
        if parsed.path == "/health":
            return self.send_json(200, {"ready": True})
        if parsed.path == "/codec":
            typed = parse_qs(parsed.query).get("value", [""])[0]
            decoded = format(float(typed), ".0f") if MODE == "broken" else typed
            return self.send_json(200, {} if MODE == "incomplete" else {"decoded": decoded})
        if parsed.path == "/representation":
            headers = {"ETag": TAG, "Vary": "accept-language"}
            if self.headers.get("If-None-Match") == TAG:
                if MODE == "broken":
                    return self.send_bytes(200, b"contradictory-v2", "text/plain", headers)
                return self.send_bytes(304, b"", "text/plain", headers)
            return self.send_bytes(200, b"authoritative-v1", "text/plain", headers)
        if parsed.path == "/media":
            body = b"{invalid-json" if MODE == "broken" else b'{"ok":true}'
            return self.send_bytes(200, body, None if MODE == "incomplete" else "application/json")
        if parsed.path == "/lifecycle":
            names = (["request.start", "request.close", "callback"] if MODE == "broken"
                     else ["request.start", "callback", "request.close"])
            return self.send_json(200, {
                "complete": MODE != "incomplete", "scopeKind": "request", "scopeId": "scope-1",
                "events": [{"sequence": i + 1, "name": name, "scopeId": "scope-1"}
                           for i, name in enumerate(names)]
            })
        self.send_bytes(404, b"")

print(f"READY {PORT}", flush=True)
ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
