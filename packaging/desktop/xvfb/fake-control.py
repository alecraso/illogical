"""A stand-in for illogical control: enough for the app's sign-in to start.

POST /auth/app answers with a ticket, as control's app_login.rs does; every
request goes to the log. Writes the port it took to argv[1].
"""

import json
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer

port_file, log = sys.argv[1], sys.argv[2]


class Control(BaseHTTPRequestHandler):
    def note(self, body=b""):
        with open(log, "a") as f:
            f.write(f"{self.command} {self.path} {body.decode(errors='replace')}\n")

    def answer(self, status, body):
        out = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(out)))
        self.end_headers()
        self.wfile.write(out)

    def do_POST(self):
        body = self.rfile.read(int(self.headers.get("content-length") or 0))
        self.note(body)
        if self.path == "/auth/app":
            base = f"http://127.0.0.1:{self.server.server_port}"
            self.answer(200, {"ticket": "t1", "code": "WXYZ-1234", "url": f"{base}/#app=t1"})
        else:
            self.answer(404, {"error": "not here"})

    def do_GET(self):
        self.note()
        self.answer(404, {"error": "not here"})

    def log_message(self, *args):
        pass


srv = HTTPServer(("127.0.0.1", 0), Control)
with open(port_file, "w") as f:
    f.write(str(srv.server_port))
srv.serve_forever()
