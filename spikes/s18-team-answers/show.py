#!/usr/bin/env python3
"""S18: print recorded hook inputs compactly. usage: show.py <dir> [prefix...]
Drops the long path fields; prints each file's reply (.out) and timing log too."""
import json, os, sys

d = sys.argv[1]
prefixes = sys.argv[2:]
for f in sorted(os.listdir(d)):
    if not f.endswith(".json") or (prefixes and not any(f.startswith(p) for p in prefixes)):
        continue
    j = json.load(open(os.path.join(d, f)))
    for k in ("transcript_path", "scratchpad_dir", "cwd", "session_id", "prompt_id"):
        j.pop(k, None)
    print("==", f)
    print(" ", json.dumps(j)[:900])
    base = os.path.join(d, f[:-5])
    if os.path.exists(base + ".out"):
        print("  reply:", open(base + ".out").read()[:300])
    if os.path.exists(base + ".log"):
        print("  log:", " | ".join(open(base + ".log").read().split("\n")).strip(" |"))
