#!/usr/bin/env python3
"""firstresult.py NDJSON... — the first MCP tool result each Claude Code run saw."""
import json
import sys

for path in sys.argv[1:]:
    print("===", path)
    for line in open(path):
        e = json.loads(line)
        if e.get("type") != "user":
            continue
        done = False
        for c in e["message"]["content"]:
            if isinstance(c, dict) and c.get("type") == "tool_result" and "tool_reference" not in json.dumps(c.get("content")):
                s = c["content"] if isinstance(c["content"], str) else json.dumps(c["content"])
                print(len(s), "chars, is_error:", c.get("is_error"))
                print(s[:1600].replace("x" * 20, "x.."))
                done = True
                break
        if done:
            break
