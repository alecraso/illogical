# What a VM agent runs as its `arugula` MCP server (#59): stdio MCP,
# carried to the relay's Unix socket (`guest_relay.py`) and from there to
# the daemon. `python3 -c THIS arugula-mcp SOCKET`.
#
# The relay comes and goes (it starts beside the agent, and a restarted
# daemon starts a new one), so this waits for the socket, and when the
# connection drops it connects again and replays the client's
# `initialize`. Requests that were in flight get an error saying to call
# again.
import json
import os
import socket
import sys
import threading
import time

path = sys.argv[-1]
cond = threading.Condition()
state = {"conn": None, "init": None, "initialized": None, "answered": False, "swallow": None}
pending = set()
out_lock = threading.Lock()


def emit(line):
    with out_lock:
        sys.stdout.buffer.write(line + b"\n")
        sys.stdout.buffer.flush()


def connect(patience):
    deadline = time.time() + patience
    while True:
        c = socket.socket(socket.AF_UNIX)
        try:
            c.connect(path)
            return c
        except OSError:
            c.close()
            if time.time() > deadline:
                print("arugula: no MCP relay at", path, file=sys.stderr, flush=True)
                os._exit(1)
            time.sleep(0.2)


def reader(c):
    for raw in c.makefile("rb"):
        line = raw.strip()
        if not line:
            continue
        try:
            m = json.loads(line)
        except ValueError:
            continue
        mid = m.get("id") if isinstance(m, dict) else None
        if mid is not None and "method" not in m:
            with cond:
                if state["swallow"] is not None and mid == state["swallow"]:
                    # The answer to a replayed initialize.
                    state["swallow"] = None
                    continue
                pending.discard(json.dumps(mid))
                if state["init"] is not None and mid == state["init"].get("id"):
                    state["answered"] = True
        emit(line)
    # Dropped: what was in flight won't be answered.
    with cond:
        if state["conn"] is c:
            state["conn"] = None
        init_id = json.dumps(state["init"].get("id")) if state["init"] and not state["answered"] else None
        lost = [p for p in pending if p != init_id]
        for p in lost:
            pending.discard(p)
    for p in lost:
        emit(json.dumps({"jsonrpc": "2.0", "id": json.loads(p), "error": {
            "code": -32000,
            "message": "the connection to Arugula dropped (did its daemon restart?); call again"}}).encode())
    attach(connect(300))


def attach(c):
    """Replay the session's start on `c`, then make it the connection."""
    with cond:
        try:
            if state["init"] is not None:
                if state["answered"]:
                    state["swallow"] = state["init"].get("id")
                c.sendall(json.dumps(state["init"]).encode() + b"\n")
            if state["initialized"] is not None:
                c.sendall(state["initialized"] + b"\n")
        except OSError:
            pass
        state["conn"] = c
        cond.notify_all()
    threading.Thread(target=reader, args=(c,), daemon=True).start()


attach(connect(300))
for raw in sys.stdin.buffer:
    line = raw.strip()
    if not line:
        continue
    try:
        m = json.loads(line)
    except ValueError:
        m = None
    with cond:
        if isinstance(m, dict):
            if m.get("method") == "initialize":
                state["init"], state["answered"] = m, False
            elif m.get("method") == "notifications/initialized":
                state["initialized"] = line
            if "method" in m and m.get("id") is not None:
                pending.add(json.dumps(m["id"]))
    while True:
        with cond:
            while state["conn"] is None:
                cond.wait()
            c = state["conn"]
        try:
            c.sendall(line + b"\n")
            break
        except OSError:
            # Not sent: wait for the next connection and send it there.
            with cond:
                while state["conn"] is c:
                    cond.wait(0.5)
