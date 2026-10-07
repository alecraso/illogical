#!/usr/bin/env python3
"""M27's stand-in for code-server: takes its flags, serves HTTP on the
--socket it's given, and says what it was started with.

Each start appends a line to $FAKE_CS_LOG: its pid, argv, and the
environment variables the tests look at. Every request appends one too.
`GET /` answers a page titled "fake code-server"; `GET /echo` answers the
request's path and headers as JSON. With $FAKE_CS_IDLE (seconds) it exits
after that long without a request, as code-server's idle timeout does.
"""
import http.server
import json
import os
import socketserver
import sys
import threading
import time

args = sys.argv[1:]


def flag(name):
    return args[args.index(name) + 1] if name in args else None


sock = flag("--socket")
log = os.environ.get("FAKE_CS_LOG", "/dev/null")
idle = float(os.environ.get("FAKE_CS_IDLE", "0"))
last = time.time()


def note(**kw):
    with open(log, "a") as f:
        f.write(json.dumps({"pid": os.getpid(), **kw}) + "\n")


env = {k: os.environ.get(k) for k in ("ARUGULA_SOCK", "ARUGULA_PANE", "VSCODE_IPC_HOOK_CLI", "NOTIFY_SOCKET")}
note(start=True, argv=args, env=env, pgid=os.getpgid(0) == os.getpid())


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_GET(self):
        global last
        last = time.time()
        note(path=self.path, host=self.headers.get("host"))
        if self.path.startswith("/echo"):
            body = json.dumps({"path": self.path, "headers": {k.lower(): v for k, v in self.headers.items()}}).encode()
            kind = "application/json"
        else:
            body = b"<html><head><title>fake code-server</title></head><body>workbench</body></html>"
            kind = "text/html"
        self.send_response(200)
        self.send_header("content-type", kind)
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *a):
        pass


class Server(socketserver.ThreadingMixIn, socketserver.UnixStreamServer):
    daemon_threads = True

    def get_request(self):
        r, _ = super().get_request()
        return r, ("local", 0)


if os.path.exists(sock):
    os.remove(sock)
srv = Server(sock, Handler)
os.chmod(sock, int(flag("--socket-mode") or "755", 8))
if idle:

    def watch():
        while time.time() - last < idle:
            time.sleep(0.1)
        os.remove(sock)
        os._exit(0)

    threading.Thread(target=watch, daemon=True).start()
srv.serve_forever()
