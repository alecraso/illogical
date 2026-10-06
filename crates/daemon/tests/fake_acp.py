#!/usr/bin/env python3
"""A tiny ACP agent server for tests: newline-delimited JSON-RPC on stdio,
scripted by the prompt text. Like claude-agent-acp it exits when stdin
closes. Sessions are kept in $FAKE_ACP_DIR so session/load and
session/resume work from a new process.

Prompts:
  hello          a reply, a usage_update (cost grows by 0.01 per turn)
  run CMD        a tool call that asks permission, then "prints" its output
  read PATH      a Read tool call with no command that fails (not a shell command)
  slow           streams for ~10s unless cancelled
  remember WORD  remembers WORD; "recall" says it (across processes)
  crash          exits with code 3 mid-turn
  ask [one|two|preview]
                 AskUserQuestion as claude-agent-acp 0.81.2 sends it (S13):
                 a tool call, then elicitation/create with question_<n>
                 fields; the reply says what was answered. Three questions
                 by default; one with two options; two (a multi-select and
                 a single); one with previews. Stop withdraws it with
                 $/cancel_request.
  form           an MCP server's form (no toolCallId, enum + enumNames)
  signin         an MCP server's sign-in link; elicitation/complete follows
                 the accept
  codex ask      Codex's plan-mode question form
  meta           says the _meta its session was last opened or forked with
  servers        says the MCP servers its session got, ${VAR}s expanded from
                 its environment (as an agent that leaks what it was given
                 would)
  model          says the model set_config_option chose
  mode           says the permission mode session/set_mode chose (it knows
                 claude-agent-acp's: default, acceptEdits, plan, auto)
  look           says what came with the prompt (M71): its images' types,
                 and any other text blocks (an image's path, to an agent
                 that takes no images); the prompt's blocks are in
                 $FAKE_ACP_DIR/prompt-<session>.json. Run with --images, it
                 says it takes images (promptCapabilities.image).
  mcp TOOL JSON  calls TOOL on the session's `illogical` MCP server (an http
                 one, as illogical passes local agents, M16; or a stdio one,
                 as it passes agents in a VM, #59) with JSON as its
                 arguments, and says "MCP " and the result as JSON

Claude Code conversations (M33): a session/resume or session/load of a
session it doesn't have finds $CLAUDE_CONFIG_DIR/projects/*/<id>.jsonl, as
Claude Code would, and remembers the last "remember WORD" prompt in it.
session/fork copies a session to a new id (it isn't opened, as with
claude-agent-acp). With $FAKE_ACP_CLAUDE_SESSIONS set, the session it has
open is in $CLAUDE_CONFIG_DIR/sessions/<pid>.json, as Claude Code lists it.

Only clients that declare elicitation {form: {}, url: {}} get questions; the
others get "I don't have access to an AskUserQuestion tool" (S13).
"""

import json
import os
import re
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request

DIR = os.environ.get("FAKE_ACP_DIR", "/tmp/fake-acp")
os.makedirs(DIR, exist_ok=True)

out_lock = threading.Lock()
next_id = [0]
waiting = {}  # our request id -> [event, answer]
cancelled = set()  # session ids with a cancel pending
asking = {}  # session id -> the elicitation request id it waits on
caps = {}  # the client's capabilities
WITHDRAWN = {"withdrawn": True}
# As claude-agent-acp names its models.
CONFIG_OPTIONS = [{"id": "model", "name": "Model", "type": "select", "currentValue": "default", "options": [
    {"value": "default", "name": "Default (recommended)"},
    {"value": "opus", "name": "Opus 5.5"},
    {"value": "haiku", "name": "Haiku 4.5"},
]}]


def log(*a):
    print("fake-acp:", *a, file=sys.stderr, flush=True)


def send(m):
    m["jsonrpc"] = "2.0"
    with out_lock:
        sys.stdout.write(json.dumps(m) + "\n")
        sys.stdout.flush()


def holding(sid):
    """Say we hold sid in $CLAUDE_CONFIG_DIR/sessions, as Claude Code does."""
    if not os.environ.get("FAKE_ACP_CLAUDE_SESSIONS"):
        return
    d = os.path.join(os.environ.get("CLAUDE_CONFIG_DIR", os.path.expanduser("~/.claude")), "sessions")
    me = os.getpid()
    start = open(f"/proc/{me}/stat").read().rsplit(")", 1)[1].split()[19]
    with open(os.path.join(d, f"{me}.json"), "w") as f:
        json.dump({"pid": me, "sessionId": sid, "procStart": start, "kind": "sdk",
                   "entrypoint": "sdk-ts", "status": "idle"}, f)


def path(sid):
    return os.path.join(DIR, sid + ".json")


def load(sid):
    try:
        with open(path(sid)) as f:
            return json.load(f)
    except OSError:
        return from_transcript(sid)


def from_transcript(sid):
    """A Claude Code transcript of this session, if there is one."""
    root = os.path.join(os.environ.get("CLAUDE_CONFIG_DIR", os.path.expanduser("~/.claude")), "projects")
    if not sid or not os.path.isdir(root):
        return None
    for d in os.listdir(root):
        p = os.path.join(root, d, f"{sid}.jsonl")
        if not os.path.isfile(p):
            continue
        s = {"updates": [], "imported": p}
        for line in open(p):
            try:
                o = json.loads(line)
            except ValueError:
                continue
            c = (o.get("message") or {}).get("content")
            if o.get("type") == "user" and isinstance(c, str) and c.startswith("remember "):
                s["memory"] = c[9:]
        return s
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


COLOUR = {"question": "Which colour do you prefer?", "header": "Colour", "multiSelect": False, "options": [
    {"label": "Red", "description": "A warm, bold colour"}, {"label": "Blue", "description": "A cool, calming colour"}]}
FRUIT = {"question": "Which fruits do you like?", "header": "Fruit", "multiSelect": True, "options": [
    {"label": "Apple", "description": "Crisp and sweet"}, {"label": "Pear", "description": "Soft and juicy"},
    {"label": "Plum", "description": "Tart and sweet"}]}
PET = {"question": "Which pet do you prefer?", "header": "Pet", "multiSelect": False, "options": [
    {"label": "Cat", "description": "Independent"}, {"label": "Dog", "description": "Loyal"}]}
LAYOUT = {"question": "Which layout?", "header": "Layout", "multiSelect": False, "options": [
    {"label": "Sidebar", "description": "Navigation on the left side",
     "preview": "+------+--------------+\n| Nav  | Content Area |\n|      | goes here    |\n+------+--------------+"},
    {"label": "Top bar", "description": "Navigation across the top",
     "preview": "+---------------------+\n| Nav                 |\n+---------------------+\n| Content Area        |"}]}
QUESTIONS = {"ask": [COLOUR, FRUIT, PET], "ask one": [COLOUR], "ask two": [FRUIT, PET], "ask preview": [LAYOUT]}


def schema_of(questions):
    """The form claude-agent-acp makes of AskUserQuestion's input."""
    props = {}
    for n, q in enumerate(questions):
        opts = []
        for o in q["options"]:
            opt = {"const": o["label"], "title": o["label"], "description": o.get("description", "")}
            if "preview" in o:
                opt["_meta"] = {"_claude/askUserQuestionOption": {"preview": o["preview"]}}
            opts.append(opt)
        field = {"type": "array", "title": q["header"], "items": {"anyOf": opts}} if q["multiSelect"] else \
            {"type": "string", "title": q["header"], "oneOf": opts}
        if len(questions) > 1:
            field["description"] = q["question"]
        props[f"question_{n}"] = field
        props[f"question_{n}_custom"] = {
            "type": "string", "title": "Other",
            "description": "Type your own answer to add to your selection above (optional)." if q["multiSelect"]
            else "Type your own answer, or add a note to the option you chose above (optional).",
            "_meta": {"_askUserQuestionCustomAnswer": {"questionId": f"question_{n}", "isCustomAnswer": True}}}
    return {"type": "object", "properties": props}


def answers_of(questions, content):
    answers, notes = {}, {}
    for n, q in enumerate(questions):
        pick = content.get(f"question_{n}")
        picks = [pick] if isinstance(pick, str) and pick else [x for x in (pick or []) if x] if isinstance(pick, list) else []
        custom = (content.get(f"question_{n}_custom") or "").strip()
        if q["multiSelect"]:
            items = picks + ([custom] if custom else [])
            if items:
                answers[q["question"]] = ", ".join(items)
        elif picks:
            answers[q["question"]] = picks[0]
            if custom:
                notes[q["question"]] = {"notes": custom}
        elif custom:
            answers[q["question"]] = custom
    return answers, notes


def elicit(sid, params):
    """Send elicitation/create; wait for the answer (or a withdrawal)."""
    rid = next_id[0]
    next_id[0] += 1
    ev = threading.Event()
    waiting[rid] = [ev, None]
    asking[sid] = rid
    send({"id": rid, "method": "elicitation/create", "params": {"sessionId": sid, **params}})
    ev.wait()
    asking.pop(sid, None)
    return waiting.pop(rid)[1]


def ask_user(sid, s, n, questions, msg):
    """AskUserQuestion. Returns the stop reason."""
    tid = f"toolu_{n}"
    meta = {"claudeCode": {"toolName": "AskUserQuestion"}}
    update(sid, s, {"_meta": meta, "toolCallId": tid, "sessionUpdate": "tool_call", "name": "AskUserQuestion",
                    "rawInput": {}, "status": "pending", "title": "Asking for your input", "kind": "other",
                    "content": []})
    update(sid, s, {"_meta": meta, "toolCallId": tid, "sessionUpdate": "tool_call_update",
                    "rawInput": {"questions": questions}, "title": "Asking for your input", "kind": "other",
                    "content": [{"type": "content", "content": {"type": "text", "text": q["question"]}}
                                for q in questions]})
    message = questions[0]["question"] if len(questions) == 1 else "Please answer the following questions."
    answer = elicit(sid, {"mode": "form", "toolCallId": tid, "message": message,
                          "requestedSchema": schema_of(questions)})
    if answer is WITHDRAWN:
        return "cancelled"
    if (answer or {}).get("action") == "accept":
        answers, notes = answers_of(questions, answer.get("content") or {})
        response = {"questions": questions, "answers": answers}
        if notes:
            response["annotations"] = notes
        update(sid, s, {"_meta": {"claudeCode": {"toolResponse": response, "toolName": "AskUserQuestion"}},
                        "toolCallId": tid, "sessionUpdate": "tool_call_update"})
        said = ", ".join(f'"{q}"="{a}"' + (f" notes: {notes[q]['notes']}" if q in notes else "")
                         for q, a in answers.items())
        text = f"The user answered: {said}."
        update(sid, s, {"_meta": meta, "toolCallId": tid, "sessionUpdate": "tool_call_update", "status": "completed",
                        "rawOutput": text, "content": [{"type": "content", "content": {"type": "text", "text": text}}]})
        msg("You answered: " + "; ".join(f"{q} {a}" + (f" ({notes[q]['notes']})" if q in notes else "")
                                         for q, a in answers.items()))
    elif (answer or {}).get("action") == "decline":
        text = "The user did not answer the questions."
        update(sid, s, {"_meta": meta, "toolCallId": tid, "sessionUpdate": "tool_call_update", "status": "completed",
                        "rawOutput": text, "content": [{"type": "content", "content": {"type": "text", "text": text}}]})
        msg("You didn't answer the question.")
    else:
        update(sid, s, {"_meta": meta, "toolCallId": tid, "sessionUpdate": "tool_call_update", "status": "failed",
                        "rawOutput": "Tool permission request failed: Error: Tool use aborted"})
        msg("I couldn't ask you.")
    return "end_turn"


def expand(v):
    """${VAR}s from our environment, as Claude Code expands them in its MCP
    config (M44, #128: illogical passes Claude Code references)."""
    if isinstance(v, str):
        return re.sub(r"\$\{([A-Za-z_][A-Za-z0-9_]*)\}", lambda m: os.environ.get(m.group(1), m.group(0)), v)
    if isinstance(v, list):
        return [expand(x) for x in v]
    if isinstance(v, dict):
        return {k: expand(x) for k, x in v.items()}
    return v


def mcp_call(servers, tool, args):
    """A tool call over Streamable HTTP, as an MCP client: initialize (a
    2025-06-18 session), then tools/call; answers come as SSE."""
    srv = next((x for x in servers or [] if x.get("name") == "illogical"), None)
    if srv is None:
        return {"error": "no illogical server in this session"}
    srv = expand(srv)
    if srv.get("type") != "http":
        return mcp_call_stdio(srv, tool, args)
    base = {h["name"]: h["value"] for h in srv.get("headers", [])}
    base.update({"Content-Type": "application/json", "Accept": "application/json, text/event-stream"})

    def post(body, session=None):
        h = dict(base)
        if session:
            h.update({"Mcp-Session-Id": session, "MCP-Protocol-Version": "2025-06-18"})
        req = urllib.request.Request(srv["url"], data=json.dumps(body).encode(), headers=h, method="POST")
        try:
            r = urllib.request.urlopen(req, timeout=120)
        except urllib.error.HTTPError as e:
            return e.code, None, e.read().decode()
        text = r.read().decode()
        msgs = []
        if r.headers.get("Content-Type", "").startswith("text/event-stream"):
            for block in text.split("\n\n"):
                data = "".join(x[5:].lstrip() for x in block.split("\n") if x.startswith("data:"))
                if data:
                    msgs.append(json.loads(data))
        elif text:
            msgs.append(json.loads(text))
        return r.status, r.headers.get("Mcp-Session-Id"), msgs

    status, sid, msgs = post({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "fake-agent", "version": "1"}}})
    if status != 200:
        return {"error": f"HTTP {status}: {msgs}"}
    post({"jsonrpc": "2.0", "method": "notifications/initialized"}, sid)
    status, _, msgs = post({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
                            "params": {"name": tool, "arguments": args}}, sid)
    if status != 200:
        return {"error": f"HTTP {status}: {msgs}"}
    for m in msgs:
        if m.get("id") == 2:
            return m.get("result") or m.get("error")
    return {"error": "no answer"}


def mcp_call_stdio(srv, tool, args):
    """The same over stdio: the server's command, newline-delimited."""
    env = dict(os.environ)
    env.update({e["name"]: e["value"] for e in srv.get("env", [])})
    p = subprocess.Popen([srv["command"], *srv.get("args", [])], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                         env=env)

    def send(m):
        p.stdin.write((json.dumps(m) + "\n").encode())
        p.stdin.flush()

    def answer(i):
        for line in p.stdout:
            m = json.loads(line)
            if m.get("id") == i and "method" not in m:
                return m
        return {"error": "the server closed"}

    try:
        send({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "fake-agent", "version": "1"}}})
        m = answer(1)
        if "result" not in m:
            return {"error": f"initialize: {m}"}
        send({"jsonrpc": "2.0", "method": "notifications/initialized"})
        send({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": tool, "arguments": args}})
        m = answer(2)
        return m.get("result") or m.get("error")
    finally:
        p.kill()
        p.wait()


def prompt(mid, p):
    sid = p["sessionId"]
    s = load(sid)
    blocks = p.get("prompt", [])
    with open(os.path.join(DIR, f"prompt-{sid}.json"), "w") as f:
        json.dump(blocks, f)
    text = "".join(c.get("text", "") for c in blocks)
    update(sid, s, {"sessionUpdate": "user_message_chunk", "content": {"type": "text", "text": text}})
    n = len(s["updates"])
    msg = lambda t: update(sid, s, {"sessionUpdate": "agent_message_chunk", "messageId": f"m{n}",
                                   "content": {"type": "text", "text": t}})
    stop = "end_turn"
    if text.startswith("look"):
        images = [c.get("mimeType") for c in blocks if c.get("type") == "image"]
        others = [c.get("text", "") for c in blocks[1:] if c.get("type") == "text"]
        msg(f"Saw {len(images)} image(s) {images}; text {others}")
    elif text.startswith("run "):
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
    elif text.startswith("read "):
        tid = f"tool{n}"
        update(sid, s, {"sessionUpdate": "tool_call", "toolCallId": tid, "title": f"Read {text[5:]}",
                        "kind": "read", "status": "pending"})
        update(sid, s, {"sessionUpdate": "tool_call_update", "toolCallId": tid, "status": "failed"})
        msg("Couldn't read it.")
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
    elif text in QUESTIONS:
        if (caps.get("elicitation") or {}).get("form") is None:
            msg("I don't have access to an AskUserQuestion tool.")
        else:
            stop = ask_user(sid, s, n, QUESTIONS[text], msg)
    elif text == "form":
        answer = elicit(sid, {"mode": "form", "message": "Order details", "requestedSchema": {
            "type": "object", "properties": {
                "size": {"type": "string", "title": "Size", "enum": ["S", "M", "L"],
                         "enumNames": ["Small", "Medium", "Large"]},
                "qty": {"type": "integer", "title": "Quantity", "minimum": 1, "maximum": 9},
                "gift": {"type": "boolean", "title": "Gift wrap"}},
            "required": ["size"]}})
        if answer is WITHDRAWN:
            stop = "cancelled"
        else:
            msg(f"Order: {json.dumps(answer, sort_keys=True)}")
    elif text == "signin":
        eid = f"signin-{n}"
        answer = elicit(sid, {"mode": "url", "message": "Sign in to Fake", "url": "https://example.com/fake-signin",
                              "elicitationId": eid})
        if answer is WITHDRAWN:
            stop = "cancelled"
        elif (answer or {}).get("action") == "accept":
            time.sleep(0.5)
            send({"method": "elicitation/complete", "params": {"elicitationId": eid}})
            msg("Signed in.")
        else:
            msg("Not signed in.")
    elif text == "codex ask":
        answer = elicit(sid, {"mode": "form", "toolCallId": f"call_{n}", "message": "Codex needs your input to continue.",
                              "requestedSchema": {"type": "object", "properties": {
                                  "colour": {"title": "Which colour do you prefer, red or blue?", "description": "Colour",
                                             "_meta": {"codex": {"isOther": True, "isSecret": False}}, "type": "string",
                                             "oneOf": [{"const": "Red", "title": "Red", "description": "Choose red."},
                                                       {"const": "Blue", "title": "Blue", "description": "Choose blue."},
                                                       {"const": "None of the above", "title": "None of the above",
                                                        "description": "Provide a different answer in the note field."}]},
                                  "colour_note": {"type": "string", "title": "Additional answer or note",
                                                  "_meta": {"codex": {"questionId": "colour", "role": "user_note"}}}},
                                  "required": ["colour"]},
                              "_meta": {"codex": {"autoResolutionMs": None}}})
        if answer is WITHDRAWN:
            stop = "cancelled"
        else:
            msg(f"Codex got: {json.dumps(answer, sort_keys=True)}")
    elif text.startswith("mcp "):
        _, tool, *rest = text.split(" ", 2)
        args = json.loads(rest[0]) if rest else {}
        msg("MCP " + json.dumps(mcp_call(s.get("mcp"), tool, args)))
    elif text == "meta":
        msg("META " + json.dumps(s.get("meta"), sort_keys=True))
    elif text == "servers":
        msg("SERVERS " + json.dumps(expand(s.get("mcp")), sort_keys=True))
    elif text == "model":
        msg(f"Model: {s.get('model', 'default')}")
    elif text == "mode":
        msg(f"Mode: {s.get('mode', 'default')}")
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
        caps.update(p.get("clientCapabilities") or {})
        # Run as `fountain acp --agent no-load-session`: an agent that can't
        # load or resume a session (M45b's Follow refuses it).
        no_load = "--agent" in sys.argv[:-1] and sys.argv[sys.argv.index("--agent") + 1] == "no-load-session"
        agent_caps = {"mcpCapabilities": {"http": True}}
        if "--images" in sys.argv:
            agent_caps["promptCapabilities"] = {"image": True}
        if not no_load:
            agent_caps.update({"loadSession": True, "sessionCapabilities": {"resume": {}, "fork": {}}})
        send({"id": mid, "result": {"protocolVersion": 1, "agentCapabilities": agent_caps,
            "agentInfo": {"name": "fake-acp", "version": "1"}, "authMethods": []}})
    elif method == "session/new":
        sid = f"fake-{os.getpid()}-{int(time.time() * 1000)}"
        save(sid, {"updates": [], "cwd": p.get("cwd"), "mcp": p.get("mcpServers"), "meta": p.get("_meta")})
        with open(os.path.join(DIR, f"mcp-{sid}.json"), "w") as f:
            json.dump(p.get("mcpServers"), f)
        send({"id": mid, "result": {"sessionId": sid}})
    elif method in ("session/load", "session/resume"):
        s = load(p.get("sessionId", ""))
        if s is None:
            send({"id": mid, "error": {"code": -32002, "message": "no such session"}})
            return
        s["mcp"] = p.get("mcpServers")
        if "_meta" in p:
            s["meta"] = p["_meta"]
        save(p["sessionId"], s)
        holding(p["sessionId"])
        if method == "session/load":
            for u in s["updates"]:
                send({"method": "session/update", "params": {"sessionId": p["sessionId"], "update": u}})
        send({"id": mid, "result": {"configOptions": CONFIG_OPTIONS}})
    elif method == "session/fork":
        s = load(p.get("sessionId", ""))
        if s is None:
            send({"id": mid, "error": {"code": -32002, "message": "no such session"}})
            return
        sid = f"fork-{os.getpid()}-{int(time.time() * 1000)}"
        s = dict(s, forked_from=p["sessionId"], meta=p.get("_meta"))
        save(sid, s)
        send({"id": mid, "result": {"sessionId": sid}})
    elif method == "session/set_config_option":
        s = load(p.get("sessionId", ""))
        if s is not None and p.get("configId") == "model":
            s["model"] = p.get("value")
            save(p["sessionId"], s)
        send({"id": mid, "result": {"configOptions": CONFIG_OPTIONS}})
    elif method == "session/set_mode":
        s = load(p.get("sessionId", ""))
        if s is None or p.get("modeId") not in ("default", "acceptEdits", "plan", "auto"):
            send({"id": mid, "error": {"code": -32602, "message": "Invalid Mode"}})
            return
        s["mode"] = p["modeId"]
        save(p["sessionId"], s)
        send({"id": mid, "result": {}})
    elif method == "session/prompt":
        threading.Thread(target=prompt, args=(mid, p), daemon=True).start()
    elif method == "session/cancel":
        sid = p.get("sessionId")
        rid = asking.get(sid)
        if rid is not None and rid in waiting:
            # As claude-agent-acp does: withdraw our own open request; a late
            # answer to it is ignored.
            send({"method": "$/cancel_request", "params": {"requestId": rid}})
            waiting[rid][1] = WITHDRAWN
            waiting[rid][0].set()
        else:
            cancelled.add(sid)
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
