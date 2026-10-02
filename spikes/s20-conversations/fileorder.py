#!/usr/bin/env python3
"""Q1, the rule M33 takes: file order. Checks that every tool_result comes
after its tool_use, and counts rewinds: a prompt (user text, not meta) whose
parent is a user/assistant line that already had a later user/assistant child
written before it (the prompt was re-sent from an earlier point)."""
import json, glob, os, collections
from chain import load
root = os.path.expanduser("~/.claude/projects")
orphan = late = rewinds = files_rw = roots2 = 0; ex = []
for p in glob.glob(f"{root}/*/*.jsonl"):
    rows = load(p); seen_tool = set(); kids = collections.Counter(); by = {}
    rw = 0; nroots = 0
    for o in rows:
        if o.get("type") not in ("user", "assistant") or o.get("isSidechain"): continue
        c = o["message"].get("content")
        if o["type"] == "assistant":
            for b in c or []:
                if isinstance(b, dict) and b.get("type") == "tool_use": seen_tool.add(b["id"])
        elif isinstance(c, list):
            for b in c:
                if isinstance(b, dict) and b.get("type") == "tool_result" and b.get("tool_use_id") not in seen_tool: orphan += 1
        prompt = o["type"] == "user" and not o.get("isMeta") and (isinstance(c, str) or any(b.get("type") == "text" for b in c))
        par = o.get("parentUuid")
        if prompt and par is None and by: nroots += 1
        if prompt and par in by and kids[par] > 0:
            rw += 1
            if len(ex) < 4: ex.append((os.path.basename(p)[:8], str(c)[:60]))
        if par: kids[par] += 1
        by[o["uuid"]] = o
    rewinds += rw + nroots; files_rw += bool(rw or nroots)
print(f"tool_results before their tool_use: {orphan}")
print(f"rewinds (re-sent from an earlier point, or a second root): {rewinds} in {files_rw} files")
for e in ex: print("   ", e)
