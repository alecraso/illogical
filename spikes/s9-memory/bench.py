#!/usr/bin/env python3
"""S9: memory per pane in illogicald.

Starts a private illogicald on a loopback port, drives it over its HTTP API
and WebSocket protocol, and records RSS/PSS/USS of the daemon and of its pane
processes (shims, shells) at each step. Every scenario gets a fresh daemon
and state dir.

    uv run --with websockets python3 spikes/s9-memory/bench.py \
        --bin spikes/s9-memory/work/illogicald [--port 7741] [--scenarios idle,full,...]

Results go to work/results-<scenario>.json and a summary to stdout.
"""

import argparse
import asyncio
import json
import os
import random
import shutil
import signal
import socket
import subprocess
import sys
import time
import urllib.request

import websockets

HERE = os.path.dirname(os.path.abspath(__file__))
WORK = os.path.join(HERE, "work")
MiB = 1024 * 1024
TAG = ""  # set from --tag: names results, logs and state dirs


# ---------------------------------------------------------------- /proc


def smaps(pid):
    """Rss/Pss/USS/anon in KiB from smaps_rollup, plus threads."""
    out = {}
    try:
        with open(f"/proc/{pid}/smaps_rollup") as f:
            for line in f:
                parts = line.split()
                if len(parts) >= 3 and parts[-1] == "kB":
                    out[parts[0].rstrip(":")] = int(parts[1])
        with open(f"/proc/{pid}/status") as f:
            for line in f:
                if line.startswith(("Threads:", "VmHWM:", "VmSize:")):
                    k, v = line.split(":", 1)
                    out[k] = int(v.split()[0])
    except (FileNotFoundError, ProcessLookupError, PermissionError):
        return None
    return {
        "rss": out.get("Rss", 0),
        "pss": out.get("Pss", 0),
        "uss": out.get("Private_Clean", 0) + out.get("Private_Dirty", 0),
        "anon": out.get("Anonymous", 0),
        "threads": out.get("Threads", 0),
        "hwm": out.get("VmHWM", 0),
        "vsz": out.get("VmSize", 0),
    }


def descendants(root):
    kids = {}
    for d in os.listdir("/proc"):
        if not d.isdigit():
            continue
        try:
            with open(f"/proc/{d}/stat") as f:
                s = f.read()
            comm = s[s.index("(") + 1 : s.rindex(")")]
            ppid = int(s[s.rindex(")") + 2 :].split()[1])
        except (FileNotFoundError, ProcessLookupError, ValueError):
            continue
        kids.setdefault(ppid, []).append((int(d), comm))
    out, todo = [], [root]
    while todo:
        p = todo.pop()
        for c, comm in kids.get(p, []):
            out.append((c, comm))
            todo.append(c)
    return out


def measure(pid):
    d = smaps(pid)
    groups = {}
    for c, comm in descendants(pid):
        # The shim is the daemon's binary re-executed as `illogicald _shim`
        # (comm is the file name, cut to 15 bytes).
        m = smaps(c)
        if not m:
            continue
        if comm.startswith("illogicald"):
            comm = "illogicald"
        g = groups.setdefault(comm, {"n": 0, "rss": 0, "pss": 0, "uss": 0})
        g["n"] += 1
        for k in ("rss", "pss", "uss"):
            g[k] += m[k]
    return {"daemon": d, "children": groups, "t": time.time()}


def settle(pid, quiet=2.0, limit=30.0):
    """Wait until daemon RSS stops moving (<0.5% over `quiet` seconds)."""
    t0 = time.time()
    last = smaps(pid)["rss"]
    while time.time() - t0 < limit:
        time.sleep(quiet)
        now = smaps(pid)["rss"]
        if abs(now - last) <= max(64, last * 0.005):
            return
        last = now


# ---------------------------------------------------------------- daemon


class Daemon:
    def __init__(self, binary, port, name, env_extra=None):
        name = name + TAG
        self.port = port
        self.state = os.path.join(WORK, f"state-{name}")
        shutil.rmtree(self.state, ignore_errors=True)
        env = dict(os.environ)
        env.pop("NOTIFY_SOCKET", None)  # no systemd: no scopes, no FD store
        env.pop("LISTEN_FDS", None)
        env["PS1"] = "$ "
        env.update(env_extra or {})
        self.log = open(os.path.join(WORK, f"daemon-{name}.log"), "w")
        self.proc = subprocess.Popen(
            [
                binary,
                "--listen", f"127.0.0.1:{port}",
                "--shell", "bash --norc --noprofile",
                "--no-manager-env",
                "--state-dir", self.state,
                "--wisp-token-file", os.path.join(WORK, "no-wisp-token"),
            ],
            env=env, stdout=self.log, stderr=subprocess.STDOUT,
            start_new_session=True,
        )
        for _ in range(200):
            try:
                socket.create_connection(("127.0.0.1", port), timeout=0.2).close()
                break
            except OSError:
                time.sleep(0.05)
        else:
            raise SystemExit("daemon did not start")
        self.pid = self.proc.pid

    def http(self, method, path, body=None, timeout=30):
        data = json.dumps(body).encode() if body is not None else None
        req = urllib.request.Request(
            f"http://127.0.0.1:{self.port}{path}", data=data, method=method,
            headers={"Content-Type": "application/json"},
        )
        with urllib.request.urlopen(req, timeout=timeout) as r:
            raw = r.read()
            try:
                return json.loads(raw)
            except ValueError:
                return raw.decode(errors="replace")

    def panes(self):
        return [p["id"] for p in self.http("GET", "/api/panes")]

    def new_pane(self):
        return self.http("POST", "/api/run", {})["pane"]

    def send(self, pane, text):
        self.http("POST", f"/api/panes/{pane}/send", {"text": text, "enter": True})

    def close(self, pane):
        self.http("POST", f"/api/panes/{pane}/close", {})

    def wait_prompt(self, panes, limit=60):
        t0 = time.time()
        for p in panes:
            while time.time() - t0 < limit:
                txt = self.http("GET", f"/api/panes/{p}/capture?format=text")
                if "$" in str(txt):
                    break
                time.sleep(0.1)

    def stop(self):
        # The shells are in their own sessions; hang them up too.
        kids = [c for c, _ in descendants(self.pid)]
        self.proc.terminate()
        try:
            self.proc.wait(10)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.wait()
        for c in kids:
            try:
                os.kill(c, signal.SIGKILL)
            except ProcessLookupError:
                pass
        self.log.close()


async def ws_connect(port):
    ws = await websockets.connect(f"ws://127.0.0.1:{port}/ws", max_size=None)
    hello = json.loads(await ws.recv())
    return ws, hello


async def resize_all(port, cols, rows):
    """Claim every tab at cols x rows; the returned socket must stay open."""
    ws, hello = await ws_connect(port)
    for t in hello["state"]["tabs"]:
        await ws.send(json.dumps(
            {"type": "view", "tab": t["id"], "cols": cols, "rows": rows, "zoom": None, "claim": True}))
    return ws


async def drain(ws, seconds):
    end = time.time() + seconds
    n = 0
    while time.time() < end:
        try:
            m = await asyncio.wait_for(ws.recv(), timeout=max(0.01, end - time.time()))
            n += len(m)
        except asyncio.TimeoutError:
            break
    return n


# ---------------------------------------------------------------- fixtures


def fixtures():
    os.makedirs(WORK, exist_ok=True)
    rnd = random.Random(9)
    # A full 200x50 screen, every cell styled: truecolor fg+bg, attributes
    # varying, a mix of ASCII and a few wide/box characters.
    path = os.path.join(WORK, "screen.ans")
    if not os.path.exists(path):
        chars = "abcdefghijklmnopqrstuvwxyz0123456789#@%&*=+-─│┼"
        out = ["\x1b[?1049h\x1b[H"]
        for r in range(50):
            out.append(f"\x1b[{r + 1};1H")
            for c in range(200):
                attrs = rnd.choice(["", "1;", "3;", "4;", "1;3;", "7;"])
                out.append(
                    f"\x1b[0;{attrs}38;2;{rnd.randrange(256)};{rnd.randrange(256)};{rnd.randrange(256)};"
                    f"48;2;{rnd.randrange(256)};{rnd.randrange(256)};{rnd.randrange(256)}m{rnd.choice(chars)}"
                )
        out.append("\x1b[0m")
        with open(path, "w") as f:
            f.write("".join(out))
    # The same screen as a typical full-screen app draws it: 16 colours,
    # a few attributes (tens of distinct styles, not thousands).
    path = os.path.join(WORK, "screen16.ans")
    if not os.path.exists(path):
        out = ["\x1b[?1049h\x1b[H"]
        for r in range(50):
            out.append(f"\x1b[{r + 1};1H")
            for c in range(0, 200, 10):
                out.append(f"\x1b[0;{rnd.choice(['', '1;'])}{30 + rnd.randrange(8)};{40 + rnd.randrange(8)}m"
                           + "".join(rnd.choice("abcdefghijklmnop ") for _ in range(10)))
        out.append("\x1b[0m")
        with open(path, "w") as f:
            f.write("".join(out))
    # Scrollback: coloured log-like lines (~100 columns, 256-colour SGR).
    for n in (10_000, 200_000):
        path = os.path.join(WORK, f"lines{n}.ans")
        if os.path.exists(path):
            continue
        with open(path, "w") as f:
            for i in range(n):
                c1, c2 = rnd.randrange(256), rnd.randrange(256)
                f.write(
                    f"\x1b[38;5;{c1}m{i:07d}\x1b[0m \x1b[1;38;5;{c2}mINFO\x1b[0m "
                    f"request id={rnd.getrandbits(64):016x} path=/api/v1/items/{rnd.randrange(10**6)} "
                    f"took={rnd.random() * 100:.2f}ms status=200\n"
                )


# ---------------------------------------------------------------- report


def row(label, m, panes, base=None):
    d = m["daemon"]
    ch = m["children"]
    bash = ch.get("bash", {"n": 0, "rss": 0, "pss": 0, "uss": 0})
    shim = ch.get("illogicald", {"n": 0, "rss": 0, "pss": 0, "uss": 0})
    r = {
        "label": label, "panes": panes,
        "rss_kib": d["rss"], "pss_kib": d["pss"], "uss_kib": d["uss"], "anon_kib": d["anon"],
        "threads": d["threads"], "hwm_kib": d["hwm"], "vsz_kib": d["vsz"],
        "bash_n": bash["n"], "bash_pss_kib": bash["pss"], "bash_uss_kib": bash["uss"], "bash_rss_kib": bash["rss"],
        "shim_n": shim["n"], "shim_pss_kib": shim["pss"], "shim_uss_kib": shim["uss"], "shim_rss_kib": shim["rss"],
        "other_children": {k: v for k, v in ch.items() if k not in ("bash", "illogicald")},
    }
    if base is not None and panes != base["panes"]:
        dp = panes - base["panes"]
        r["per_pane_rss_kib"] = round((d["rss"] - base["rss_kib"]) / dp, 1)
        r["per_pane_uss_kib"] = round((d["uss"] - base["uss_kib"]) / dp, 1)
    print(
        f"{label:<34} panes={panes:<4} rss={d['rss'] / 1024:8.1f}M uss={d['uss'] / 1024:8.1f}M "
        f"thr={d['threads']:<5} bash={bash['n']}x pss {bash['pss'] / 1024:7.1f}M "
        f"shim={shim['n']}x pss {shim['pss'] / 1024:6.1f}M"
        + (f"  per-pane rss {r['per_pane_rss_kib']:.0f}K uss {r['per_pane_uss_kib']:.0f}K" if "per_pane_rss_kib" in r else ""),
        flush=True,
    )
    return r


def save(name, rows):
    with open(os.path.join(WORK, f"results-{name}{TAG}.json"), "w") as f:
        json.dump(rows, f, indent=1)


# ---------------------------------------------------------------- scenarios


def grow(d, target):
    """Open tabs (one pane each) until there are `target` panes."""
    have = d.panes()
    new = [d.new_pane() for _ in range(target - len(have))]
    d.wait_prompt(new)
    return d.panes()


def scenario_idle(a):
    """N idle empty panes; then close them all and open them again."""
    d = Daemon(a.bin, a.port, "idle", a.env)
    rows = []
    try:
        time.sleep(2)
        settle(d.pid)
        base = row("baseline (1 pane, the default)", measure(d.pid), len(d.panes()))
        rows.append(base)
        for n in a.counts:
            grow(d, n)
            settle(d.pid)
            rows.append(row(f"idle empty x{n}", measure(d.pid), len(d.panes()), base))
        # Close all but the first pane: does memory come back?
        ps = d.panes()
        for p in ps[1:]:
            d.close(p)
        time.sleep(5)
        settle(d.pid)
        rows.append(row("after closing all but 1", measure(d.pid), len(d.panes())))
        grow(d, a.counts[-1])
        settle(d.pid)
        rows.append(row(f"reopened x{a.counts[-1]}", measure(d.pid), len(d.panes()), base))
    finally:
        d.stop()
    save("idle", rows)
    return rows


def content_scenario(a, name, n, cols, rows_, command, wait, clients=None):
    clients = a.clients if clients is None else clients
    d = Daemon(a.bin, a.port, name, a.env)
    out = []
    try:
        time.sleep(2)
        grow(d, n)

        async def body():
            ws = await resize_all(a.port, cols, rows_)
            await drain(ws, 2)
            settle(d.pid)
            base = row(f"{name}: x{n} empty {cols}x{rows_}", measure(d.pid), n)
            out.append(base)
            for p in d.panes():
                d.send(p, command)
            await drain(ws, wait)
            settle(d.pid, quiet=3)
            # Checkpoints follow 5s of quiet output.
            await drain(ws, 8)
            settle(d.pid)
            m = measure(d.pid)
            r = row(f"{name}: x{n} after", m, n)
            r["per_pane_delta_rss_kib"] = round((m["daemon"]["rss"] - base["rss_kib"]) / n, 1)
            r["per_pane_delta_uss_kib"] = round((m["daemon"]["uss"] - base["uss_kib"]) / n, 1)
            print(f"   -> content cost per pane: rss {r['per_pane_delta_rss_kib']:.0f}K "
                  f"uss {r['per_pane_delta_uss_kib']:.0f}K", flush=True)
            out.append(r)
            # Clients: K sockets each attached to every pane (fresh views).
            prev = r
            socks = []
            for k in clients:
                while len(socks) < k:
                    s, hello = await ws_connect(a.port)
                    await s.send(json.dumps({"type": "attach", "panes": [
                        {"pane": p["id"], "offset": None} for p in hello["state"]["panes"]]}))
                    socks.append(s)
                    await drain(s, 0.5)
                for s in socks:
                    await drain(s, 0.2)
                settle(d.pid)
                m = measure(d.pid)
                r = row(f"{name}: +{k} clients attached to all", m, n)
                r["per_client_rss_kib"] = round((m["daemon"]["rss"] - out[1]["rss_kib"]) / k, 1)
                print(f"   -> per client (all {n} panes): rss {r['per_client_rss_kib']:.0f}K", flush=True)
                out.append(r)
            for s in socks:
                await s.close()
            await ws.close()
            await asyncio.sleep(2)
            settle(d.pid)
            out.append(row(f"{name}: clients gone", measure(d.pid), n))
            for p in d.panes()[1:]:
                d.close(p)
            await asyncio.sleep(6)
            settle(d.pid)
            out.append(row(f"{name}: closed all but 1", measure(d.pid), len(d.panes())))

        asyncio.run(body())
    finally:
        d.stop()
    save(name, out)
    return out


def scenario_full16(a):
    """A full screen as apps draw it (alt screen, 16 colours), left showing."""
    cmd = f"cat {WORK}/screen16.ans; exec sleep 100000"
    return content_scenario(a, "full16", a.full_n, 200, 50, cmd, 5)


def scenario_full(a):
    """Worst case: every cell its own truecolor fg/bg and attributes."""
    cmd = f"cat {WORK}/screen.ans; exec sleep 100000"
    return content_scenario(a, "full", a.full_n, 200, 50, cmd, 5, [1])


def scenario_nvim(a):
    """nvim --clean editing this script (syntax highlighting on)."""
    cmd = f"exec nvim --clean {os.path.abspath(__file__)}"
    return content_scenario(a, "nvim", a.nvim_n, 200, 50, cmd, 8, [1])


def scenario_sb10k(a):
    return content_scenario(a, "sb10k", a.sb_n, 200, 50, f"cat {WORK}/lines10000.ans", 15, [1, 5])


def scenario_sb200k(a):
    return content_scenario(a, "sb200k", a.sb_big_n, 200, 50, f"cat {WORK}/lines200000.ans", 60, [1])


def scenario_stalled(a):
    """What a client that stops reading costs. The same burst of output
    runs twice: first with no client attached (the control: scrollback,
    ring and checkpoints fill up to their caps), then again with one client
    attached that never reads its socket."""
    d = Daemon(a.bin, a.port, "stalled", a.env)
    out = []
    burst = "seq 1 3000000"
    try:
        time.sleep(2)
        grow(d, a.stall_panes)

        async def body():
            ws = await resize_all(a.port, 200, 50)
            await drain(ws, 1)
            settle(d.pid)
            out.append(row("stalled: base", measure(d.pid), a.stall_panes))
            for p in d.panes():
                d.send(p, burst)
            await drain(ws, 30)
            settle(d.pid)
            await drain(ws, 8)
            control = row("stalled: control, burst with no client", measure(d.pid), a.stall_panes)
            out.append(control)
            s, hello = await ws_connect(a.port)
            await s.send(json.dumps({"type": "attach", "panes": [
                {"pane": p["id"], "offset": None} for p in hello["state"]["panes"]]}))
            await drain(s, 3)
            # Stop reading `s`: its TCP window fills, then the daemon's
            # per-client queue. Keep reading the control socket.
            for p in d.panes():
                d.send(p, burst)
            peak = 0
            t0 = time.time()
            while time.time() - t0 < 40:
                await drain(ws, 1)
                peak = max(peak, smaps(d.pid)["rss"])
            m = measure(d.pid)
            r = row("stalled: same burst, client not reading", m, a.stall_panes)
            r["peak_rss_kib"] = peak
            r["stalled_client_rss_kib"] = m["daemon"]["rss"] - control["rss_kib"]
            print(f"   -> peak rss {peak / 1024:.1f}M; over control {r['stalled_client_rss_kib'] / 1024:.1f}M",
                  flush=True)
            out.append(r)
            await s.close()
            await ws.close()
            await asyncio.sleep(3)
            settle(d.pid)
            out.append(row("stalled: client gone", measure(d.pid), a.stall_panes))

        asyncio.run(body())
    finally:
        d.stop()
    save("stalled", out)
    return out


SCENARIOS = {
    "idle": scenario_idle,
    "full16": scenario_full16,
    "full": scenario_full,
    "nvim": scenario_nvim,
    "sb10k": scenario_sb10k,
    "sb200k": scenario_sb200k,
    "stalled": scenario_stalled,
}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", default=os.path.join(WORK, "illogicald"))
    ap.add_argument("--port", type=int, default=7741)
    ap.add_argument("--scenarios", default="idle,full16,full,nvim,sb10k,sb200k,stalled")
    ap.add_argument("--counts", default="10,50,200,500")
    ap.add_argument("--full-n", type=int, default=50)
    ap.add_argument("--sb-n", type=int, default=50)
    ap.add_argument("--nvim-n", type=int, default=20)
    ap.add_argument("--sb-big-n", type=int, default=10)
    ap.add_argument("--stall-panes", type=int, default=4)
    ap.add_argument("--clients", default="1,5,20")
    ap.add_argument("--env", action="append", default=[], help="KEY=VALUE for the daemon (e.g. MALLOC_ARENA_MAX=2)")
    ap.add_argument("--tag", default="")
    a = ap.parse_args()
    a.counts = [int(x) for x in a.counts.split(",")]
    a.clients = [int(x) for x in a.clients.split(",")]
    a.env = dict(e.split("=", 1) for e in a.env)
    global TAG
    TAG = f"-{a.tag}" if a.tag else ""
    fixtures()
    for s in a.scenarios.split(","):
        print(f"== {s} {a.tag} {a.env}", flush=True)
        SCENARIOS[s](a)


if __name__ == "__main__":
    main()
