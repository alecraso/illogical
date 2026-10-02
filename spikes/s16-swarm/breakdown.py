#!/usr/bin/env python3
"""S16: where a 500-pane State's bytes go (sessions, tabs, panes; and within
a pane, which fields), to size what a summary must carry.

    uv run --with websockets python3 spikes/s16-swarm/breakdown.py --bin target/release/illogicald
"""

import argparse
import asyncio
import collections
import json
import os
import sys
import time

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "s9-memory"))
import bench  # noqa: E402

import summary  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))
bench.WORK = os.path.join(HERE, "work")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", required=True)
    ap.add_argument("--port", type=int, default=7797)
    a = ap.parse_args()
    d = bench.Daemon(os.path.abspath(a.bin), a.port, "s16-breakdown")
    try:
        ids = summary.open_panes(d, 500)
        time.sleep(8)
        # Ten panes mid-command, so `current` is filled in.
        for p in ids[1:11]:
            d.send(p, "sleep 30")
        time.sleep(7)

        async def hello():
            ws, h = await bench.ws_connect(a.port)
            await ws.close()
            return h

        st = asyncio.run(hello())["state"]
    finally:
        d.stop()
    c = summary.compact
    total = len(c(st))
    print(f"State: {total} bytes, {len(st['panes'])} panes, {len(st['tabs'])} tabs, {len(st['sessions'])} sessions")
    for k, v in st.items():
        print(f"  {k:<10} {len(c(v)):>8} bytes")
    fields = collections.Counter()
    for p in st["panes"]:
        for k, v in p.items():
            fields[k] += len(c(k)) + len(c(v)) + 2
    n = len(st["panes"])
    print("per pane, by field (average bytes):")
    for k, v in fields.most_common():
        print(f"  {k:<12} {v / n:6.1f}")
    print(f"tab average: {len(c(st['tabs'])) / len(st['tabs']):.0f} bytes; one tab: {c(st['tabs'][1])[:300]}")
    print(f"one busy pane: {c(st['panes'][2])}")


if __name__ == "__main__":
    main()
