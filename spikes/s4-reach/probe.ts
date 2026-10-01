// S4 harness: drives any Sprites-compatible API (Fly or wisp).
// env: SPRITES_API_URL (default https://api.sprites.dev), SPRITE_TOKEN.
// usage: bun probe.ts <cmd> <sprite> [args]
const BASE = process.env.SPRITES_API_URL ?? "https://api.sprites.dev";
const TOKEN = process.env.SPRITE_TOKEN ?? "";
const WS_BASE = BASE.replace(/^http/, "ws");
const auth = { Authorization: `Bearer ${TOKEN}` };
const t0 = performance.now();
const ms = () => Math.round(performance.now() - t0);
const sleep = (n: number) => new Promise((r) => setTimeout(r, n));
const log = (...a: unknown[]) => console.log(`[${ms()}ms]`, ...a);

async function api(method: string, path: string, body?: BodyInit, query = "") {
  const headers = typeof body === "string" ? { ...auth, "Content-Type": "application/json" } : auth;
  const r = await fetch(`${BASE}/v1/sprites${path}${query}`, { method, headers, body });
  const text = await r.text();
  if (!r.ok) throw new Error(`${method} ${path} -> ${r.status} ${text.slice(0, 300)}`);
  try { return JSON.parse(text); } catch { return text; }
}
const status = async (name: string) => (await api("GET", `/${name}`)).status as string;

function open(url: string): Promise<WebSocket> {
  return new Promise((res, rej) => {
    const ws = new WebSocket(url, { headers: auth } as any);
    ws.binaryType = "arraybuffer";
    ws.onopen = () => res(ws);
    ws.onerror = (e) => rej(new Error(`ws error ${url}: ${(e as any).message ?? e}`));
  });
}

// A TTY exec session: collects raw output, records control frames.
async function tty(name: string, cmd: string[], extra = "") {
  const q = cmd.map((c) => `cmd=${encodeURIComponent(c)}`).join("&");
  const ws = await open(`${WS_BASE}/v1/sprites/${name}/exec?tty=true&${q}&env=TERM=xterm-256color${extra}`);
  return wrap(ws);
}
function wrap(ws: WebSocket) {
  const s = { ws, out: "", bytes: 0, info: null as any, exit: null as any, closed: false };
  ws.onmessage = (m) => {
    if (typeof m.data === "string") {
      const j = JSON.parse(m.data);
      if (j.type === "session_info") { s.info = j; ws.send(JSON.stringify({ type: "resize", cols: 120, rows: 40 })); }
      else if (j.type === "exit") s.exit = j;
    } else { const t = new TextDecoder().decode(m.data); s.out += t; s.bytes += m.data.byteLength; }
  };
  ws.onclose = () => { s.closed = true; };
  return s;
}
const until = async (f: () => boolean, max = 30000) => { const end = Date.now() + max; while (!f() && Date.now() < end) await sleep(50); return f(); };
const send = (ws: WebSocket, s: string) => ws.send(new TextEncoder().encode(s)); // raw, no 0x00 prefix in TTY mode

const [cmd, name, ...rest] = process.argv.slice(2);
const cmds: Record<string, () => Promise<void>> = {
  async create() { log(await api("POST", "", JSON.stringify({ name }), "")); },
  async delete() { log(await api("DELETE", `/${name}`)); },
  async status() { log(await api("GET", `/${name}`)); },
  async sessions() { log(JSON.stringify(await api("GET", `/${name}/exec`))); },
  async run() { // one-shot, via a TTY session so it wakes the sprite like a user would
    const s = await tty(name, ["bash", "-lc", rest.join(" ")], "&max_run_after_disconnect=30s");
    await until(() => s.exit !== null || s.closed, 60000);
    process.stdout.write(s.out); log("exit", s.exit);
  },
  // Replay: print N numbered lines in a detached-capable session, detach, reattach, see what comes back.
  async replay() {
    const n = Number(rest[0] ?? 200000);
    const s = await tty(name, ["bash"], "&max_run_after_disconnect=10m");
    await until(() => s.info !== null);
    send(s.ws, `seq -f 'L%09g' 1 ${n}; echo REPLAY-$((40+2))-DONE\n`); // marker only appears in output, not the echo
    await until(() => s.out.includes("REPLAY-42-DONE"), 120000);
    const live = s.bytes, id = s.info.session_id;
    s.ws.close(); await sleep(1500);
    const a = wrap(await open(`${WS_BASE}/v1/sprites/${name}/exec/${id}`));
    await sleep(4000);
    const lines = a.out.split(/\r?\n/).filter((l) => /^L\d{9}$/.test(l));
    log(JSON.stringify({ session: id, live_bytes: live, replay_bytes: a.bytes, replay_info: a.info,
      first_line: lines[0], last_line: lines.at(-1), lines: lines.length }));
    send(a.ws, "exit\n"); await sleep(500); a.ws.close();
  },
  // Start a long-lived detached session (an idle shell, or a busy loop), leave it, print its id.
  async detach() {
    const kind = rest[0] ?? "idle";
    const s = await tty(name, ["bash"], "&max_run_after_disconnect=2h");
    await until(() => s.info !== null);
    if (kind === "busy") send(s.ws, "while true; do date +%T; sleep 5; done\n");
    await sleep(1500); s.ws.close(); await sleep(500);
    log(JSON.stringify({ detached_session: s.info.session_id, kind }));
  },
  async attach() {
    const a = wrap(await open(`${WS_BASE}/v1/sprites/${name}/exec/${rest[0]}`));
    await sleep(3000);
    log(JSON.stringify({ info: a.info, bytes: a.bytes, tail: a.out.slice(-400) }));
    a.ws.close();
  },
  async kill() { log(await api("POST", `/${name}/exec/${rest[0]}/kill`).catch((e) => String(e))); },
  // Poll status (control-plane call, should not count as activity) and print transitions.
  async watch() {
    const mins = Number(rest[0] ?? 5); let last = ""; const end = Date.now() + mins * 60000;
    while (Date.now() < end) {
      const st = await status(name);
      if (st !== last) { log(`status ${st}`); last = st; }
      await sleep(5000);
    }
    log(`final ${last}`);
  },
  // Install the probe binary as a runtime-owned service (no --http-port).
  async install() {
    const bin = await Bun.file(rest[0]).arrayBuffer();
    log(await api("PUT", `/${name}/fs/write`, bin, `?path=/home/sprite/s4-probe&mode=0755&mkdir=true`));
    const s = await tty(name, ["sprite-env", "services", "create", "s4", "--cmd", "/home/sprite/s4-probe",
      "--args", "serve,127.0.0.1:7681", "--duration", "3s"], "&max_run_after_disconnect=30s");
    await until(() => s.exit !== null || s.closed, 30000);
    process.stdout.write(s.out); log("exit", s.exit);
  },
  // Hold an idle proxy connection open for N seconds and report whether it survived.
  async hold() {
    const secs = Number(rest[0] ?? 90);
    const ws = await open(`${WS_BASE}/v1/sprites/${name}/proxy`);
    let closedAt = -1; ws.onclose = () => { closedAt = ms(); };
    ws.onmessage = () => {};
    ws.send(JSON.stringify({ host: "localhost", port: 7681 }));
    await until(() => closedAt >= 0, secs * 1000);
    log(JSON.stringify({ held_s: secs, closed_at_ms: closedAt }));
    if (closedAt < 0) ws.close();
  },
  // Reach the in-sprite daemon through the authenticated TCP proxy; time first byte and a round trip.
  async proxy() {
    const before = await status(name);
    const ws = await open(`${WS_BASE}/v1/sprites/${name}/proxy`);
    const opened = ms();
    let out = "", firstByte = -1, ctrl: string[] = [];
    ws.onmessage = (m) => {
      if (typeof m.data === "string") { ctrl.push(m.data); return; }
      if (firstByte < 0) firstByte = ms();
      out += new TextDecoder().decode(m.data);
    };
    ws.send(JSON.stringify({ host: "localhost", port: Number(rest[0] ?? 7681) }));
    await until(() => firstByte >= 0, 30000);
    const rt0 = ms();
    send(ws, "echo PROBE-$((6*7)); uname -m; tty; cat /proc/1/comm\n");
    await until(() => out.includes("PROBE-42"), 15000);
    const rt = ms() - rt0;
    await sleep(800);
    log(JSON.stringify({ status_before: before, ws_open_ms: opened, first_byte_ms: firstByte, echo_rt_ms: rt, ctrl,
      tail: out.slice(-300) }));
    send(ws, "exit\n"); await sleep(300); ws.close();
  },
};
if (!cmds[cmd]) { console.error(`commands: ${Object.keys(cmds).join(" ")}`); process.exit(2); }
await cmds[cmd]();
process.exit(0);
