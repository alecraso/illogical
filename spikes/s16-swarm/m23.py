#!/usr/bin/env python3
"""M23's done-when: S16's `summary.py` load, measured on the real protocol.

The same daemon setup and driver as summary.py (N panes in tabs of 4 over 5
sessions, `--busy` of them starting a build-like command every ~5 s), but the
observer counts every text message it gets (`state`, `delta` and the rest),
so the numbers are what a client of an M23 daemon really receives.

    uv run --with websockets python3 spikes/s16-swarm/m23.py \
        --bin target/release/illogicald --panes 500 --busy 50 --observers 1
"""

import argparse
import asyncio
import json
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import summary  # noqa: E402  (S16's setup, driver and helpers)
from summary import bench, cpu_s, deflate_sizes, drive, open_panes  # noqa: E402


async def observe(port, seconds, record):
    ws, hello = await bench.ws_connect(port)
    record.append((time.time(), "hello", len(json.dumps(hello))))
    end = time.time() + seconds
    while time.time() < end:
        try:
            m = await asyncio.wait_for(ws.recv(), timeout=max(0.01, end - time.time()))
        except asyncio.TimeoutError:
            break
        if isinstance(m, bytes):
            continue
        record.append((time.time(), json.loads(m).get("type"), m))
    await ws.close()


async def scenario(a, d, ids):
    busy = ids[1:1 + a.busy]
    records = [[] for _ in range(a.observers)]
    log = []
    c0, t0 = cpu_s(d.pid), time.time()
    await asyncio.gather(drive(d, busy, a.seconds - 1, a.every, log), *[observe(d.port, a.seconds, r) for r in records])
    return records[0], log, (cpu_s(d.pid) - c0) / (time.time() - t0)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", required=True)
    ap.add_argument("--port", type=int, default=7797)
    ap.add_argument("--panes", type=int, default=500)
    ap.add_argument("--busy", type=int, default=50)
    ap.add_argument("--every", type=float, default=5.0)
    ap.add_argument("--seconds", type=float, default=60)
    ap.add_argument("--observers", type=int, default=1)
    a = ap.parse_args()
    os.makedirs(summary.WORK, exist_ok=True)
    tag = f"m23-p{a.panes}-b{a.busy}-o{a.observers}"
    d = bench.Daemon(os.path.abspath(a.bin), a.port, f"s16-{tag}")
    try:
        ids = open_panes(d, a.panes)
        time.sleep(8)
        c0 = cpu_s(d.pid)
        time.sleep(10)
        idle = (cpu_s(d.pid) - c0) / 10
        rec, log, cpu = asyncio.run(scenario(a, d, ids))
    finally:
        d.stop()
    secs = rec[-1][0] - rec[0][0] if len(rec) > 1 else a.seconds
    msgs = [m for _, _, m in rec[1:]]
    kinds = {}
    for _, k, _ in rec[1:]:
        kinds[k] = kinds.get(k, 0) + 1
    res = {
        "tag": tag, "panes": len(ids), "busy": a.busy, "observers": a.observers, "seconds": round(secs, 1),
        "commands": len(log), "hello_bytes": rec[0][2], "idle_cpu_pct": round(idle * 100, 1),
        "cpu_pct": round(cpu * 100, 1), "msgs": kinds,
        "bytes_per_s": round(sum(len(m) for m in msgs) / secs),
        "deflate_bytes_per_s": round(deflate_sizes(msgs) / secs),
    }
    with open(os.path.join(summary.WORK, f"{tag}.json"), "w") as f:
        json.dump(res, f, indent=1)
    print(json.dumps(res, indent=1))


if __name__ == "__main__":
    main()
