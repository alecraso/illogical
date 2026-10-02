#!/usr/bin/env python3
"""Q1: what's in ~/.claude/projects. Counts line types, content blocks,
versions and entrypoints, and how often a file isn't one linear chain."""
import json, os, sys, collections, glob

root = os.path.expanduser(sys.argv[1] if len(sys.argv) > 1 else "~/.claude/projects")
C = collections.Counter
types, blocks, versions, entry, kinds_user, subtypes, extra = C(), C(), C(), C(), C(), C(), C()
files = top = sub = 0
branchy, multiroot, sidechain_files, bad = [], 0, 0, 0
sizes = []
for path in glob.glob(f"{root}/**/*.jsonl", recursive=True):
    files += 1
    rel = os.path.relpath(path, root)
    is_sub = "/subagents/" in path or rel.count("/") > 1
    sub += is_sub; top += not is_sub
    sizes.append(os.path.getsize(path))
    children = collections.defaultdict(list); uuids = set(); roots = 0; sidechain = False
    for line in open(path, errors="replace"):
        try: o = json.loads(line)
        except Exception: bad += 1; continue
        t = o.get("type"); types[t] += 1
        if o.get("subtype"): subtypes[f"{t}/{o['subtype']}"] += 1
        if o.get("version"): versions[o["version"].rsplit(".", 1)[0]] += 1
        if o.get("entrypoint") and not is_sub: entry[o["entrypoint"]] += 0  # counted per file below
        if o.get("isSidechain"): sidechain = True
        for k in ("isMeta", "isCompactSummary", "isVisibleInTranscriptOnly", "toolUseResult", "logicalParentUuid", "isApiErrorMessage"):
            if o.get(k): extra[k] += 1
        if t in ("user", "assistant") and isinstance(o.get("message"), dict):
            c = o["message"].get("content")
            if isinstance(c, str): blocks[f"{t}:str"] += 1
            elif isinstance(c, list):
                for b in c: blocks[f"{t}:{b.get('type')}"] += 1
        if "uuid" in o and t in ("user", "assistant", "system", "attachment"):
            uuids.add(o["uuid"])
            p = o.get("parentUuid")
            if p is None: roots += 1
            else: children[p].append(o["uuid"])
    sidechain_files += sidechain
    if any(len(v) > 1 for v in children.values()): branchy.append(rel)
    if roots > 1: multiroot += 1
    if not is_sub:
        for line in open(path, errors="replace"):
            try: o = json.loads(line)
            except Exception: continue
            if o.get("entrypoint"): entry[o["entrypoint"]] += 1; break

def show(name, c, n=40):
    print(f"\n## {name}")
    for k, v in c.most_common(n): print(f"{v:8} {k}")
print(f"files {files} (top-level {top}, subagent/nested {sub}), unparseable lines {bad}")
sizes.sort(); print(f"sizes: median {sizes[len(sizes)//2]//1024} KiB, p95 {sizes[int(len(sizes)*.95)]//1024} KiB, max {sizes[-1]//1048576} MiB, total {sum(sizes)//1048576} MiB")
print(f"files with a parent having >1 child (branches): {len(branchy)}; with >1 root: {multiroot}; with sidechain lines: {sidechain_files}")
show("line types", types); show("subtypes", subtypes); show("content blocks", blocks)
show("versions (major.minor)", versions, 20); show("entrypoint of top-level files", entry); show("flags", extra)
