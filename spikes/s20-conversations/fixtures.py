#!/usr/bin/env python3
"""Writes fixtures/ for M33's converter and index.

- shapes/*.jsonl: lines from real transcripts with the shape kept (types,
  keys, uuids and their links, tool names and ids, flags) and every free
  string redacted, since this repo is public.
- scratch/: S20's own session (synthetic: a CLI session, then turns through
  the adapter, then a fork), with the home directory rewritten and
  attachment bodies dropped.
"""
import json, glob, os, re, shutil, sys

HOME = os.path.expanduser("~")
ROOT = f"{HOME}/.claude/projects"
OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "fixtures")
KEEP = {"type", "subtype", "role", "name", "id", "tool_use_id", "uuid", "parentUuid", "logicalParentUuid",
        "sessionId", "leafUuid", "entrypoint", "version", "userType", "agentId", "media_type", "stop_reason",
        "level", "trigger", "permissionMode", "operation", "model", "timestamp", "promptId", "requestId",
        "headUuid", "cliSessionId", "titleSource", "effort", "anchorUuid", "tailUuid", "messageUuid", "toolUseID", "continuedInSessionId", "agentType"}

def red(v, key=None):
    if isinstance(v, dict): return {k: red(x, k) for k, x in v.items()}
    if isinstance(v, list): return [red(x, key) for x in v]
    if isinstance(v, str):
        if key in KEEP or key == "uuids" or key == "allUuids": return v
        if key in ("signature",): return "<sig>"
        if key == "data": return "iVBORw0KGgo="  # image bytes
        if key in ("cwd", "relocatedCwd", "file_path", "path"): return "/work/project" + ("/file.rs" if key in ("file_path", "path") else "")
        if key == "gitBranch": return "main"
        return f"<{len(v)} chars>"
    return v

def lines(path):
    return [json.loads(l) for l in open(path, errors="replace") if l.strip()]

def find(pred, files=None):
    for p in files or sorted(glob.glob(f"{ROOT}/*/*.jsonl"), key=os.path.getmtime, reverse=True):
        rows = lines(p)
        for i, o in enumerate(rows):
            if pred(o): return p, rows, i
    sys.exit(f"no line for {pred}")

def tool_use(name):
    return lambda o: any(isinstance(b, dict) and b.get("type") == "tool_use" and b.get("name") == name
                         for b in ((o.get("message") or {}).get("content") or []) if not isinstance(o["message"]["content"], str))

def with_result(rows, i):
    """The tool_use line and every line up to and including its tool_result."""
    ids = {b["id"] for b in rows[i]["message"]["content"] if b.get("type") == "tool_use"}
    for j in range(i + 1, len(rows)):
        c = (rows[j].get("message") or {}).get("content")
        if isinstance(c, list) and any(b.get("tool_use_id") in ids for b in c if isinstance(b, dict)):
            return rows[i:j + 1]
    return rows[i:i + 1]

def write(name, rows):
    with open(f"{OUT}/shapes/{name}.jsonl", "w") as f:
        for o in rows: f.write(json.dumps(red(o)) + "\n")

shutil.rmtree(OUT, ignore_errors=True); os.makedirs(f"{OUT}/shapes"); os.makedirs(f"{OUT}/scratch")

for name, tool in [("bash", "Bash"), ("edit", "Edit"), ("ask-user-question", "AskUserQuestion")]:
    p, rows, i = find(tool_use(tool)); write(name, with_result(rows, i))
p, rows, i = find(lambda o: isinstance((o.get("message") or {}).get("content"), list) and any(b.get("type") == "image" for b in o["message"]["content"] if isinstance(b, dict)))
write("image", rows[max(0, i - 1):i + 2])
p, rows, i = find(lambda o: o.get("subtype") == "compact_boundary")
write("compaction", rows[i - 2:i + 12])
p, rows, i = find(lambda o: o.get("subtype") == "api_error"); write("api-error", rows[i:i + 1])
p, rows, i = find(lambda o: isinstance((o.get("message") or {}).get("content"), str) and o["message"]["content"].startswith("<command-name>"))
write("local-command", rows[max(0, i - 1):i + 2])
p, rows, i = find(lambda o: o.get("type") == "assistant" and (o.get("message") or {}).get("content") and o["message"]["content"][0].get("type") == "thinking")
write("thinking-then-text", rows[i:i + 3])
# parallel tool calls: an assistant tool_use line whose child is another tool_use line
def parallel(o, cache={}):
    return False
p, rows, i = find(lambda o: False) if False else (None, None, None)
for p in sorted(glob.glob(f"{ROOT}/*/*.jsonl"), key=os.path.getmtime, reverse=True):
    rows = lines(p); by = {o.get("uuid"): o for o in rows}
    hit = next((j for j, o in enumerate(rows) if o.get("type") == "assistant" and tool_use("Read")(o)
                and (by.get(o.get("parentUuid")) or {}).get("type") == "assistant" and tool_use("Read")(by[o["parentUuid"]])), None)
    if hit is not None:
        k = rows.index(by[rows[hit]["parentUuid"]]); write("parallel-tool-calls", rows[k:hit + 4]); break
# a subagent: the Agent tool call and the first lines of its own transcript
p, rows, i = find(tool_use("Agent"))
write("subagent-call", with_result(rows, i)[:1])
sub = sorted(glob.glob(f"{p[:-6]}/subagents/agent-*.jsonl"))
if sub:
    write("subagent-transcript", lines(sub[0])[:3])
    meta = sub[0][:-6] + ".meta.json"
    if os.path.exists(meta): json.dump(red(json.load(open(meta))), open(f"{OUT}/shapes/subagent-transcript.meta.json", "w"))
# a rewind: a prompt whose parent already had a later child
for p in glob.glob(f"{ROOT}/*/*.jsonl"):
    rows = lines(p); kids = {}; hit = None
    for j, o in enumerate(rows):
        if o.get("type") not in ("user", "assistant") or o.get("isSidechain"): continue
        par = o.get("parentUuid")
        if o["type"] == "user" and isinstance(o["message"]["content"], str) and not o.get("isMeta") and par in kids:
            hit = (kids[par], j); break
        if par: kids.setdefault(par, j)
    if hit:
        a, b = hit; write("rewind", [o for o in rows[a - 1:b + 2] if o.get("type") in ("user", "assistant")]); break
# metadata lines
meta_rows = []
for t in ("ai-title", "custom-title", "agent-name", "last-prompt", "continued-in", "relocated"):
    try: meta_rows.append(find(lambda o, t=t: o.get("type") == t)[1:][0][find(lambda o, t=t: o.get("type") == t)[2]])
    except SystemExit: pass
write("metadata", meta_rows)
# a live-session file
s = sorted(glob.glob(f"{HOME}/.claude/sessions/*.json"))
if s:
    o = json.load(open(s[0])); o.update(cwd="/work/project", messagingSocketPath="/run/user/1000/cc-socks/1.sock", name="<name>")
    json.dump(o, open(f"{OUT}/shapes/live-session.json", "w"), indent=1)

# S20's own session and its fork, verbatim but for paths and attachments
scr = f"{ROOT}/-home-jake-dev-jhgaylor-illogical-spikes-s20-conversations-work-scratch"
first = None
for p in [f"{scr}/a683c96a-c2b1-4ed7-bdd4-51b7d759125b.jsonl", f"{scr}/d1ccc1e4-4b63-4b89-80b7-38128ee9d8cc.jsonl"]:
    rows = lines(p)
    if not any(o.get("type") == "user" and o.get("message", {}).get("content") for o in rows): continue
    with open(f"{OUT}/scratch/{os.path.basename(p)}", "w") as f:
        for o in rows:
            if o.get("type") == "attachment":
                kind = (o.get("attachment") or {}).get("type")
                o = {k: v for k, v in o.items() if k in ("type", "uuid", "parentUuid", "sessionId", "timestamp", "isSidechain")}
                o["attachment"] = {"type": kind}
            elif o.get("isMeta") or o.get("type") == "system":
                o = red(o)
            for k in ("hookAdditionalContext", "hookInfos", "modelUsage"):
                o.pop(k, None)
            s = json.dumps(o).replace(HOME, "/home/user").replace("/home/user/dev/jhgaylor/illogical", "/home/user/illogical").replace("-home-jake-dev-jhgaylor-illogical", "-home-user-illogical")
            s = re.sub(r'"signature": "[^"]*"', '"signature": "<sig>"', s)
            f.write(s + "\n")
print("fixtures:", sorted(os.listdir(f"{OUT}/shapes")), sorted(os.listdir(f"{OUT}/scratch")))

# The desktop app's Code tab: its own record of a session, and the start of
# the session's transcript (S20 Q6).
desk = sorted(glob.glob(f"{HOME}/.config/Claude/claude-code-sessions/*/*/local_*.json"))
if desk:
    o = json.load(open(desk[0]))
    o["enabledMcpTools"] = {"<server>:<tool>": True}
    o = {k: v for k, v in o.items() if not isinstance(v, (list, dict)) or k == "enabledMcpTools"}  # connector configs name people
    json.dump(red(o), open(f"{OUT}/shapes/desktop-session.json", "w"), indent=1)
    p = glob.glob(f"{ROOT}/*/{o['cliSessionId']}.jsonl")
    if p: write("desktop-transcript", [r for r in lines(p[0]) if r.get("type") in ("user", "assistant", "custom-title", "agent-name")])
