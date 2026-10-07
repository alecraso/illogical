#!/usr/bin/env python3
"""A stand-in for Claude Code talking to its IDE (M28), as S17 recorded the
real one (2.1.287): find the IDE by CLAUDE_CODE_SSE_PORT among the lockfiles
in $ARUGULA_CLAUDE_IDE_DIR (else ~/.claude/ide), connect over a WebSocket
(subprotocol mcp, the lockfile's token, no Origin), initialize, say
ide_connected with our pid, list tools. Then one line of stdin at a time:

    edit FILE TEXT...   openDiff for FILE with TEXT as its contents (\\n for
                        newlines), wait, and on FILE_SAVED write what came
                        back to FILE (as Claude Code applies it)
    close               close_tab for the last diff (the terminal answered)
    quit

Prints what happens: `connected <port>`, `result FILE_SAVED`, `wrote FILE`,
`result DIFF_REJECTED`, `result TAB_CLOSED`. Standard library only.
"""
import base64, json, os, socket, struct, sys, threading


def lockfile():
    d = os.environ.get("ARUGULA_CLAUDE_IDE_DIR") or os.path.expanduser("~/.claude/ide")
    port = os.environ["CLAUDE_CODE_SSE_PORT"]
    with open(os.path.join(d, f"{port}.lock")) as f:
        return int(port), json.load(f)


class Ws:
    def __init__(self, port, token):
        self.s = socket.create_connection(("127.0.0.1", port))
        key = base64.b64encode(os.urandom(16)).decode()
        req = (
            f"GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
            f"Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Protocol: mcp\r\n"
            f"X-Claude-Code-Ide-Authorization: {token}\r\nUser-Agent: claude-code/fake (cli)\r\n\r\n"
        )
        self.s.sendall(req.encode())
        head = b""
        while b"\r\n\r\n" not in head:
            c = self.s.recv(1)
            if not c:
                raise SystemExit("refused")
            head += c
        if b" 101 " not in head.split(b"\r\n")[0]:
            raise SystemExit("refused: " + head.split(b"\r\n")[0].decode())
        self.lock = threading.Lock()

    def send(self, obj):
        data = json.dumps(obj).encode()
        mask = os.urandom(4)
        n = len(data)
        if n < 126:
            head = struct.pack("!BB", 0x81, 0x80 | n)
        elif n < 65536:
            head = struct.pack("!BBH", 0x81, 0x80 | 126, n)
        else:
            head = struct.pack("!BBQ", 0x81, 0x80 | 127, n)
        body = bytes(b ^ mask[i % 4] for i, b in enumerate(data))
        with self.lock:
            self.s.sendall(head + mask + body)

    def exact(self, n):
        out = b""
        while len(out) < n:
            c = self.s.recv(n - len(out))
            if not c:
                raise EOFError
            out += c
        return out

    def recv(self):
        while True:
            b0, b1 = self.exact(2)
            n = b1 & 0x7F
            if n == 126:
                n = struct.unpack("!H", self.exact(2))[0]
            elif n == 127:
                n = struct.unpack("!Q", self.exact(8))[0]
            data = self.exact(n)
            op = b0 & 0x0F
            if op == 1:
                return json.loads(data)
            if op == 8:
                raise EOFError


def main():
    port, lock = lockfile()
    ws = Ws(port, lock["authToken"])
    pending = {}
    done = threading.Condition()
    ws.send({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "claude-code", "version": "fake"}}})
    ws.send({"jsonrpc": "2.0", "method": "notifications/initialized"})
    ws.send({"jsonrpc": "2.0", "method": "ide_connected", "params": {"pid": os.getpid()}})
    ws.send({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})
    print(f"connected {port}", flush=True)

    def reader():
        try:
            while True:
                m = ws.recv()
                if "id" in m and m["id"] in pending:
                    with done:
                        what, path = pending.pop(m["id"])
                        parts = [c.get("text", "") for c in m.get("result", {}).get("content", [])]
                        if what == "openDiff":
                            print(f"result {parts[0] if parts else '?'}", flush=True)
                            if parts and parts[0] == "FILE_SAVED":
                                with open(path, "w") as f:
                                    f.write(parts[1] if len(parts) > 1 else "")
                                print(f"wrote {path}", flush=True)
                        done.notify_all()
                elif m.get("method") == "at_mentioned":
                    p = m["params"]
                    print(f"note at_mentioned {os.path.basename(p['filePath'])} {p['lineStart']}-{p['lineEnd']}", flush=True)
                elif m.get("method"):
                    print(f"note {m['method']}", flush=True)
        except EOFError:
            print("disconnected", flush=True)
            os._exit(0)

    threading.Thread(target=reader, daemon=True).start()
    n = 10
    last_tab = None
    for line in sys.stdin:
        words = line.strip().split(" ", 2)
        if not words or not words[0]:
            continue
        n += 1
        if words[0] == "edit":
            path = os.path.abspath(words[1])
            text = (words[2] if len(words) > 2 else "").replace("\\n", "\n")
            last_tab = f"✻ [Claude Code] {os.path.basename(path)} ⧉"
            pending[n] = ("openDiff", path)
            ws.send({"jsonrpc": "2.0", "id": n, "method": "tools/call", "params": {"name": "openDiff", "arguments": {
                "old_file_path": path, "new_file_path": path, "new_file_contents": text, "tab_name": last_tab}}})
            print(f"asked {os.path.basename(path)}", flush=True)
        elif words[0] == "close":
            pending[n] = ("close_tab", None)
            ws.send({"jsonrpc": "2.0", "id": n, "method": "tools/call", "params": {"name": "close_tab", "arguments": {"tab_name": last_tab}}})
        elif words[0] == "quit":
            break


if __name__ == "__main__":
    main()
