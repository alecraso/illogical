#!/usr/bin/env python3
"""S17: a throwaway fake IDE for Claude Code (what illogicald would be).

usage: work/venv/bin/python fake_ide.py <name> <workspace> [--ide-name NAME] [--port N] [--token T] [--no-auth-check] [--no-folders]

- Listens on 127.0.0.1:<port> (random if not given) for WebSocket subprotocol "mcp".
- Writes the lockfile ~/.claude/ide/<port>.lock the way the IDE extensions do:
  {pid, workspaceFolders, ideName, transport: "ws", authToken}. Removes it on exit (SIGINT/SIGTERM/normal).
- Speaks MCP (JSON-RPC 2.0): initialize, tools/list, tools/call; logs every frame with a timestamp to
  work/ide-<name>.jsonl (and the upgrade request's headers).
- openDiff waits for a decision. The decision comes from work/ide-<name>.mode (read when the call
  arrives): accept | reject | close | edit | hold. "hold" waits until a line is appended to
  work/ide-<name>.cmd: "accept", "reject", "close", "edit".
- Other lines in work/ide-<name>.cmd send notifications to Claude Code:
    sel <file> <line> <col> <endline> <endcol> [text]   -> selection_changed
    at <file> <line0> <line1>                            -> at_mentioned
- Prints "port <n>" on stdout once listening.
"""
import argparse, asyncio, json, os, secrets, signal, sys, time

from websockets.asyncio.server import serve
from websockets.http11 import Response
from websockets.datastructures import Headers

HERE = os.path.dirname(os.path.abspath(__file__))
WORK = os.path.join(HERE, "work")
IDE_DIR = os.path.expanduser("~/.claude/ide")

ap = argparse.ArgumentParser()
ap.add_argument("name")
ap.add_argument("workspace")
ap.add_argument("--ide-name", default="illogical")
ap.add_argument("--port", type=int, default=0)
ap.add_argument("--token", default=None)
ap.add_argument("--no-auth-check", action="store_true")
ap.add_argument("--no-folders", action="store_true", help="lockfile workspaceFolders: [] (only CLAUDE_CODE_SSE_PORT picks it)")
args = ap.parse_args()

token = args.token or secrets.token_hex(16)
base = os.path.join(WORK, f"ide-{args.name}")
logf = open(base + ".jsonl", "a")
t0 = time.time()
lockpath = None
conns = set()
pending = []  # futures waiting for a "hold" decision


def log(kind, **kw):
    kw.update(t=round(time.time() - t0, 3), kind=kind)
    logf.write(json.dumps(kw) + "\n")
    logf.flush()


TOOLS = [
    ("openDiff", {"old_file_path": "string", "new_file_path": "string", "new_file_contents": "string", "tab_name": "string"}),
    ("openFile", {"filePath": "string", "preview": "boolean", "startText": "string", "endText": "string", "selectToEndOfLine": "boolean", "makeFrontmost": "boolean"}),
    ("close_tab", {"tab_name": "string"}),
    ("closeAllDiffTabs", {}),
    ("getDiagnostics", {"uri": "string"}),
    ("getCurrentSelection", {}),
    ("getLatestSelection", {}),
    ("getOpenEditors", {}),
    ("getWorkspaceFolders", {}),
    ("checkDocumentDirty", {"filePath": "string"}),
    ("saveDocument", {"filePath": "string"}),
    ("executeCode", {"code": "string"}),
]


def tool_list():
    out = []
    for name, props in TOOLS:
        out.append({
            "name": name,
            "description": f"fake {name}",
            "inputSchema": {"type": "object", "properties": {k: {"type": v} for k, v in props.items()}},
        })
    return out


def text(*parts):
    return {"content": [{"type": "text", "text": p} for p in parts]}


def read_mode():
    try:
        return open(base + ".mode").read().strip() or "hold"
    except FileNotFoundError:
        return "hold"


async def decide_diff(params):
    mode = read_mode()
    if mode == "hold":
        fut = asyncio.get_running_loop().create_future()
        pending.append(fut)
        log("diff_pending", tab=params.get("tab_name"), path=params.get("new_file_path"))
        mode = await fut
    contents = params.get("new_file_contents", "")
    if mode == "accept":
        return text("FILE_SAVED", contents)
    if mode == "edit":
        return text("FILE_SAVED", contents + "# edited in the fake IDE before accepting\n")
    if mode == "close":
        return text("TAB_CLOSED")
    return text("DIFF_REJECTED", params.get("tab_name", ""))


async def call_tool(name, params):
    ws_folder = os.path.abspath(args.workspace)
    if name == "openDiff":
        return await decide_diff(params)
    if name == "getDiagnostics":
        uri = params.get("uri")
        return text(json.dumps([{"uri": uri or f"file://{ws_folder}/x", "diagnostics": []}] if uri else []))
    if name in ("getCurrentSelection", "getLatestSelection"):
        return text(json.dumps({"success": False, "message": "No active editor found"}))
    if name == "getOpenEditors":
        return text(json.dumps({"tabs": []}))
    if name == "getWorkspaceFolders":
        return text(json.dumps({"success": True, "folders": [{"name": os.path.basename(ws_folder), "uri": f"file://{ws_folder}", "path": ws_folder}], "rootPath": ws_folder}))
    if name in ("close_tab", "closeAllDiffTabs"):
        # The diff's card closes: whoever answered, it wasn't us.
        while pending:
            fut = pending.pop(0)
            if not fut.done():
                fut.set_result("close")
        return text("TAB_CLOSED")
    if name == "checkDocumentDirty":
        return text(json.dumps({"success": True, "filePath": params.get("filePath"), "isDirty": False, "isUntitled": False}))
    if name == "saveDocument":
        return text(json.dumps({"success": True, "saved": False, "message": "not open"}))
    return text(json.dumps({"success": True}))


async def handle(ws):
    conns.add(ws)
    log("open", path=ws.request.path, headers=dict(ws.request.headers.raw_items()), subprotocol=ws.subprotocol)
    try:
        async for raw in ws:
            msg = json.loads(raw)
            log("in", msg=msg)
            mid, method = msg.get("id"), msg.get("method")
            if method is None:
                continue
            if mid is None:  # notification
                continue

            async def reply(result, mid=mid):
                out = {"jsonrpc": "2.0", "id": mid, "result": result}
                log("out", msg=out)
                await ws.send(json.dumps(out))

            if method == "initialize":
                await reply({
                    "protocolVersion": msg["params"].get("protocolVersion", "2025-06-18"),
                    "capabilities": {"tools": {"listChanged": False}},
                    "serverInfo": {"name": "illogical-fake-ide", "version": "0.0.1"},
                })
            elif method == "tools/list":
                await reply({"tools": tool_list()})
            elif method == "tools/call":
                p = msg["params"]
                asyncio.create_task(reply_later(reply, p["name"], p.get("arguments") or {}))
            elif method in ("prompts/list",):
                await reply({"prompts": []})
            elif method in ("resources/list",):
                await reply({"resources": []})
            else:
                err = {"jsonrpc": "2.0", "id": mid, "error": {"code": -32601, "message": f"no {method}"}}
                log("out", msg=err)
                await ws.send(json.dumps(err))
    except Exception as e:  # noqa
        log("error", error=repr(e))
    finally:
        conns.discard(ws)
        log("close", code=ws.close_code)


async def reply_later(reply, name, params):
    await reply(await call_tool(name, params))


def check(conn, request):
    got = request.headers.get("X-Claude-Code-Ide-Authorization")
    if not args.no_auth_check and got != token:
        log("reject", reason="bad token", got=got, headers=dict(request.headers.raw_items()))
        return Response(401, "Unauthorized", Headers(), b"bad token\n")
    return None


async def commands():
    path = base + ".cmd"
    open(path, "a").close()
    pos = os.path.getsize(path)
    while True:
        await asyncio.sleep(0.2)
        size = os.path.getsize(path)
        if size <= pos:
            continue
        with open(path) as f:
            f.seek(pos)
            chunk = f.read()
        pos = size
        for line in chunk.splitlines():
            parts = line.split()
            if not parts:
                continue
            log("cmd", line=line)
            if parts[0] in ("accept", "reject", "close", "edit"):
                while pending:
                    fut = pending.pop(0)
                    if not fut.done():
                        fut.set_result(parts[0])
            elif parts[0] == "sel":
                f_, l0, c0, l1, c1 = parts[1], *map(int, parts[2:6])
                note = {"jsonrpc": "2.0", "method": "selection_changed", "params": {
                    "text": " ".join(parts[6:]), "filePath": f_, "fileUrl": f"file://{f_}",
                    "selection": {"start": {"line": l0, "character": c0}, "end": {"line": l1, "character": c1}, "isEmpty": l0 == l1 and c0 == c1}}}
                await broadcast(note)
            elif parts[0] == "at":
                note = {"jsonrpc": "2.0", "method": "at_mentioned", "params": {"filePath": parts[1], "lineStart": int(parts[2]), "lineEnd": int(parts[3])}}
                await broadcast(note)


async def broadcast(note):
    log("out", msg=note)
    for ws in list(conns):
        await ws.send(json.dumps(note))


def cleanup(*_):
    if lockpath and os.path.exists(lockpath):
        os.unlink(lockpath)
        log("lock_removed", path=lockpath)
    os._exit(0)  # sys.exit inside asyncio's loop left the process running without its server


async def main():
    global lockpath
    async with serve(handle, "127.0.0.1", args.port, subprotocols=["mcp"], process_request=check, max_size=None) as server:
        port = server.sockets[0].getsockname()[1]
        os.makedirs(IDE_DIR, exist_ok=True)
        lockpath = os.path.join(IDE_DIR, f"{port}.lock")
        with open(lockpath, "w") as f:
            json.dump({"pid": os.getpid(), "workspaceFolders": [] if args.no_folders else [os.path.abspath(args.workspace)],
                       "ideName": args.ide_name, "transport": "ws", "runningInWindows": False, "authToken": token}, f)
        log("listening", port=port, lock=lockpath, ide=args.ide_name)
        print(f"port {port}", flush=True)
        await commands()


signal.signal(signal.SIGTERM, cleanup)
signal.signal(signal.SIGINT, cleanup)
try:
    asyncio.run(main())
finally:
    cleanup()
