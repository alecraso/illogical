"""A stand-in for an old arugulad (#317): answers /api/host as 0.8.0 (a
version and no protocol), 404s the rest of /api as 0.8.0 does (it has no
/api/update), and serves anything else a page of its own. Every request
goes to STATE/old-requests.log. Takes the daemon's flags (only --listen
and --state-dir matter), as the service's ExecStart= runs it.
"""

import json
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer

args = sys.argv[1:]
listen = args[args.index("--listen") + 1] if "--listen" in args else "127.0.0.1:7681"
state = args[args.index("--state-dir") + 1] if "--state-dir" in args else "."
host, port = listen.rsplit(":", 1)
VERSION = "0.8.0"


class Old(BaseHTTPRequestHandler):
    def do_GET(self):
        with open(f"{state}/old-requests.log", "a") as f:
            f.write(f"{self.command} {self.path}\n")
        status = 200
        if self.path.startswith("/api/host"):
            body, kind = json.dumps({"name": "old", "version": VERSION}).encode(), "application/json"
        elif self.path.startswith("/api/"):
            status, body, kind = 404, b"not found", "text/plain"
        else:
            body, kind = b"<!doctype html><title>arugula</title><p>arugulad 0.8.0's page</p>", "text/html"
        self.send_response(status)
        self.send_header("content-type", kind)
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    do_POST = do_GET

    def log_message(self, *a):
        pass


HTTPServer((host, int(port)), Old).serve_forever()
