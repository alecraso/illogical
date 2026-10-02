#!/usr/bin/env python3
"""S16: what server-side previews cost, through today's capture route.

A private daemon with 500 panes, 50 of them busy (summary.py's build-like
commands), and a client that captures the screen text of 50 panes once a
second, as the swarm would for panes zoomed in far enough to read. Reports
daemon CPU with and without the captures, capture latency, and the bytes a
"last 6 lines" preview would be against the whole screen.

    uv run --with websockets python3 spikes/s16-swarm/previews.py --bin target/release/illogicald
"""

import argparse
import asyncio
import concurrent.futures
import json
import os
import sys
import time

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "s9-memory"))
import bench  # noqa: E402

import summary  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))
WORK = os.path.join(HERE, "work")
bench.WORK = WORK


def pct(xs, p):
    s = sorted(xs)
    return s[min(len(s) - 1, int(p / 100 * len(s)))] if s else 0


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", required=True)
    ap.add_argument("--port", type=int, default=7796)
    ap.add_argument("--seconds", type=float, default=30)
    ap.add_argument("--visible", type=int, default=50)
    a = ap.parse_args()
    d = bench.Daemon(os.path.abspath(a.bin), a.port, "s16-previews")
    res = {}
    try:
        ids = summary.open_panes(d, 500)
        time.sleep(8)
        busy = ids[1:51]

        async def load(seconds):
            await summary.drive(d, busy, seconds, 5.0, [])

        # Busy, no previews.
        c0, t0 = summary.cpu_s(d.pid), time.time()
        asyncio.run(load(a.seconds))
        res["cpu_pct_busy_no_previews"] = round((summary.cpu_s(d.pid) - c0) / (time.time() - t0) * 100, 1)

        # Busy, plus 50 captures a second: half busy panes, half idle ones.
        visible = busy[: a.visible // 2] + ids[100:100 + a.visible // 2]
        lat, full_bytes, six_bytes = [], [], []

        def one(p):
            t = time.time()
            txt = d.http("GET", f"/api/panes/{p}/capture?format=text")
            lat.append((time.time() - t) * 1000)
            txt = str(txt)
            full_bytes.append(len(txt.encode()))
            lines = [x for x in txt.rstrip("\n").split("\n")]
            six_bytes.append(len("\n".join(lines[-6:]).encode()))

        stop = time.time() + a.seconds

        def previews():
            with concurrent.futures.ThreadPoolExecutor(8) as ex:
                while time.time() < stop:
                    tick = time.time()
                    list(ex.map(one, visible))
                    time.sleep(max(0, 1 - (time.time() - tick)))

        c0, t0 = summary.cpu_s(d.pid), time.time()

        async def both():
            loop = asyncio.get_running_loop()
            await asyncio.gather(load(a.seconds), loop.run_in_executor(None, previews))

        asyncio.run(both())
        res["cpu_pct_busy_with_previews"] = round((summary.cpu_s(d.pid) - c0) / (time.time() - t0) * 100, 1)
        res["captures"] = len(lat)
        res["capture_ms_p50"] = round(pct(lat, 50), 2)
        res["capture_ms_p95"] = round(pct(lat, 95), 2)
        res["screen_bytes_avg"] = round(sum(full_bytes) / len(full_bytes))
        res["last6_bytes_avg"] = round(sum(six_bytes) / len(six_bytes))
        res["last6_bytes_per_s_for_visible"] = round(sum(six_bytes) / a.seconds)
    finally:
        d.stop()
    with open(os.path.join(WORK, "previews.json"), "w") as f:
        json.dump(res, f, indent=1)
    print(json.dumps(res, indent=1))


if __name__ == "__main__":
    main()
