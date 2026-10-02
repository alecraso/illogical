#!/usr/bin/env python3
"""S17: stand-in for illogicald's local socket. Accepts on a unix socket and appends every line it
gets, with the time it arrived, to a JSONL file.

usage: listen.py <socket-path> <out.jsonl>
"""
import json, os, socket, sys, threading, time

path, out = sys.argv[1], sys.argv[2]
if os.path.exists(path):
    os.unlink(path)
srv = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
srv.bind(path)
os.chmod(path, 0o600)
srv.listen(8)
f = open(out, "a")
lock = threading.Lock()


def serve(conn, n):
    buf = b""
    while True:
        d = conn.recv(65536)
        if not d:
            break
        buf += d
        while b"\n" in buf:
            line, buf = buf.split(b"\n", 1)
            try:
                msg = json.loads(line)
            except ValueError:
                continue
            msg["recv"] = time.time()
            msg["conn"] = n
            with lock:
                f.write(json.dumps(msg) + "\n")
                f.flush()


n = 0
while True:
    c, _ = srv.accept()
    n += 1
    threading.Thread(target=serve, args=(c, n), daemon=True).start()
