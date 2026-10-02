#!/usr/bin/env python3
"""show.py work/NAME.log — print the server's event log compactly."""
import json
import sys

t0 = None
for line in open(sys.argv[1]):
    try:
        e = json.loads(line)
    except json.JSONDecodeError:
        print("?? bad line", line[:120])
        continue
    t0 = t0 or e["t"]
    t = f"{e['t'] - t0:8.2f}"
    d = e["data"]
    if e["event"] == "http":
        b = d["body"]
        hdr = {k: v for k, v in d["headers"].items() if k.startswith("mcp-") or k in ("origin", "user-agent", "host", "accept", "authorization")}
        print(t, d["method"], json.dumps(b)[:300] if b else "", json.dumps(hdr))
    elif e["event"] == "http.resp":
        print(t, "   ->", d["status"], d["ct"])
    else:
        print(t, e["event"], json.dumps(d)[:400])
