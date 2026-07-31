#!/usr/bin/env python3
"""Serves the blank page the webview loads. The SDK modules and harness are
injected as WKUserScripts, so the page itself is intentionally empty."""
import http.server, sys
PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 19803
PAGE = b"<!doctype html><html><head><meta charset='utf-8'></head><body></body></html>"

class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(PAGE)))
        self.end_headers()
        self.wfile.write(PAGE)
    def log_message(self, *a): pass

http.server.HTTPServer(("127.0.0.1", PORT), H).serve_forever()
