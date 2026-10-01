#!/usr/bin/env python3
"""A tiny ACP agent server for tests: newline-delimited JSON-RPC on stdio,
scripted by the prompt text. Like claude-agent-acp it exits when stdin
closes. Sessions are kept in $FAKE_ACP_DIR so session/load and
session/resume work from a new process.

Prompts:
  hello          a reply, a usage_update (cost grows by 0.01 per turn)
  run CMD        a tool call that asks permission, then "prints" its output
  slow           streams for ~10s unless cancelled
  remember WORD  remembers WORD; "recall" says it (across processes)
  crash          exits with code 3 mid-turn
"""

import json
import os
import sys
import threading
import time

DIR = os.environ.get("FAKE_ACP_DIR", "/tmp/fake-acp")
os.makedirs(DIR, exist_ok=True)

out_lock = threading.Lock()
next_id = [0]
waiting = {}  # our request id -> [event, answer]
cancelled = set()  # session ids with a cancel pending


def log(*a):
    print("fake-acp:", *a, file=sys.stderr, flush=True)


def send(m):
    m["jsonrpc"] = "2.0"
    with out_lock:
        sys.stdout.write(json.dumps(m) + "\n")
        sys.stdout.flush()


def path(sid):
    return os.path.join(DIR, sid + ".json")


def load(sid):
    try:
        with open(path(sid)) as f:
            return json.load(f)
    except OSError:
        return None


def save(sid, s):
    with open(path(sid), "w") as f:
        json.dump(s, f)


def update(sid, s, u):
    s["updates"].append(u)
    save(sid, s)
    send({"method": "session/update", "params": {"sessionId": sid, "update": u}})


def ask(sid, tool_id, cmd):
    """Ask permission; wait for the answer (forever, like the real one)."""
    rid = next_id[0]
    next_id[0] += 1
    ev = threading.Event()
    waiting[rid] = [ev, None]
    send({"id": rid, "method": "session/request_permission", "params": {
        "sessionId": sid,
        "toolCall": {"toolCallId": tool_id, "name": "Bash", "title": cmd, "kind": "execute",
                     "status": "pending", "rawInput": {"command": cmd}},
        "options": [
            {"optionId": "allow-once", "name": "Yes", "kind": "allow_once"},
            {"optionId": "allow-with-updates", "name": "Yes, always", "kind": "allow_always"},
            {"optionId": "reject", "name": "No", "kind": "reject_once"},
        ]}})
    ev.wait()
    return waiting.pop(rid)[1]


def prompt(mid, p):
    sid = p["sessionId"]
    s = load(sid)
    text = "".join(c.get("text", "") for c in p.get("prompt", []))
    update(sid, s, {"sessionUpdate": "user_message_chunk", "content": {"type": "text", "text": text}})
    n = len(s["updates"])
    msg = lambda t: update(sid, s, {"sessionUpdate": "agent_message_chunk", "messageId": f"m{n}",
                                   "content": {"type": "text", "text": t}})
    stop = "end_turn"
    if text.startswith("run "):
        cmd = text[4:]
        tid = f"tool{n}"
        update(sid, s, {"sessionUpdate": "tool_call", "toolCallId": tid, "title": "Terminal", "kind": "execute",
                        "status": "pending", "_meta": {"terminal_info": {"terminal_id": tid}}})
        update(sid, s, {"sessionUpdate": "tool_call_update", "toolCallId": tid, "title": cmd,
                        "rawInput": {"command": cmd}})
        answer = ask(sid, tid, cmd)
        outcome = (answer or {}).get("outcome", {})
        if outcome.get("outcome") == "selected" and outcome.get("optionId", "").startswith("allow"):
            update(sid, s, {"sessionUpdate": "tool_call_update", "toolCallId": tid, "status": "in_progress"})
            update(sid, s, {"sessionUpdate": "tool_call_update", "toolCallId": tid,
                            "_meta": {"terminal_output": {"terminal_id": tid,
                                                          "data": f"\x1b[32mran: {cmd}\x1b[0m\r\n"}}})
            update(sid, s, {"sessionUpdate": "tool_call_update", "toolCallId": tid, "status": "completed",
                            "_meta": {"terminal_exit": {"terminal_id": tid, "exit_code": 0}}})
            msg("Ran it.")
        elif outcome.get("outcome") == "cancelled":
            update(sid, s, {"sessionUpdate": "tool_call_update", "toolCallId": tid, "status": "failed"})
            stop = "cancelled"
        else:
            update(sid, s, {"sessionUpdate": "tool_call_update", "toolCallId": tid, "status": "failed",
                            "rawOutput": "User refused permission to run tool"})
            msg("Not allowed.")
    elif text == "slow":
        for i in range(50):
            if sid in cancelled:
                cancelled.discard(sid)
                stop = "cancelled"
                break
            msg(f"tick {i} ")
            time.sleep(0.2)
    elif text.startswith("remember "):
        s["memory"] = text[9:]
        msg("OK")
    elif text == "recall":
        msg(f"You said {s.get('memory', 'nothing')}.")
    elif text == "crash":
        msg("bye")
        os._exit(3)
    else:
        msg("Hello! I am fake.")
    if stop != "cancelled":
        s["cost"] = round(s.get("cost", 0) + 0.01, 4)
        update(sid, s, {"sessionUpdate": "usage_update", "used": 10, "size": 1000,
                        "cost": {"amount": s["cost"], "currency": "USD"}})
    save(sid, s)
    send({"id": mid, "result": {"stopReason": stop, "usage": {"inputTokens": 1, "outputTokens": 2, "totalTokens": 3}}})


def handle(m):
    method, mid, p = m.get("method"), m.get("id"), m.get("params") or {}
    if method is None:
        if mid in waiting:
            waiting[mid][1] = m.get("result")
            waiting[mid][0].set()
        return
    if method == "initialize":
        send({"id": mid, "result": {"protocolVersion": 1, "agentCapabilities": {
            "loadSession": True, "sessionCapabilities": {"resume": {}}},
            "agentInfo": {"name": "fake-acp", "version": "1"}, "authMethods": []}})
    elif method == "session/new":
        sid = f"fake-{os.getpid()}-{int(time.time() * 1000)}"
        save(sid, {"updates": [], "cwd": p.get("cwd")})
        send({"id": mid, "result": {"sessionId": sid}})
    elif method in ("session/load", "session/resume"):
        s = load(p.get("sessionId", ""))
        if s is None:
            send({"id": mid, "error": {"code": -32002, "message": "no such session"}})
            return
        if method == "session/load":
            for u in s["updates"]:
                send({"method": "session/update", "params": {"sessionId": p["sessionId"], "update": u}})
        send({"id": mid, "result": {}})
    elif method == "session/set_config_option":
        send({"id": mid, "result": {"configOptions": []}})
    elif method == "session/prompt":
        threading.Thread(target=prompt, args=(mid, p), daemon=True).start()
    elif method == "session/cancel":
        cancelled.add(p.get("sessionId"))
    elif mid is not None:
        send({"id": mid, "error": {"code": -32601, "message": f"no {method}"}})


log("started", os.getpid())
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    try:
        handle(json.loads(line))
    except Exception as e:  # noqa: BLE001
        log("error", e)
log("stdin closed; exiting")
