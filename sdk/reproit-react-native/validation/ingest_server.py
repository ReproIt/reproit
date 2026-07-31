#!/usr/bin/env python3
"""Stub ReproIt ingest. Writes each received batch to disk so the harness can
validate the real bytes the device sent."""
import http.server, json, os, sys

PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 19802
OUT = sys.argv[2] if len(sys.argv) > 2 else "/tmp/reproit-sim/ios/received"
os.makedirs(OUT, exist_ok=True)

class H(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        n = int(self.headers.get("Content-Length", "0"))
        raw = self.rfile.read(n)
        name = "capture-batch" if self.path.endswith("/v1/capture-batches") else "events"
        idx = len([f for f in os.listdir(OUT) if f.startswith(name)])
        with open(os.path.join(OUT, f"{name}-{idx}.json"), "wb") as fh:
            fh.write(raw)
        self.send_response(200)
        self.send_header("Access-Control-Allow-Origin", "*")
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(b'{"ok":true}')
    def do_OPTIONS(self):
        self.send_response(204)
        self.send_header("Access-Control-Allow-Origin", "*")
        self.send_header("Access-Control-Allow-Methods", "GET, POST, OPTIONS")
        self.send_header("Access-Control-Allow-Headers", "Content-Type, Authorization")
        self.end_headers()
    def log_message(self, *a): pass

http.server.HTTPServer(("127.0.0.1", PORT), H).serve_forever()
