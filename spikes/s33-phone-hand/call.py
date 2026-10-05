#!/usr/bin/env python3
"""Call an illogical MCP tool the way an agent would, through `illogical mcp`
on a daemon's socket, and print the result with how long it took.

    call.py SOCKET TOOL '{"json": "args"}'

Appends one line per call to results/calls.jsonl next to this script.
"""
import json, os, subprocess, sys, time

sock, tool = sys.argv[1], sys.argv[2]
args = json.loads(sys.argv[3]) if len(sys.argv) > 3 else {}
cli = os.environ.get("ILLOGICAL_CLI", "illogical")
p = subprocess.Popen([cli, "--socket", sock, "mcp"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)

def rpc(id, method, params):
    p.stdin.write(json.dumps({"jsonrpc": "2.0", "id": id, "method": method, "params": params}) + "\n")
    p.stdin.flush()
    while True:
        m = json.loads(p.stdout.readline())
        if m.get("id") == id:
            return m

rpc(1, "initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "s33-probe", "version": "1"}})
p.stdin.write(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n")
p.stdin.flush()
t0 = time.time()
r = rpc(2, "tools/call", {"name": tool, "arguments": args})
ms = round((time.time() - t0) * 1000)
p.terminate()
res = r.get("result", {})
out = res.get("structuredContent") or res.get("content")
print(json.dumps({"tool": tool, "ms": ms, "error": res.get("isError", False), "out": out}, indent=1))
os.makedirs(os.path.join(os.path.dirname(__file__), "results"), exist_ok=True)
with open(os.path.join(os.path.dirname(__file__), "results", "calls.jsonl"), "a") as f:
    f.write(json.dumps({"at": time.strftime("%H:%M:%S"), "tool": tool, "args": args, "ms": ms, "error": res.get("isError", False), "out": out}) + "\n")
