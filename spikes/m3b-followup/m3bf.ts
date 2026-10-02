// M3b follow-up spike: what keeps a sprite awake, slow readers, kill signals,
// the cold reboot after warm-ttl, and replay alignment.
// env: SPRITES_API_URL (default local wisp), SPRITE_TOKEN.
// usage: bun m3bf.ts <cmd> [args]   (every sprite name must start with illogical-m3bf-)
//
// Uses a raw WebSocket client over node:net (rawws) so that pings, pongs and
// reading can be controlled and every control frame is seen.
import * as net from "node:net";
import { randomBytes } from "node:crypto";
import { execSync } from "node:child_process";

const BASE = process.env.SPRITES_API_URL ?? "http://127.0.0.1:7788";
const TOKEN = process.env.SPRITE_TOKEN ?? "";
const auth = { Authorization: `Bearer ${TOKEN}` };
const PREFIX = "illogical-m3bf-";
const sleep = (n: number) => new Promise((r) => setTimeout(r, n));
const now = () => performance.now();
const T0 = now();
const log = (...a: unknown[]) => console.log(`[${new Date().toISOString().slice(11, 19)} +${Math.round(now() - T0)}ms]`, ...a);
const guard = (name: string) => { if (!name?.startsWith(PREFIX)) throw new Error(`refusing sprite ${name}`); return name; };
const until = async (f: () => boolean, max = 30000, step = 5) => {
  const end = Date.now() + max; while (!f() && Date.now() < end) await sleep(step); return f();
};
const strip = (s: string) => s.replace(/\x1b\[[0-9;?]*[ -\/]*[@-~]/g, "").replace(/\x1b\][^\x07\x1b]*(\x07|\x1b\\)/g, "").replace(/\x1b[()][0-9A-B]/g, "").replace(/\x1b[=>]/g, "");
const promptVisible = (out: string) => /[$#] $/.test(strip(out));
const wispdRss = () => { try { return Number(execSync("ps -o rss= -C wispd").toString().trim().split(/\s+/)[0]); } catch { return -1; } };

async function api(method: string, path: string, body?: BodyInit, query = "", headers: Record<string, string> = {}) {
  const h = typeof body === "string" ? { ...auth, "Content-Type": "application/json", ...headers } : { ...auth, ...headers };
  const r = await fetch(`${BASE}/v1/sprites${path}${query}`, { method, headers: h, body });
  const text = await r.text();
  let j: any; try { j = JSON.parse(text); } catch { j = text; }
  return { status: r.status, body: j, text };
}
const create = (name: string) => api("POST", "", JSON.stringify({ name: guard(name) }));
const status = async (name: string) => { const r = await api("GET", `/${guard(name)}`); return r.status === 200 ? r.body.status as string : `http-${r.status}`; };
async function ensure(name: string) {
  const st = await status(name);
  if (st.startsWith("http-404")) { const c = await create(name); log(`create ${name}: ${c.status}`); }
  else log(`${name} exists: ${st}`);
}
async function deleteAndConfirm(name: string) {
  const d = await api("DELETE", `/${guard(name)}`);
  const g = await api("GET", `/${guard(name)}`);
  log(`delete ${name}: ${d.status}; GET -> ${g.status}`);
}
function watchStatus(name: string, every = 1000) {
  const t0 = now(); const tr: { s: number; status: string }[] = []; let stop = false; let last = "";
  (async () => {
    while (!stop) {
      const st = await status(name).catch((e) => `err:${e}`);
      if (st !== last) { tr.push({ s: +((now() - t0) / 1000).toFixed(1), status: st }); last = st; log(`status ${name}: ${st}`); }
      await sleep(every);
    }
  })();
  return { tr, stop: () => { stop = true; } };
}

// ---- raw WebSocket client ----
type Ctl = { ms: number; op: string; len: number; text?: string };
class RawWS {
  sock!: net.Socket;
  t0 = now();
  hdr = Buffer.alloc(0); upgraded = false; status = 0; respHead = "";
  buf = Buffer.alloc(0);
  out: Buffer[] = []; outBytes = 0; keepOut = true;
  texts: { ms: number; text: string }[] = [];
  ctl: Ctl[] = [];
  info: any = null; exit: any = null;
  closed = false; closeInfo: any = null;
  autoPong = true;
  onBin?: (b: Buffer) => void;
  rxBytesWire = 0;
  static open(path: string, opts: { autoPong?: boolean } = {}): Promise<RawWS> {
    const w = new RawWS(); w.autoPong = opts.autoPong ?? true;
    const u = new URL(BASE);
    return new Promise((res, rej) => {
      w.sock = net.connect(Number(u.port || 80), u.hostname, () => {
        const key = randomBytes(16).toString("base64");
        w.sock.write(`GET ${path} HTTP/1.1\r\nHost: ${u.host}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: ${key}\r\nSec-WebSocket-Version: 13\r\nAuthorization: Bearer ${TOKEN}\r\n\r\n`);
      });
      w.sock.on("data", (d: Buffer) => {
        w.rxBytesWire += d.length;
        if (!w.upgraded) {
          w.hdr = Buffer.concat([w.hdr, d]);
          const i = w.hdr.indexOf("\r\n\r\n");
          if (i < 0) return;
          w.respHead = w.hdr.subarray(0, i).toString();
          w.status = Number(w.respHead.split(" ")[1]);
          const rest = w.hdr.subarray(i + 4);
          if (w.status !== 101) { rej(new Error(`upgrade ${w.status}: ${w.respHead.split("\r\n")[0]} ${rest.toString().slice(0, 200)}`)); w.sock.destroy(); return; }
          w.upgraded = true; res(w);
          if (rest.length) w.feed(rest);
        } else w.feed(d);
      });
      w.sock.on("close", () => { if (!w.closed) { w.closed = true; w.closeInfo ??= { code: 1006, ms: Math.round(now() - w.t0) }; } if (!w.upgraded) rej(new Error("closed before upgrade")); });
      w.sock.on("error", (e) => { w.closeInfo ??= { code: 1006, err: String(e), ms: Math.round(now() - w.t0) }; });
    });
  }
  feed(d: Buffer) {
    this.buf = this.buf.length ? Buffer.concat([this.buf, d]) : d;
    for (;;) {
      const b = this.buf; if (b.length < 2) return;
      const op = b[0] & 0x0f; let len = b[1] & 0x7f; let off = 2;
      if (len === 126) { if (b.length < 4) return; len = b.readUInt16BE(2); off = 4; }
      else if (len === 127) { if (b.length < 10) return; len = Number(b.readBigUInt64BE(2)); off = 10; }
      if (b.length < off + len) return;
      const p = b.subarray(off, off + len); this.buf = b.subarray(off + len);
      this.frame(op, p);
    }
  }
  frame(op: number, p: Buffer) {
    const ms = Math.round(now() - this.t0);
    if (op === 2 || op === 0) { this.outBytes += p.length; if (this.keepOut) this.out.push(Buffer.from(p)); this.onBin?.(p); }
    else if (op === 1) {
      const t = p.toString(); this.texts.push({ ms, text: t });
      try { const j = JSON.parse(t); if (j.type === "session_info") this.info = j; else if (j.type === "exit") this.exit = j; } catch {}
    } else if (op === 9) { this.ctl.push({ ms, op: "ping", len: p.length }); if (this.autoPong) this.send(0xa, p); }
    else if (op === 0xa) { this.ctl.push({ ms, op: "pong", len: p.length }); }
    else if (op === 8) {
      const code = p.length >= 2 ? p.readUInt16BE(0) : 1005;
      this.ctl.push({ ms, op: "close", len: p.length, text: `${code} ${p.subarray(2).toString()}` });
      this.closeInfo = { code, ms };
      if (!this.closed) { try { this.send(8, p.subarray(0, 2)); } catch {} }
      this.closed = true; this.sock.end();
    }
  }
  send(op: number, data: Buffer | string) {
    const p = typeof data === "string" ? Buffer.from(data) : data;
    const mask = randomBytes(4);
    let h: Buffer;
    if (p.length < 126) { h = Buffer.from([0x80 | op, 0x80 | p.length]); }
    else if (p.length < 65536) { h = Buffer.alloc(4); h[0] = 0x80 | op; h[1] = 0x80 | 126; h.writeUInt16BE(p.length, 2); }
    else { h = Buffer.alloc(10); h[0] = 0x80 | op; h[1] = 0x80 | 127; h.writeBigUInt64BE(BigInt(p.length), 2); }
    const m = Buffer.alloc(p.length); for (let i = 0; i < p.length; i++) m[i] = p[i] ^ mask[i & 3];
    this.sock.write(Buffer.concat([h, mask, m]));
  }
  input(s: string) { this.send(2, s); }
  json(o: any) { this.send(1, JSON.stringify(o)); }
  ping(s = "p") { this.send(9, s); this.ctl.push({ ms: Math.round(now() - this.t0), op: "ping-sent", len: s.length }); }
  text() { return Buffer.concat(this.out).toString("latin1"); }
  close() { if (!this.closed) { try { this.send(8, Buffer.from([0x03, 0xe8])); } catch {} } this.closed = true; setTimeout(() => this.sock.destroy(), 200); }
}

const execPath = (name: string, cmd: string[], extra = "") =>
  `/v1/sprites/${guard(name)}/exec?tty=true&${cmd.map((c) => `cmd=${encodeURIComponent(c)}`).join("&")}&env=TERM=xterm-256color&cols=120&rows=40${extra}`;
async function shell(name: string, extra = "&max_run_after_disconnect=30m", opts: { autoPong?: boolean } = {}) {
  const w = await RawWS.open(execPath(name, ["bash", "-l"], extra), opts);
  await until(() => w.info !== null && promptVisible(w.text()), 30000);
  return w;
}
let mk = 0;
async function sh(w: RawWS, line: string, max = 20000) {
  const k = `M${++mk}X${Date.now() % 100000}`; const from = w.outBytes;
  const startLen = w.text().length;
  w.input(`echo ${k}-B; ${line}; echo ${k}-$((1+1))E\n`);
  const done = await until(() => w.text().includes(`${k}-2E`, startLen), max, 10);
  const seg = strip(w.text().slice(startLen));
  const b = seg.lastIndexOf(`${k}-B\r\n`); const e = seg.lastIndexOf(`${k}-2E`);
  void from;
  return done && b >= 0 ? seg.slice(b + `${k}-B\r\n`.length, e).trim() : `TIMEOUT(${seg.slice(-200)})`;
}
// One-shot command over a fresh TTY exec; returns stripped output and exit.
async function run(name: string, line: string, max = 20000) {
  const w = await RawWS.open(execPath(name, ["bash", "-lc", line], "&max_run_after_disconnect=30s"));
  await until(() => w.closed, max);
  return { out: strip(w.text()).trim(), exit: w.exit, close: w.closeInfo };
}

const [cmd, ...rest] = process.argv.slice(2);
const cmds: Record<string, () => Promise<void>> = {
  async create() { await ensure(rest[0]); },
  async delete() { await deleteAndConfirm(rest[0]); },
  async status() { log(JSON.stringify((await api("GET", `/${guard(rest[0])}`)).body)); },
  async sessions() { log(JSON.stringify((await api("GET", `/${guard(rest[0])}/exec`)).body)); },
  async run() { const r = await run(rest[0], rest.slice(1).join(" ")); log(JSON.stringify(r)); },

  // ---- 4. cold reboot after warm-ttl ----
  // coldsetup NAME: detached shell with a 6h max_run_after_disconnect and markers.
  async coldsetup() {
    const name = guard(rest[0]); await ensure(name);
    const w = await shell(name, "&max_run_after_disconnect=6h");
    const r = await sh(w, "echo PID=$$; date +%s > /tmp/cold-mark; date +%s > ~/cold-mark; (sleep 86400 &) ; nohup sleep 86401 >/dev/null 2>&1 & echo SLEEPER=$!; cat /proc/sys/kernel/random/boot_id; uptime -s; echo COLD-BEFORE-$((6*7))");
    log("setup:", r, "session", w.info?.session_id, "bytes", w.outBytes);
    await Bun.write(`work/cold-${name}.json`, JSON.stringify({ name, session: w.info.session_id, bytes: w.outBytes, setupAt: new Date().toISOString(), setup: r, text: w.text() }));
    w.close(); await sleep(500);
  },
  // coldwatch NAME MINUTES EVERY_S: poll GET status only, log transitions.
  async coldwatch() {
    const name = guard(rest[0]); const mins = Number(rest[1] ?? 80); const every = Number(rest[2] ?? 180) * 1000;
    const t0 = Date.now(); let last = "";
    while (Date.now() - t0 < mins * 60000) {
      const r = await api("GET", `/${name}`);
      const st = r.status === 200 ? r.body.status : `http-${r.status}`;
      log(`+${((Date.now() - t0) / 60000).toFixed(1)}min status=${st}${st !== last ? " (changed)" : ""} ${JSON.stringify(r.body).slice(0, 300)}`);
      if (st !== last) last = st;
      if (st === "cold") break;
      await sleep(every);
    }
  },
  // coldwake NAME: after cold, try the old session first, then a new exec.
  async coldwake() {
    const name = guard(rest[0]); const saved = JSON.parse(await Bun.file(`work/cold-${name}.json`).text());
    log("status before:", JSON.stringify((await api("GET", `/${name}`)).body));
    const ws = watchStatus(name, 100);
    const t = now(); let res: any;
    try {
      const w = await RawWS.open(`/v1/sprites/${name}/exec/${saved.session}?output_offset=0`);
      await until(() => w.closed || (w.info && w.outBytes > 0), 15000);
      await sleep(1500);
      res = { ok: true, ms: Math.round(now() - t), info: w.info, exit: w.exit, close: w.closeInfo, replayBytes: w.outBytes, replay: strip(w.text()).slice(-400), texts: w.texts };
      w.close();
    } catch (e) { res = { ok: false, ms: Math.round(now() - t), err: String(e) }; }
    log("reattach old session:", JSON.stringify(res));
    const t2 = now();
    const r = await run(name, "cat /proc/sys/kernel/random/boot_id; uptime -s; echo tmp=$(cat /tmp/cold-mark 2>&1); echo home=$(cat ~/cold-mark 2>&1); pgrep -a sleep || echo no-sleepers");
    log(`new exec (${Math.round(now() - t2)}ms):`, JSON.stringify(r));
    log("exec list:", JSON.stringify((await api("GET", `/${name}/exec`)).body));
    ws.stop(); log("transitions:", JSON.stringify(ws.tr));
  },

  // ---- 1. what keeps a sprite awake ----
  // awake NAME MODE SECONDS; MODE: idle | nopong | ping10 | paused | detached
  async awake() {
    const name = guard(rest[0]); const mode = rest[1]; const secs = Number(rest[2] ?? 75);
    await ensure(name);
    // let it settle to warm first so every run starts from the same place
    const w = await shell(name, "&max_run_after_disconnect=10m", { autoPong: mode !== "nopong" });
    await sh(w, "true");
    const t0 = now(); let firstWarm = -1;
    let iv: any;
    if (mode === "ping10") iv = setInterval(() => w.ping("keep"), 10000);
    if (mode === "paused") w.sock.pause();
    if (mode === "detached") w.close();
    while ((now() - t0) / 1000 < secs) {
      const st = await status(name);
      if (st !== "running" && firstWarm < 0) { firstWarm = +((now() - t0) / 1000).toFixed(1); log(`${mode}: ${st} at ${firstWarm}s`); }
      await sleep(1000);
    }
    clearInterval(iv);
    if (mode === "paused") w.sock.resume();
    await sleep(500);
    log(JSON.stringify({ mode, secs, firstNotRunning: firstWarm, finalStatus: await status(name), ctl: w.ctl, closed: w.closed, closeInfo: w.closeInfo,
      texts: w.texts.map((t) => t.text.slice(0, 100)), exec: (await api("GET", `/${name}/exec`)).body }));
    if (mode !== "detached") {
      // still usable afterwards?
      if (!w.closed) log("after:", await sh(w, "echo still-$((1+1))"));
      w.close();
    }
  },

  // ---- 2. slow reader ----
  // slow NAME PAUSE_S [N]: seq 1 N with the client not reading for PAUSE_S.
  async slow() {
    const name = guard(rest[0]); const pause = Number(rest[1] ?? 45); const n = Number(rest[2] ?? 5000000);
    await ensure(name);
    const w = await shell(name, "&max_run_after_disconnect=10m");
    const rss0 = wispdRss();
    const startBytes = w.outBytes;
    const t0 = now();
    w.input(`seq 1 ${n}; echo SLOW-END-$((1+1))X\n`);
    await sleep(300);
    w.sock.pause(); log(`paused reading at ${w.outBytes - startBytes} bytes, wispd rss ${rss0}KB`);
    const samples: any[] = [];
    const tp = now();
    while ((now() - tp) / 1000 < pause) {
      const probe = await run(name, "P=$(pgrep -x seq); if [ -n \"$P\" ]; then echo seq=$P stat=$(ps -o stat= -p $P) wchan=$(cat /proc/$P/wchan) wchar=$(grep wchar /proc/$P/io | cut -d' ' -f2); else echo noseq; fi; A=$(ps -o ppid= -p $$ | tr -d \" \"); echo agent=$(ps -o comm= -p $A) agent_rss=$(ps -o rss= -p $A)", 10000).catch((e) => ({ out: String(e) }));
      samples.push({ s: +((now() - tp) / 1000).toFixed(1), wispdRssKB: wispdRss(), rxWire: w.rxBytesWire, probe: (probe as any).out?.replace(/\r?\n/g, " ") , closed: w.closed });
      log(JSON.stringify(samples.at(-1)));
      await sleep(4000);
    }
    w.sock.resume(); const tr = now(); log("resumed");
    await until(() => w.closed || w.text().includes("SLOW-END-2X"), 120000, 20);
    log(`after resume: done=${w.text().includes("SLOW-END-2X")} closed=${w.closed} ${JSON.stringify(w.closeInfo)} in ${Math.round(now() - tr)}ms, total ${Math.round((now() - t0) / 1000)}s`);
    let txt = strip(w.text()); let reattached: any = null;
    if (w.closed && !txt.includes("SLOW-END-2X")) {
      // recover what we missed via output_offset
      const off = w.outBytes;
      const r = await RawWS.open(`/v1/sprites/${name}/exec/${w.info.session_id}?output_offset=${off}`);
      await until(() => r.closed || r.text().includes("SLOW-END-2X"), 120000, 20);
      reattached = { offset: off, bytes: r.outBytes, done: r.text().includes("SLOW-END-2X") };
      txt = strip(w.text() + r.text()); r.close();
    }
    const nums = txt.split("\r\n").filter((l) => /^\d+$/.test(l)).map(Number);
    let missing = 0, outOfOrder = 0, prev = 0; const seen = new Set<number>();
    for (const x of nums) { if (x !== prev + 1) outOfOrder++; prev = x; seen.add(x); }
    const missList: number[] = []; for (let i = 1; i <= n; i++) if (!seen.has(i)) { missing++; if (missList.length < 5) missList.push(i); }
    log(JSON.stringify({ pause, n, bytes: w.outBytes, lines: nums.length, unique: seen.size, missing, missList, firstLines: JSON.stringify(txt.slice(txt.indexOf("seq 1"), txt.indexOf("seq 1") + 80)), discontinuities: outOfOrder, reattached, wispdRssEndKB: wispdRss() }));
    await Bun.write(`work/slow-${pause}.json`, JSON.stringify({ samples }));
    if (!w.closed) w.close();
  },

  // ---- 3. kill with a chosen signal ----
  async kill() {
    const name = guard(rest[0]); await ensure(name);
    const variants: { label: string; q?: string; body?: string; pre?: string; ws?: any; eof?: boolean }[] = [
      { label: "default (no params)", q: "" },
      { label: "?signal=SIGHUP", q: "?signal=SIGHUP" },
      { label: "?signal=HUP", q: "?signal=HUP" },
      { label: "?signal=hup", q: "?signal=hup" },
      { label: "?signal=1", q: "?signal=1" },
      { label: "?signal=9", q: "?signal=9" },
      { label: "?signal=SIGKILL", q: "?signal=SIGKILL" },
      { label: "?signal=INT", q: "?signal=INT" },
      { label: "body {signal:SIGHUP}", q: "", body: JSON.stringify({ signal: "SIGHUP" }) },
      { label: "?signal=HUP&timeout=0", q: "?signal=HUP&timeout=0" },
      { label: "?signal=BOGUS", q: "?signal=BOGUS" },
      { label: "?signal=TERM&timeout=1s", q: "?signal=TERM&timeout=1s" },
      { label: "HUP, fg sleep running", q: "?signal=HUP&timeout=3s", pre: "sleep 1001" },
      { label: "HUP, bg job + nohup job", q: "?signal=HUP&timeout=3s", pre: "sleep 1002 & nohup sleep 1003 >/dev/null 2>&1 &" },
      { label: "HUP, shell traps HUP", q: "?signal=HUP&timeout=3s", pre: "trap '' HUP" },
      { label: "WS frame {type:signal,signal:HUP}", ws: { type: "signal", signal: "HUP" } },
      { label: "Ctrl-D (EOF) on input", eof: true },
    ];
    const results: any[] = [];
    for (const v of variants) {
      const w = await shell(name, "&max_run_after_disconnect=1m");
      const id = w.info.session_id;
      if (v.pre) { w.input(v.pre + "\n"); await sleep(500); }
      const t = now(); let http: any = null;
      if (v.ws) w.json(v.ws);
      else if (v.eof) w.input("\x04");
      else {
        const r = await api("POST", `/${name}/exec/${id}/kill`, v.body, v.q ?? "");
        http = { status: r.status, ms: Math.round(now() - t), body: r.text.trim().split("\n").map((l: string) => { try { const j = JSON.parse(l); delete j.time; return j; } catch { return l; } }) };
      }
      await until(() => w.closed, 15000);
      const wsMs = w.closed ? Math.round(now() - t) : -1;
      const left = (await run(name, "pgrep -a sleep || echo none")).out.replace(/\r?\n/g, "; ");
      const res = { label: v.label, http, exit: w.exit, close: w.closeInfo?.code, wsClosedMs: wsMs, sleepersAfter: left };
      log(JSON.stringify(res)); results.push(res);
      if (!w.closed) { await api("POST", `/${name}/exec/${id}/kill`, undefined, "?signal=KILL"); w.close(); }
      await run(name, "pkill sleep; true");
    }
    await Bun.write("work/kill.json", JSON.stringify(results, null, 1));
  },

  // ---- 5. replay alignment ----
  async replay() {
    const name = guard(rest[0]); await ensure(name);
    const att = async (id: string, q: string, wait = 1500) => {
      const r = await RawWS.open(`/v1/sprites/${name}/exec/${id}${q}`); await sleep(wait);
      const o = { bytes: r.outBytes, buf: Buffer.concat(r.out), texts: r.texts.map((t) => t.text) }; r.close(); await sleep(200); return o;
    };
    // a. small
    let w = await shell(name, "&max_run_after_disconnect=10m");
    await sh(w, "echo SMALL-$((2+2))");
    let S = Buffer.concat(w.out); let id = w.info.session_id; w.close(); await sleep(500);
    const a0 = await att(id, "");
    const a1 = await att(id, `?output_offset=${S.length}`);
    const a2 = await att(id, `?output_offset=${S.length - 10}`);
    log(JSON.stringify({ small: { live: S.length, noOffset: a0.bytes, noOffsetEqualsLive: a0.buf.equals(S), offsetAll: a1.bytes, offsetMinus10: a2.bytes, minus10Matches: a2.buf.equals(S.subarray(S.length - 10)), texts: a0.texts } }));
    // b. big (> 1 MiB), with output while detached
    w = await shell(name, "&max_run_after_disconnect=10m");
    await sh(w, "seq 1 400000 | tail -1", 30000); // warm-up, tiny
    await sh(w, "seq 1 400000", 60000);
    S = Buffer.concat(w.out); id = w.info.session_id; const D = S.length;
    w.input("sleep 2; seq 400001 500000; echo DET-$((1+1))-DONE\n"); await sleep(300);
    const live2 = Buffer.concat(w.out); w.close(); await sleep(5000);
    const b0 = await att(id, "", 3000);
    const isTail = live2.length >= 0 && b0.buf.length > 0;
    // where does replay start relative to live stream? find replay's first 64 bytes in S
    const startInS = S.indexOf(b0.buf.subarray(0, 4096));
    const b1 = await att(id, `?output_offset=${live2.length}`, 3000);
    const b1txt = strip(b1.buf.toString("latin1"));
    const b1nums = b1txt.split("\r\n").filter((l) => /^\d+$/.test(l)).map(Number);
    const b2 = await att(id, `?output_offset=1000`, 3000);
    const total = live2.length + b1.bytes;
    log(JSON.stringify({ big: { liveBytes: live2.length, D, noOffsetReplay: b0.bytes, replayStartInLive: startInS, replayStartsWith: JSON.stringify(b0.buf.subarray(0, 40).toString("latin1")),
      replayIsSuffixOfAll: isTail, offsetLive: { bytes: b1.bytes, first: b1nums[0], last: b1nums.at(-1), count: b1nums.length, endsWithDone: b1txt.includes("DET-2-DONE") },
      offset1000: { bytes: b2.bytes, expectedIfFull: total - 1000, texts: b2.texts }, total } }));
    await api("POST", `/${name}/exec/${id}/kill`, undefined, "?signal=KILL");
  },
  // capture NAME LABEL CMD...: full TTY stream of a one-shot command to work/cap-LABEL.bin
  async capture() {
    const name = guard(rest[0]); const label = rest[1]; await ensure(name);
    const w = await RawWS.open(execPath(name, ["bash", "-lc", rest.slice(2).join(" ")], "&max_run_after_disconnect=30s"));
    await until(() => w.closed, 120000);
    const b = Buffer.concat(w.out); await Bun.write(`work/cap-${label}.bin`, b); log(`${label}: ${b.length} bytes, exit ${JSON.stringify(w.exit)}`);
  },
  // align LABEL...: offline: how often is the tail of what the client has unique in a 1 MiB ring?
  async align() {
    const Ls = [8, 16, 32, 64, 128, 256, 512, 1024, 4096];
    const rows: any[] = [];
    for (const label of rest) {
      const S = Buffer.from(await Bun.file(`work/cap-${label}.bin`).arrayBuffer());
      const RING = 1 << 20; const ringStart = Math.max(0, S.length - RING); const ring = S.subarray(ringStart);
      // client got bytes [0, D); D uniformly in the ring, at least 4096 in so a 4 KiB tail fits
      let seed = 42; const rnd = () => { seed = (seed * 1103515245 + 12345) & 0x7fffffff; return seed / 0x7fffffff; };
      const trials = 200; const row: any = { label, bytes: S.length };
      for (const L of Ls) {
        let unique = 0, wrong = 0;
        for (let t = 0; t < trials; t++) {
          const D = ringStart + 4096 + Math.floor(rnd() * (ring.length - 4096));
          const needle = S.subarray(D - L, D);
          const first = ring.indexOf(needle); const second = first >= 0 ? ring.indexOf(needle, first + 1) : -1;
          if (first >= 0 && second < 0) { unique++; if (ringStart + first + L !== D) wrong++; }
        }
        row[`L${L}`] = `${(100 * unique / trials).toFixed(0)}%${wrong ? ` (${wrong} wrong)` : ""}`;
      }
      rows.push(row);
    }
    console.table(rows);
    await Bun.write("work/align.json", JSON.stringify(rows, null, 1));
  },

  // silent NAME sleep|cpu: a detached shell doing silent work for 60s. Does the sprite pause under it?
  async silent() {
    const name = guard(rest[0]); const mode = rest[1]; await ensure(name);
    const w = await shell(name, "&max_run_after_disconnect=10m");
    const work = mode === "cpu" ? "timeout 60 sh -c 'while :; do :; done'" : "sleep 60";
    w.input(`S=$(date +%s); ${work}; echo SILENT-DONE-after-$(( $(date +%s) - S ))s\n`); await sleep(500);
    const id = w.info.session_id; const got = w.outBytes; w.close();
    const ws = watchStatus(name, 1000); const t0 = now();
    await sleep(100000); ws.stop();
    const r = await RawWS.open(`/v1/sprites/${name}/exec/${id}?output_offset=${got}`);
    await until(() => r.text().includes("SILENT-DONE"), 90000, 50);
    log(JSON.stringify({ mode, transitions: ws.tr, reattachMs: Math.round(now() - t0 - 100000), out: strip(r.text()).trim().slice(-120) }));
    r.close();
  },
};
if (!cmds[cmd]) { console.error(`unknown ${cmd}; have ${Object.keys(cmds).join(" ")}`); process.exit(2); }
await cmds[cmd]();
process.exit(0);
