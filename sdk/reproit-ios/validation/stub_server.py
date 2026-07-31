#!/usr/bin/env python3
"""Local stub dependency for the simulator run. Returns a JSON payload that
carries a secret-shaped field, so redaction is provable on device."""
import http.server, json, sys, threading

PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 19801

class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = json.dumps({
            "prices": None,
            "apiKey": "sk-live-SHOULD-NEVER-LEAVE-DEVICE",
            "tier": "gold",
        }).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, *a): pass

http.server.HTTPServer(("127.0.0.1", PORT), H).serve_forever()
