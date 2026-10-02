#!/usr/bin/env python3
"""S16: one browser page holding summary connections to 5 or 20 daemons.

Starts K private daemons (ports 7761-7769 and 7771-7781; 7770 is taken on
geek), each with P panes in tabs of 4, serves fleet.html from 127.0.0.1:7760
(an origin every daemon allows with --allow-origin), and runs fleet.mjs in
headless Chrome: page memory, first connect, and three sleep/wake cycles.

    uv run --with websockets python3 spikes/s16-swarm/fleet.py \
        --bin target/release/illogicald --daemons 20 --panes 25
"""

import argparse
import functools
import http.server
import json
import os
import subprocess
import sys
import threading
import time

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "s9-memory"))
import bench  # noqa: E402

import summary  # noqa: E402  (cpu_s, open_panes)

HERE = os.path.dirname(os.path.abspath(__file__))
WORK = os.path.join(HERE, "work")
bench.WORK = WORK
PAGE_PORT = 7760
def free_ports():
    """7761-7787, minus whatever is listening (other sessions' e2e daemons
    use some of these on geek)."""
    import socket

    out = []
    for p in range(7761, 7788):
        s = socket.socket()
        # As the daemon binds (tokio sets it): TIME_WAIT from a last run is fine.
        s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        try:
            s.bind(("127.0.0.1", p))
            out.append(p)
        except OSError:
            pass
        finally:
            s.close()
    return out


PORTS = free_ports()
ORIGIN = f"http://127.0.0.1:{PAGE_PORT}"


def with_origin(popen):
    """bench.Daemon's command line, plus --allow-origin for the page."""

    def wrapped(args, **kw):
        if args and args[0].endswith("illogicald"):
            args = list(args) + ["--allow-origin", ORIGIN]
        return popen(args, **kw)

    return wrapped


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", required=True)
    ap.add_argument("--daemons", type=int, default=5)
    ap.add_argument("--panes", type=int, default=25)
    a = ap.parse_args()
    bench.subprocess.Popen = with_origin(subprocess.Popen)

    handler = functools.partial(http.server.SimpleHTTPRequestHandler, directory=HERE)
    http.server.SimpleHTTPRequestHandler.log_message = lambda *_: None
    srv = http.server.ThreadingHTTPServer(("127.0.0.1", PAGE_PORT), handler)
    threading.Thread(target=srv.serve_forever, daemon=True).start()

    ds = []
    try:
        ports = iter(free_ports())
        for i in range(a.daemons):
            # Other sessions' e2e daemons come and go in this range: if the
            # port was taken meanwhile, ours exits; take the next.
            while True:
                d = bench.Daemon(os.path.abspath(a.bin), next(ports), f"s16-fleet{i}")
                time.sleep(0.3)
                if d.proc.poll() is None:
                    break
                d.log.close()
            ds.append(d)
            summary.open_panes(d, a.panes)
        time.sleep(6)
        rss = sum(bench.smaps(d.pid)["rss"] for d in ds)
        c0 = sum(summary.cpu_s(d.pid) for d in ds)
        t0 = time.time()
        ports = ",".join(str(d.port) for d in ds)
        out = subprocess.run(["node", os.path.join(HERE, "fleet.mjs"), ORIGIN, ports],
                             capture_output=True, text=True, timeout=300)
        if out.returncode:
            print(out.stderr, file=sys.stderr)
            raise SystemExit("fleet.mjs failed")
        res = json.loads(out.stdout.strip().splitlines()[-1])
        res["daemons_cpu_s_during_run"] = round(sum(summary.cpu_s(d.pid) for d in ds) - c0, 2)
        res["run_s"] = round(time.time() - t0, 1)
        res["panes_per_daemon"] = a.panes
        res["daemons_rss_mib"] = round(rss / 1024)
    finally:
        for d in ds:
            d.stop()
        srv.shutdown()
    with open(os.path.join(WORK, f"fleet-{a.daemons}x{a.panes}.json"), "w") as f:
        json.dump(res, f, indent=1)
    print(json.dumps(res, indent=1))


if __name__ == "__main__":
    main()
