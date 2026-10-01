#!/usr/bin/env python3
"""A stdio MCP server whose tools ask the user through MCP elicitation, as
S13's mcp-elicit.mjs did, without the SDK: newline-delimited JSON-RPC.

  pick_size   a form: size (enum + enumNames), qty (integer), gift (boolean)
  sign_in     a URL elicitation; on accept, notifications/elicitation/complete

Each tool returns what the client answered. With a path as its argument,
it logs every message it gets there.
"""

import json
import sys
import threading

lock = threading.Lock()
next_id = [0]
waiting = {}  # our request id -> [event, response]


def send(m):
    m["jsonrpc"] = "2.0"
    with lock:
        sys.stdout.write(json.dumps(m) + "\n")
        sys.stdout.flush()


def elicit(params):
    rid = f"e{next_id[0]}"
    next_id[0] += 1
    ev = threading.Event()
    waiting[rid] = [ev, None]
    send({"id": rid, "method": "elicitation/create", "params": params})
    ev.wait()
    return waiting.pop(rid)[1]


def call(mid, name):
    if name == "pick_size":
        r = elicit({"mode": "form", "message": "Order details", "requestedSchema": {
            "type": "object", "properties": {
                "size": {"type": "string", "title": "Size", "enum": ["S", "M", "L"],
                         "enumNames": ["Small", "Medium", "Large"]},
                "qty": {"type": "integer", "title": "Quantity", "minimum": 1, "maximum": 9},
                "gift": {"type": "boolean", "title": "Gift wrap"}},
            "required": ["size"]}})
        text = f"elicitation result: {json.dumps(r)}"
    elif name == "sign_in":
        eid = "fake-signin-1"
        r = elicit({"mode": "url", "message": "Sign in to Fake", "url": "https://example.com/fake-signin",
                    "elicitationId": eid})
        if (r or {}).get("action") == "accept":
            send({"method": "notifications/elicitation/complete", "params": {"elicitationId": eid}})
        text = f"url elicitation result: {json.dumps(r)}"
    else:
        send({"id": mid, "error": {"code": -32602, "message": f"no tool {name}"}})
        return
    send({"id": mid, "result": {"content": [{"type": "text", "text": text}]}})


def handle(m):
    method, mid, p = m.get("method"), m.get("id"), m.get("params") or {}
    if method is None:
        if mid in waiting:
            waiting[mid][1] = m.get("result") if "result" in m else {"error": m.get("error")}
            waiting[mid][0].set()
        return
    if method == "initialize":
        send({"id": mid, "result": {"protocolVersion": p.get("protocolVersion", "2025-11-25"),
                                    "capabilities": {"tools": {}},
                                    "serverInfo": {"name": "fake", "version": "1"}}})
    elif method == "tools/list":
        send({"id": mid, "result": {"tools": [
            {"name": "pick_size", "description": "Ask the user for a t-shirt size and a quantity, through a form.",
             "inputSchema": {"type": "object", "properties": {}}},
            {"name": "sign_in", "description": "Ask the user to open a sign-in link.",
             "inputSchema": {"type": "object", "properties": {}}}]}})
    elif method == "tools/call":
        threading.Thread(target=call, args=(mid, p.get("name")), daemon=True).start()
    elif method == "ping":
        send({"id": mid, "result": {}})
    elif mid is not None:
        send({"id": mid, "error": {"code": -32601, "message": f"no {method}"}})


LOG = open(sys.argv[1], "a") if len(sys.argv) > 1 else None

for line in sys.stdin:
    line = line.strip()
    if LOG:
        LOG.write(line + "\n")
        LOG.flush()
    if line:
        try:
            handle(json.loads(line))
        except Exception as e:  # noqa: BLE001
            print("fake-mcp:", e, file=sys.stderr, flush=True)
