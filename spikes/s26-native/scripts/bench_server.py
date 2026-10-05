#!/usr/bin/env python3
"""Serves S25's bench page and takes its result as a POST, for browsers on a
machine without Node:  bench_server.py DIR OUT.json [PORT]"""
import http.server, os, sys, threading

root, out = sys.argv[1], sys.argv[2]
port = int(sys.argv[3]) if len(sys.argv) > 3 else 8765
POST = b"""<script>addEventListener("load",()=>{const t=setInterval(()=>{if(window.__s25result){clearInterval(t);
fetch("/result",{method:"POST",body:JSON.stringify(window.__s25result)});}},500);});</script></body>"""

class H(http.server.SimpleHTTPRequestHandler):
    def __init__(self, *a, **k):
        super().__init__(*a, directory=root, **k)

    def do_GET(self):
        if self.path.startswith("/bench.html"):
            body = open(os.path.join(root, "bench.html"), "rb").read().replace(b"</body>", POST)
            self.send_response(200)
            self.send_header("Content-Type", "text/html")
            self.end_headers()
            self.wfile.write(body)
            return
        super().do_GET()

    def do_POST(self):
        data = self.rfile.read(int(self.headers["Content-Length"]))
        open(out, "wb").write(data)
        self.send_response(204)
        self.end_headers()
        print("result saved", flush=True)
        threading.Thread(target=self.server.shutdown).start()

    def log_message(self, *a):
        pass

http.server.ThreadingHTTPServer(("127.0.0.1", port), H).serve_forever()
