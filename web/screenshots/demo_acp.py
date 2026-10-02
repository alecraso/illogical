#!/usr/bin/env python3
"""A scripted ACP agent for `just screenshots`: newline-delimited JSON-RPC
on stdio, like crates/daemon/tests/fake_acp.py, but with a transcript that
looks like real work. Nothing here calls a model.

Prompts:
  fix the flaky session test   reads, runs the tests (with output), edits,
                               then asks to commit (an approval card)
  and the other sleeps?        asks which approach (a question card)
"""

import json
import sys
import threading

out_lock = threading.Lock()
next_id = [0]
waiting = {}
cost = [0.0]
caps = {}


def send(m):
    m["jsonrpc"] = "2.0"
    with out_lock:
        sys.stdout.write(json.dumps(m) + "\n")
        sys.stdout.flush()


def request(method, params):
    rid = next_id[0]
    next_id[0] += 1
    ev = threading.Event()
    waiting[rid] = [ev, None]
    send({"id": rid, "method": method, "params": params})
    ev.wait()
    return waiting.pop(rid)[1]


TESTS = (
    "\x1b[1m\x1b[32m   Compiling\x1b[0m auth v0.4.2 (/home/demo/src/auth)\r\n"
    "\x1b[1m\x1b[32m    Finished\x1b[0m `test` profile [unoptimized + debuginfo] target(s) in 2.31s\r\n"
    "\x1b[1m\x1b[32m     Running\x1b[0m unittests src/lib.rs\r\n\r\n"
    "running 6 tests\r\n"
    "test session::tests::creates_a_session ... \x1b[32mok\x1b[0m\r\n"
    "test session::tests::refreshes_before_expiry ... \x1b[32mok\x1b[0m\r\n"
    "test session::tests::rejects_a_forged_token ... \x1b[32mok\x1b[0m\r\n"
    "test session::tests::expires_after_ttl ... \x1b[31mFAILED\x1b[0m\r\n"
    "test session::tests::revokes_on_logout ... \x1b[32mok\x1b[0m\r\n"
    "test session::tests::survives_a_restart ... \x1b[32mok\x1b[0m\r\n\r\n"
    "---- session::tests::expires_after_ttl stdout ----\r\n"
    "thread 'session::tests::expires_after_ttl' panicked at src/session.rs:212:9:\r\n"
    "assertion failed: store.get(&id).is_none()\r\n\r\n"
    "test result: \x1b[31mFAILED\x1b[0m. 5 passed; 1 failed; 0 ignored; finished in 0.14s\r\n"
)

APPROACH = {
    "question": "Three more tests sleep on the real clock. How should I handle them?",
    "header": "Approach",
    "multiSelect": False,
    "options": [
        {"label": "Inject the clock everywhere", "description": "Same fix as expires_after_ttl, in all three tests"},
        {"label": "Only the flaky ones", "description": "Leave the two that have never failed"},
        {"label": "Leave them", "description": "Fix them when they flake"},
    ],
}


def prompt(mid, p):
    sid = p["sessionId"]
    text = "".join(c.get("text", "") for c in p.get("prompt", []))
    up = lambda u: send({"method": "session/update", "params": {"sessionId": sid, "update": u}})
    n = [0]

    def msg(t):
        n[0] += 1
        up({"sessionUpdate": "agent_message_chunk", "messageId": f"m{mid}-{n[0]}", "content": {"type": "text", "text": t}})

    def tool(tid, title, kind, status="completed", **extra):
        up({"sessionUpdate": "tool_call", "toolCallId": tid, "title": title, "kind": kind, "status": status, **extra})

    def terminal(tid, cmd, output, code):
        tool(tid, "Terminal", "execute", "pending", _meta={"terminal_info": {"terminal_id": tid}})
        up({"sessionUpdate": "tool_call_update", "toolCallId": tid, "title": cmd, "rawInput": {"command": cmd}, "status": "in_progress"})
        up({"sessionUpdate": "tool_call_update", "toolCallId": tid, "_meta": {"terminal_output": {"terminal_id": tid, "data": output}}})
        up({"sessionUpdate": "tool_call_update", "toolCallId": tid, "status": "completed",
            "_meta": {"terminal_exit": {"terminal_id": tid, "exit_code": code}}})

    up({"sessionUpdate": "user_message_chunk", "content": {"type": "text", "text": text}})
    if text.startswith("fix"):
        up({"sessionUpdate": "agent_thought_chunk", "content": {"type": "text", "text": "Run the session tests first to see which one fails, and how."}})
        terminal(f"t{mid}a", "cargo test -p auth session", TESTS, 101)
        tool(f"t{mid}b", "Read src/session.rs", "read", locations=[{"path": "/home/demo/src/auth/src/session.rs", "line": 198}])
        msg("`expires_after_ttl` sleeps 100 ms and expects the session to be gone, so it fails whenever the expiry "
            "sweep runs late. I'll give `Store` a clock and advance it in the test instead of sleeping.")
        tool(f"t{mid}c", "Edit src/session.rs", "edit", locations=[{"path": "/home/demo/src/auth/src/session.rs"}])
        terminal(f"t{mid}d", "cargo test -p auth session",
                 "running 6 tests\r\n......\r\ntest result: \x1b[32mok\x1b[0m. 6 passed; 0 failed; 0 ignored; finished in 0.02s\r\n", 0)
        cmd = 'git commit -am "session: advance a test clock instead of sleeping"'
        tid = f"t{mid}e"
        tool(tid, "Terminal", "execute", "pending", _meta={"terminal_info": {"terminal_id": tid}})
        up({"sessionUpdate": "tool_call_update", "toolCallId": tid, "title": cmd, "rawInput": {"command": cmd}})
        answer = request("session/request_permission", {
            "sessionId": sid,
            "toolCall": {"toolCallId": tid, "name": "Bash", "title": cmd, "kind": "execute", "status": "pending",
                         "rawInput": {"command": cmd}},
            "options": [
                {"optionId": "allow-once", "name": "Yes", "kind": "allow_once"},
                {"optionId": "allow-always", "name": "Yes, always", "kind": "allow_always"},
                {"optionId": "reject", "name": "No", "kind": "reject_once"},
            ]})
        if (answer or {}).get("outcome", {}).get("optionId", "").startswith("allow"):
            up({"sessionUpdate": "tool_call_update", "toolCallId": tid, "status": "completed",
                "_meta": {"terminal_output": {"terminal_id": tid, "data": "[main 4f2a9c1] session: advance a test clock instead of sleeping\r\n 1 file changed, 14 insertions(+), 6 deletions(-)\r\n"}}})
            msg("Committed. All six session tests pass, in 0.02s instead of 0.14s.")
        else:
            up({"sessionUpdate": "tool_call_update", "toolCallId": tid, "status": "failed"})
            msg("OK, I left it uncommitted.")
    else:
        tid = f"toolu_{mid}"
        meta = {"claudeCode": {"toolName": "AskUserQuestion"}}
        up({"_meta": meta, "toolCallId": tid, "sessionUpdate": "tool_call", "name": "AskUserQuestion", "rawInput": {"questions": [APPROACH]},
            "status": "pending", "title": "Asking for your input", "kind": "other",
            "content": [{"type": "content", "content": {"type": "text", "text": APPROACH["question"]}}]})
        opts = [{"const": o["label"], "title": o["label"], "description": o["description"]} for o in APPROACH["options"]]
        schema = {"type": "object", "properties": {
            "question_0": {"type": "string", "title": APPROACH["header"], "oneOf": opts},
            "question_0_custom": {"type": "string", "title": "Other",
                                  "description": "Type your own answer, or add a note to the option you chose above (optional).",
                                  "_meta": {"_askUserQuestionCustomAnswer": {"questionId": "question_0", "isCustomAnswer": True}}}}}
        answer = request("elicitation/create", {"sessionId": sid, "mode": "form", "toolCallId": tid,
                                                "message": APPROACH["question"], "requestedSchema": schema})
        pick = ((answer or {}).get("content") or {}).get("question_0", "nothing")
        up({"_meta": meta, "toolCallId": tid, "sessionUpdate": "tool_call_update", "status": "completed"})
        msg(f"{pick}: on it.")
    cost[0] = round(cost[0] + 0.04, 2)
    up({"sessionUpdate": "usage_update", "used": 18000, "size": 200000, "cost": {"amount": cost[0], "currency": "USD"}})
    send({"id": mid, "result": {"stopReason": "end_turn"}})


def handle(m):
    method, mid, p = m.get("method"), m.get("id"), m.get("params") or {}
    if method is None:
        if mid in waiting:
            waiting[mid][1] = m.get("result")
            waiting[mid][0].set()
    elif method == "initialize":
        caps.update(p.get("clientCapabilities") or {})
        send({"id": mid, "result": {"protocolVersion": 1, "agentCapabilities": {"loadSession": False},
                                    "agentInfo": {"name": "claude-code", "version": "demo"}, "authMethods": []}})
    elif method == "session/new":
        send({"id": mid, "result": {"sessionId": "demo"}})
    elif method == "session/prompt":
        threading.Thread(target=prompt, args=(mid, p), daemon=True).start()
    elif method == "session/set_config_option":
        send({"id": mid, "result": {"configOptions": []}})
    elif mid is not None:
        send({"id": mid, "error": {"code": -32601, "message": f"no {method}"}})


for line in sys.stdin:
    if line.strip():
        handle(json.loads(line))
