#!/usr/bin/env python3
"""Group a process's mappings by kind and size: which anon regions hold RSS.

    python3 smaps_breakdown.py PID
"""
import collections, sys

pid = sys.argv[1]
maps = []
cur = None
with open(f"/proc/{pid}/smaps") as f:
    for line in f:
        parts = line.split()
        if "-" in parts[0] and len(parts[0].split("-")) == 2 and not parts[0].endswith(":"):
            a, b = (int(x, 16) for x in parts[0].split("-"))
            cur = {"size": (b - a) // 1024, "perm": parts[1], "name": parts[5] if len(parts) > 5 else "", "rss": 0, "anon": 0}
            maps.append(cur)
        elif parts[0] == "Rss:":
            cur["rss"] = int(parts[1])
        elif parts[0] == "Anonymous:":
            cur["anon"] = int(parts[1])

groups = collections.defaultdict(lambda: [0, 0, 0])
for m in maps:
    if m["name"] and not m["name"].startswith("["):
        key = ("file", m["name"].rsplit("/", 1)[-1], "")
    elif m["name"]:
        key = (m["name"], "", "")
    else:
        key = ("anon", m["perm"], f"{m['size']}K")
    g = groups[key]
    g[0] += 1
    g[1] += m["rss"]
    g[2] += m["size"]
tot = sum(g[1] for g in groups.values())
print(f"total rss {tot/1024:.1f} MiB over {len(maps)} mappings")
for k, (n, rss, size) in sorted(groups.items(), key=lambda kv: -kv[1][1])[:25]:
    print(f"{rss/1024:9.1f} MiB rss  {n:5d} maps  {size/1024:10.1f} MiB virt  {' '.join(k)}")
