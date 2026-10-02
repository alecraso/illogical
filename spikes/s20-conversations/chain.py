#!/usr/bin/env python3
"""Q1: which user/assistant lines does "the chain back from the last
user/assistant line, plus tool results by tool_use_id" leave out, and what
are they? Prints per-kind counts and a few examples."""
import json, glob, os, sys, collections

def load(path):
    rows = []
    for l in open(path, errors="replace"):
        try: rows.append(json.loads(l))
        except Exception: pass
    return rows

def chain(rows):
    ua = [o for o in rows if o.get("type") in ("user", "assistant") and not o.get("isSidechain") and o.get("uuid")]
    by = {o["uuid"]: o for o in rows if o.get("uuid")}
    if not ua: return set(), ua
    keep, u = set(), ua[-1]["uuid"]
    while u and u in by and u not in keep:
        keep.add(u); o = by[u]
        u = o.get("parentUuid") or o.get("logicalParentUuid")
    tools = {b["id"] for o in ua if o["uuid"] in keep and o["type"] == "assistant"
             for b in (o["message"].get("content") or []) if isinstance(b, dict) and b.get("type") == "tool_use"}
    for o in ua:
        c = o["message"].get("content")
        if o["type"] == "user" and isinstance(c, list) and c and all(b.get("type") == "tool_result" and b.get("tool_use_id") in tools for b in c):
            keep.add(o["uuid"])
    return keep, ua

def kind(o):
    c = o["message"].get("content")
    if isinstance(c, str): return f"{o['type']}:str" + (":meta" if o.get("isMeta") else "")
    return f"{o['type']}:" + ",".join(sorted({b.get('type','?') for b in c})) + (":meta" if o.get("isMeta") else "")

if __name__ == "__main__":
    root = os.path.expanduser("~/.claude/projects")
    dropped, total, files_dropping, ex = collections.Counter(), 0, 0, collections.defaultdict(list)
    for p in glob.glob(f"{root}/*/*.jsonl"):
        rows = load(p); keep, ua = chain(rows); total += len(ua)
        miss = [o for o in ua if o["uuid"] not in keep]
        files_dropping += bool(miss)
        for o in miss:
            k = kind(o); dropped[k] += 1
            if len(ex[k]) < 3: ex[k].append((os.path.relpath(p, root)[:70], str(o["message"].get("content"))[:100]))
    print(f"user/assistant lines {total}, dropped {sum(dropped.values())} in {files_dropping} files")
    for k, v in dropped.most_common(): print(v, k, *ex[k], sep="\n    ")
