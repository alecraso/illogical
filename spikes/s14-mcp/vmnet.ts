// S14: can a process in a wisp sprite reach an MCP endpoint on the host?
// env: SPRITES_API_URL (default local wisp), SPRITE_TOKEN.
// usage: bun vmnet.ts create|delete|status NAME
//        bun vmnet.ts run NAME 'shell line'          non-TTY exec, prints stdout/stderr/exit
//        bun vmnet.ts relay NAME SOCKET -- CMD...    host-side bridge (below)
// Every sprite name must start with illogical-s14-.
//
// relay: opens a non-TTY exec in the sprite running a one-connection Unix
// socket listener (python3) whose stdin/stdout are the exec stream, and pipes
// that stream to CMD on the host (e.g. `s14-mcp stdio`). A process in the
// sprite that connects to SOCKET is then talking stdio MCP to the host
// server, over a connection the host opened.
import { spawn } from "node:child_process";

const BASE = process.env.SPRITES_API_URL ?? "http://127.0.0.1:7788";
const TOKEN = process.env.SPRITE_TOKEN ?? "";
const WS_BASE = BASE.replace(/^http/, "ws");
const auth = { Authorization: `Bearer ${TOKEN}` };
const PREFIX = "illogical-s14-";
const guard = (n: string) => { if (!n?.startsWith(PREFIX)) throw new Error(`refusing sprite ${n}`); return n; };
const T0 = performance.now();
const log = (...a: unknown[]) => console.error(`[${Math.round(performance.now() - T0)}ms]`, ...a);

async function api(method: string, path: string, body?: string) {
  const headers = body ? { ...auth, "Content-Type": "application/json" } : auth;
  const r = await fetch(`${BASE}/v1/sprites${path}`, { method, headers, body });
  const t = await r.text();
  let j: any; try { j = JSON.parse(t); } catch { j = t; }
  return { status: r.status, body: j };
}

function exec(name: string, cmd: string[], extra = "") {
  const q = cmd.map((c) => `cmd=${encodeURIComponent(c)}`).join("&");
  const ws = new WebSocket(`${WS_BASE}/v1/sprites/${guard(name)}/exec?${q}&stdin=true${extra}`, { headers: auth } as any);
  ws.binaryType = "arraybuffer";
  return ws;
}
const frame = (id: number, data: Uint8Array) => { const b = new Uint8Array(data.length + 1); b[0] = id; b.set(data, 1); return b; };

const [cmd, name, ...rest] = process.argv.slice(2);
if (cmd === "create") log((await api("POST", "", JSON.stringify({ name: guard(name) }))).status);
else if (cmd === "delete") { log("DELETE", (await api("DELETE", `/${guard(name)}`)).status, "GET after", (await api("GET", `/${guard(name)}`)).status); }
else if (cmd === "status") log(JSON.stringify((await api("GET", `/${guard(name)}`)).body));
else if (cmd === "run" || cmd === "runfile") {
  const line = cmd === "runfile" ? await Bun.file(rest[0]).text() : rest.join(" ");
  const ws = exec(name, ["bash", "-lc", line]);
  const dec = new TextDecoder();
  await new Promise<void>((res) => {
    ws.onopen = () => ws.send(frame(4, new Uint8Array()));
    ws.onmessage = (m) => {
      if (typeof m.data === "string") { const j = JSON.parse(m.data); if (j.type !== "session_info") log("ctl", m.data); return; }
      const b = new Uint8Array(m.data);
      if (b[0] === 1) process.stdout.write(dec.decode(b.subarray(1)));
      else if (b[0] === 2) process.stderr.write(dec.decode(b.subarray(1)));
      else if (b[0] === 3) log("exit", b[1]);
    };
    ws.onclose = (e) => { log("closed", e.code); res(); };
    ws.onerror = (e) => { log("ws error", (e as any).message); };
  });
} else if (cmd === "relay") {
  const sock = rest[0];
  const hostCmd = rest.slice(rest.indexOf("--") + 1);
  const py = `
import socket, os, sys, threading
p = ${JSON.stringify(sock)}
try: os.unlink(p)
except FileNotFoundError: pass
s = socket.socket(socket.AF_UNIX); s.bind(p); s.listen(1)
print("relay listening", p, file=sys.stderr, flush=True)
c, _ = s.accept()
print("relay accepted", file=sys.stderr, flush=True)
def up():
    while True:
        d = c.recv(65536)
        if not d: break
        os.write(1, d)
    os.close(1)
threading.Thread(target=up, daemon=True).start()
while True:
    d = os.read(0, 65536)
    if not d: break
    c.sendall(d)
`;
  const ws = exec(name, ["python3", "-c", py]);
  const child = spawn(hostCmd[0], hostCmd.slice(1), { stdio: ["pipe", "pipe", "inherit"] });
  ws.onopen = () => log("exec open; host side:", hostCmd.join(" "));
  child.stdout.on("data", (d: Buffer) => { log("host->guest", d.length, "bytes"); ws.send(frame(0, new Uint8Array(d))); });
  ws.onmessage = (m) => {
    if (typeof m.data === "string") { log("ctl", m.data.slice(0, 200)); return; }
    const b = new Uint8Array(m.data);
    if (b[0] === 1) { log("guest->host", b.length - 1, "bytes"); child.stdin.write(b.subarray(1)); }
    else if (b[0] === 2) process.stderr.write("[guest] " + new TextDecoder().decode(b.subarray(1)));
    else if (b[0] === 3) { log("relay exited", b[1]); child.kill(); }
  };
  ws.onclose = (e) => { log("exec closed", e.code); child.kill(); process.exit(0); };
} else console.error("usage: bun vmnet.ts create|delete|status|run|relay NAME ...");
