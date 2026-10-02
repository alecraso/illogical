#!/usr/bin/env python3
"""Q1, second rule. The conversation is the parentUuid chain back from the
last user/assistant line, where
  - a compact_boundary (no parentUuid) continues at its logicalParentUuid,
  - a parent that isn't in the file continues at the previous
    user/assistant line in file order,
plus every line whose tool results all answer a tool_use on the chain, and
every assistant line that shares a message.id with one on the chain (one
API response is written as one line per content block)."""
import json, glob, os, collections
from chain import load, kind

def conversation(rows):
    by = {o["uuid"]: o for o in rows if o.get("uuid")}
    pos = {o["uuid"]: i for i, o in enumerate(rows) if o.get("uuid")}
    ua = [o for o in rows if o.get("type") in ("user", "assistant") and not o.get("isSidechain") and o.get("uuid")]
    if not ua: return [], ua
    keep, u, fallbacks = set(), ua[-1]["uuid"], 0
    while u and u not in keep:
        if u not in by:  # a parent we don't have: step back in file order
            break
        keep.add(u); o = by[u]
        p = o.get("parentUuid") or (o.get("logicalParentUuid") if o.get("subtype") == "compact_boundary" else None)
        if p and p not in by:
            fallbacks += 1
            prev = [x for x in ua if pos[x["uuid"]] < pos[u]]
            p = prev[-1]["uuid"] if prev else None
        u = p
    on = [o for o in ua if o["uuid"] in keep]
    msg_ids = {o["message"].get("id") for o in on if o["type"] == "assistant"} - {None}
    tools = set()
    for o in ua:
        if o["type"] == "assistant" and (o["uuid"] in keep or o["message"].get("id") in msg_ids):
            keep.add(o["uuid"])
            tools |= {b["id"] for b in o["message"].get("content") or [] if isinstance(b, dict) and b.get("type") == "tool_use"}
    for o in ua:
        c = o["message"].get("content")
        if o["type"] == "user" and isinstance(c, list) and c and all(b.get("type") == "tool_result" and b.get("tool_use_id") in tools for b in c):
            keep.add(o["uuid"])
    return [o for o in rows if o.get("uuid") in keep], ua

if __name__ == "__main__":
    root = os.path.expanduser("~/.claude/projects")
    dropped, total, fd, ex = collections.Counter(), 0, collections.Counter(), collections.defaultdict(list)
    for p in glob.glob(f"{root}/*/*.jsonl"):
        rows = load(p); conv, ua = conversation(rows); total += len(ua)
        keep = {o["uuid"] for o in conv}
        miss = [o for o in ua if o["uuid"] not in keep]
        if miss: fd[os.path.basename(p)[:8]] = len(miss)
        for o in miss:
            k = kind(o); dropped[k] += 1
            if len(ex[k]) < 2: ex[k].append((os.path.basename(p)[:8], str(o["message"].get("content"))[:90]))
    print(f"user/assistant lines {total}, dropped {sum(dropped.values())} in {len(fd)} files: {dict(fd)}")
    for k, v in dropped.most_common(): print(v, k, *ex[k], sep="\n    ")
