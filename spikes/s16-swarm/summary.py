#!/usr/bin/env python3
"""S16: what a swarm view's summary of every pane costs, today and as deltas.

Starts a private illogicald (as S9 did), opens N panes in tabs of 4 across 5
sessions, keeps `--busy` of them running build-like commands, and records
every message a plain `/ws` observer gets. Today that's a whole `State` after
each change. From the same recording it works out what a delta stream would
cost: field-level changes per pane, coalesced over a 0.5 s or 1 s window,
plus a modelled `activity` field for each pane that printed in the window.

    uv run --with websockets python3 spikes/s16-swarm/summary.py \
        --bin target/release/illogicald --panes 500 --busy 50 --observers 1

Results go to work/summary-<tag>.json and a table to stdout.
"""

import argparse
import asyncio
import json
import os
import random
import sys
import time
import zlib

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "s9-memory"))
import bench  # noqa: E402  (S9's Daemon, smaps, descendants)

HERE = os.path.dirname(os.path.abspath(__file__))
WORK = os.path.join(HERE, "work")
bench.WORK = WORK
CLK = os.sysconf("SC_CLK_TCK")

# A build-like command: ~2 s of steady output, so it starts, works and ends.
BUSY_CMD = "for i in $(seq 1 20); do echo \"   Compiling crate-$i v0.1.$RANDOM\"; sleep 0.1; done"


def cpu_s(pid):
    with open(f"/proc/{pid}/stat") as f:
        s = f.read()
    rest = s[s.rindex(")") + 2:].split()
    return (int(rest[11]) + int(rest[12])) / CLK


def compact(o):
    return json.dumps(o, separators=(",", ":"))


def open_panes(d, n):
    """n panes, in tabs of 4 (a tab and three splits), over 5 sessions."""
    ids = []
    first = d.panes()
    ids.extend(first)
    s = 0
    while len(ids) < n:
        root = d.http("POST", "/api/run", {"session": f"s{s % 5}"})["pane"]
        s += 1
        ids.append(root)
        for _ in range(3):
            if len(ids) >= n:
                break
            ids.append(d.http("POST", "/api/run", {"split": root})["pane"])
    d.wait_prompt(ids[-4:])
    return ids


# ---------------------------------------------------------------- deltas


def by_id(items):
    return {x["id"]: x for x in items}


def diff(prev, cur):
    """Field-level changes from one State to the next."""
    out = {}
    pp, cp = by_id(prev["panes"]), by_id(cur["panes"])
    panes = {}
    for pid, p in cp.items():
        q = pp.get(pid)
        if q is None:
            panes[pid] = p
            continue
        ch = {k: v for k, v in p.items() if q.get(k) != v}
        if ch:
            panes[pid] = ch
    gone = [pid for pid in pp if pid not in cp]
    if panes:
        out["panes"] = panes
    if gone:
        out["closed"] = gone
    pt, ct = by_id(prev["tabs"]), by_id(cur["tabs"])
    tabs = [t for tid, t in ct.items() if pt.get(tid) != t]
    if tabs:
        out["tabs"] = tabs
    for k in ("sessions", "machines", "options", "roles"):
        if prev.get(k) != cur.get(k):
            out[k] = cur.get(k)
    return out


def merge(acc, d):
    for pid, ch in d.get("panes", {}).items():
        acc.setdefault("panes", {}).setdefault(pid, {}).update(ch)
    for k in ("closed",):
        if k in d:
            acc.setdefault(k, []).extend(d[k])
    if "tabs" in d:
        t = {x["id"]: x for x in acc.get("tabs", [])}
        t.update({x["id"]: x for x in d["tabs"]})
        acc["tabs"] = list(t.values())
    for k in ("sessions", "machines", "options", "roles"):
        if k in d:
            acc[k] = d[k]


def delta_stream(states, window, busy_windows):
    """Coalesce consecutive diffs into one message per `window` seconds.

    `busy_windows[i]` is the set of panes that printed in window i; each gets
    a modelled `activity` field, as M23 would send.
    """
    if not states:
        return [], []
    t0 = states[0][0]
    msgs, sizes = [], []
    acc, cur_w = {}, 0
    prev = states[0][1]
    rnd = random.Random(16)

    def flush(w):
        nonlocal acc
        for pid in busy_windows.get(w, ()):
            acc.setdefault("panes", {}).setdefault(pid, {})["activity"] = {
                "bps": rnd.randrange(200, 40_000), "last_ms": 1_790_000_000_000 + rnd.randrange(10**6)}
        if acc:
            m = compact({"type": "summary", **acc})
            msgs.append(m)
            sizes.append(len(m))
        acc = {}

    for t, st in states[1:]:
        w = int((t - t0) / window)
        while cur_w < w:
            flush(cur_w)
            cur_w += 1
        merge(acc, diff(prev, st))
        prev = st
    flush(cur_w)
    return msgs, sizes


def deflate_sizes(msgs):
    """Bytes with permessage-deflate (one context for the connection)."""
    c = zlib.compressobj(6, zlib.DEFLATED, -15)
    total = 0
    for m in msgs:
        total += len(c.compress(m.encode()) + c.flush(zlib.Z_SYNC_FLUSH)) - 4
    return total


# ---------------------------------------------------------------- run


async def observe(port, seconds, record):
    ws, hello = await bench.ws_connect(port)
    record.append((time.time(), len(json.dumps(hello)), hello["state"]))
    end = time.time() + seconds
    while time.time() < end:
        try:
            m = await asyncio.wait_for(ws.recv(), timeout=max(0.01, end - time.time()))
        except asyncio.TimeoutError:
            break
        if isinstance(m, bytes):
            continue
        j = json.loads(m)
        if j.get("type") == "state":
            record.append((time.time(), len(m), j["state"]))
    await ws.close()


async def drive(d, busy, seconds, per_pane_s, log):
    """Each busy pane starts a command every ~per_pane_s seconds."""
    rnd = random.Random(7)
    loop = asyncio.get_running_loop()
    end = time.time() + seconds
    nxt = {p: time.time() + rnd.expovariate(1 / per_pane_s) for p in busy}
    while time.time() < end:
        now = time.time()
        due = [p for p, t in nxt.items() if t <= now]
        for p in due:
            nxt[p] = now + 2.5 + rnd.expovariate(1 / per_pane_s)
            log.append((now, p))
            await loop.run_in_executor(None, d.send, p, BUSY_CMD)
        await asyncio.sleep(0.02)


async def scenario(a, d, ids):
    busy = ids[1:1 + a.busy]
    records = [[] for _ in range(a.observers)]
    log = []
    c0, t0 = cpu_s(d.pid), time.time()
    obs = [observe(d.port, a.seconds, r) for r in records]
    await asyncio.gather(drive(d, busy, a.seconds - 1, a.every, log), *obs)
    c1, t1 = cpu_s(d.pid), time.time()
    return records[0], log, (c1 - c0) / (t1 - t0)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", required=True)
    ap.add_argument("--port", type=int, default=7750)
    ap.add_argument("--panes", type=int, default=500)
    ap.add_argument("--busy", type=int, default=50)
    ap.add_argument("--every", type=float, default=5.0, help="mean seconds between commands per busy pane")
    ap.add_argument("--seconds", type=float, default=60)
    ap.add_argument("--observers", type=int, default=1)
    ap.add_argument("--tag", default="")
    a = ap.parse_args()
    tag = a.tag or f"p{a.panes}-b{a.busy}-o{a.observers}"
    os.makedirs(WORK, exist_ok=True)

    d = bench.Daemon(os.path.abspath(a.bin), a.port, f"s16-{tag}")
    try:
        t = time.time()
        ids = open_panes(d, a.panes)
        print(f"opened {len(ids)} panes in {time.time() - t:.1f}s", flush=True)
        time.sleep(8)  # let the shells settle and the 5 s refresh pass
        idle_c0 = cpu_s(d.pid)
        time.sleep(10)
        idle_cpu = (cpu_s(d.pid) - idle_c0) / 10
        rec, log, cpu = asyncio.run(scenario(a, d, ids))
        rss = bench.smaps(d.pid)["rss"]
    finally:
        d.stop()

    secs = rec[-1][0] - rec[0][0] if len(rec) > 1 else a.seconds
    states = [(t, st) for t, _, st in rec]
    full = [n for _, n, _ in rec[1:]]
    full_msgs = [compact(st) for _, st in states[1:]]
    # Which panes printed in each window, from the driver's log (a command
    # prints for ~2 s after it's sent).
    res = {
        "tag": tag, "panes": len(ids), "busy": a.busy, "observers": a.observers,
        "seconds": round(secs, 1), "commands": len(log),
        "hello_bytes": rec[0][1], "daemon_rss_mib": round(rss / 1024),
        "idle_cpu_pct": round(idle_cpu * 100, 1), "cpu_pct": round(cpu * 100, 1),
        "state_msgs": len(full), "state_msgs_per_s": round(len(full) / secs, 1),
        "state_avg_bytes": round(sum(full) / max(1, len(full))),
        "state_bytes_per_s": round(sum(full) / secs),
        "state_deflate_bytes_per_s": round(deflate_sizes(full_msgs) / secs),
    }
    for window in (0.5, 1.0):
        bw = {}
        for t, p in log:
            for k in range(int((t - states[0][0]) / window), int((t + 2.2 - states[0][0]) / window) + 1):
                bw.setdefault(k, set()).add(p)
        msgs, sizes = delta_stream(states, window, bw)
        key = f"delta_{window}s"
        res[key + "_msgs_per_s"] = round(len(sizes) / secs, 2)
        res[key + "_avg_bytes"] = round(sum(sizes) / max(1, len(sizes)))
        res[key + "_bytes_per_s"] = round(sum(sizes) / secs)
        res[key + "_deflate_bytes_per_s"] = round(deflate_sizes(msgs) / secs)
    with open(os.path.join(WORK, f"summary-{tag}.json"), "w") as f:
        json.dump(res, f, indent=1)
    print(json.dumps(res, indent=1))


if __name__ == "__main__":
    main()
