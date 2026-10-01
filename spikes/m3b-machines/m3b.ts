// M3b spike harness: ephemeral wisp sprites as VM panes.
// Extends spikes/s4-reach/probe.ts. env: SPRITES_API_URL (default local wisp), SPRITE_TOKEN.
// usage: bun m3b.ts <cmd> [args]   (every sprite name must start with illogical-m3b-)
const BASE = process.env.SPRITES_API_URL ?? "http://127.0.0.1:7788";
const TOKEN = process.env.SPRITE_TOKEN ?? "";
const WS_BASE = BASE.replace(/^http/, "ws");
const auth = { Authorization: `Bearer ${TOKEN}` };
const PREFIX = "illogical-m3b-";
const sleep = (n: number) => new Promise((r) => setTimeout(r, n));
const now = () => performance.now();
const T0 = now();
const log = (...a: unknown[]) => console.log(`[${Math.round(now() - T0)}ms]`, ...a);
const guard = (name: string) => { if (!name?.startsWith(PREFIX)) throw new Error(`refusing sprite ${name}`); return name; };
const until = async (f: () => boolean, max = 30000, step = 5) => {
  const end = Date.now() + max; while (!f() && Date.now() < end) await sleep(step); return f();
};
const med = (xs: number[]) => { const s = [...xs].sort((a, b) => a - b); const m = s.length >> 1; return s.length % 2 ? s[m] : (s[m - 1] + s[m]) / 2; };
const stats = (xs: number[]) => ({ median: +med(xs).toFixed(1), min: +Math.min(...xs).toFixed(1), max: +Math.max(...xs).toFixed(1), n: xs.length });
const strip = (s: string) => s.replace(/\x1b\[[0-9;?]*[ -\/]*[@-~]/g, "").replace(/\x1b\][^\x07\x1b]*(\x07|\x1b\\)/g, "").replace(/\x1b[()][0-9A-B]/g, "").replace(/\x1b[=>]/g, "");
const PROMPT = /[$#] $/;
const promptVisible = (out: string) => PROMPT.test(strip(out));

async function api(method: string, path: string, body?: BodyInit, query = "") {
  const headers = typeof body === "string" ? { ...auth, "Content-Type": "application/json" } : auth;
  const r = await fetch(`${BASE}/v1/sprites${path}${query}`, { method, headers, body });
  const text = await r.text();
  let j: any; try { j = JSON.parse(text); } catch { j = text; }
  return { status: r.status, body: j };
}
async function apiOk(method: string, path: string, body?: BodyInit, query = "") {
  const r = await api(method, path, body, query);
  if (r.status >= 300) throw new Error(`${method} ${path} -> ${r.status} ${JSON.stringify(r.body).slice(0, 300)}`);
  return r.body;
}
const create = (name: string) => api("POST", "", JSON.stringify({ name: guard(name) }));
const del = (name: string) => api("DELETE", `/${guard(name)}`);
const status = async (name: string) => { const r = await api("GET", `/${guard(name)}`); return r.status === 200 ? r.body.status as string : `http-${r.status}`; };
async function deleteAndConfirm(name: string) {
  const d = await del(name);
  const g = await api("GET", `/${guard(name)}`);
  log(`delete ${name}: ${d.status}; GET -> ${g.status}`);
  return g.status;
}

// Background status poller: records transitions with timestamps (relative to t0).
function watchStatus(name: string, t0: number, every = 100) {
  const tr: { ms: number; status: string }[] = []; let stop = false; let last = "";
  (async () => {
    while (!stop) {
      const st = await status(name).catch((e) => `err:${e}`);
      if (st !== last) { tr.push({ ms: Math.round(now() - t0), status: st }); last = st; }
      await sleep(every);
    }
  })();
  return { tr, stop: () => { stop = true; } };
}

type Sess = {
  ws: WebSocket; out: string; bytes: number; info: any; exit: any; closed: boolean;
  close?: { code: number; reason: string; ms: number }; frames: { ms: number; text: string }[];
  firstByte: number; infoAt: number; openedAt: number; t0: number; onBytes?: (s: string) => void;
};
function open(url: string): Promise<WebSocket> {
  return new Promise((res, rej) => {
    const ws = new WebSocket(url, { headers: auth } as any);
    ws.binaryType = "arraybuffer";
    ws.onopen = () => res(ws);
    ws.onerror = (e) => rej(new Error(`ws error ${url.replace(/\?.*/, "")}: ${(e as any).message ?? e}`));
    ws.onclose = (e) => rej(new Error(`ws closed before open: ${e.code} ${e.reason}`));
  });
}
// resize: sent right after session_info (null = don't send).
function wrap(ws: WebSocket, t0: number, resize: [number, number] | null = [120, 40]): Sess {
  const s: Sess = { ws, out: "", bytes: 0, info: null, exit: null, closed: false, frames: [], firstByte: -1, infoAt: -1, openedAt: now() - t0, t0 };
  ws.onmessage = (m) => {
    if (typeof m.data === "string") {
      s.frames.push({ ms: Math.round(now() - t0), text: m.data });
      let j: any; try { j = JSON.parse(m.data); } catch { return; }
      if (j.type === "session_info") {
        s.info = j; s.infoAt = now() - t0;
        if (resize) ws.send(JSON.stringify({ type: "resize", cols: resize[0], rows: resize[1] }));
      } else if (j.type === "exit") s.exit = j;
    } else {
      if (s.firstByte < 0) s.firstByte = now() - t0;
      const t = new TextDecoder().decode(m.data); s.out += t; s.bytes += m.data.byteLength; s.onBytes?.(t);
    }
  };
  ws.onclose = (e) => { s.closed = true; s.close = { code: e.code, reason: e.reason, ms: Math.round(now() - t0) }; };
  ws.onerror = () => {};
  return s;
}
async function tty(name: string, cmd: string[], extra = "&max_run_after_disconnect=10m", t0 = now(), resize: [number, number] | null = [120, 40]) {
  const q = cmd.map((c) => `cmd=${encodeURIComponent(c)}`).join("&");
  const ws = await open(`${WS_BASE}/v1/sprites/${guard(name)}/exec?tty=true&${q}&env=TERM=xterm-256color${extra}`);
  return wrap(ws, t0, resize);
}
async function attach(name: string, id: string | number, query = "", t0 = now(), resize: [number, number] | null = null) {
  const ws = await open(`${WS_BASE}/v1/sprites/${guard(name)}/exec/${id}${query}`);
  return wrap(ws, t0, resize);
}
const send = (ws: WebSocket, s: string) => ws.send(new TextEncoder().encode(s));
let mark = 0;
// Run a command line in an attached shell and return the text between two unique markers.
async function sh(s: Sess, line: string, max = 15000) {
  const k = `M${++mark}X${Date.now() % 100000}`;
  const from = s.out.length;
  send(s.ws, `echo ${k}-B; ${line}; echo ${k}-$((1+1))E\n`);
  const done = await until(() => s.out.slice(from).includes(`${k}-2E`), max);
  const seg = strip(s.out.slice(from));
  const b = seg.lastIndexOf(`${k}-B\r\n`);
  const e = seg.lastIndexOf(`${k}-2E`);
  return done && b >= 0 ? seg.slice(b + `${k}-B\r\n`.length, e).trim() : `TIMEOUT(${seg.slice(-200)})`;
}
const shell = (name: string, extra?: string, t0?: number, resize?: [number, number] | null) => tty(name, ["bash", "-l"], extra, t0, resize);
async function ready(s: Sess, max = 30000) { return until(() => s.info !== null && promptVisible(s.out), max); }
async function ensure(name: string) {
  const st = await status(name);
  if (st.startsWith("http-404")) { const c = await create(name); log(`create ${name}: ${c.status} ${JSON.stringify(c.body).slice(0, 400)}`); }
  else log(`${name} exists: ${st}`);
}

const [cmd, ...rest] = process.argv.slice(2);
const cmds: Record<string, () => Promise<void>> = {
  async create() { await ensure(rest[0]); },
  async delete() { await deleteAndConfirm(rest[0]); },
  async status() { log(JSON.stringify((await api("GET", `/${guard(rest[0])}`)).body)); },
  async sessions() { log(JSON.stringify((await api("GET", `/${guard(rest[0])}/exec`)).body)); },
  async run() {
    const s = await tty(rest[0], ["bash", "-lc", rest.slice(1).join(" ")], "&max_run_after_disconnect=30s");
    await until(() => s.exit !== null || s.closed, 60000);
    process.stdout.write(s.out); log("exit", s.exit, s.frames);
  },

  // Does a one-shot command's output all arrive before/after the exit frame?
  async oneshot() {
    const name = guard(rest[0]); const n = Number(rest[1] ?? 200000); const t0 = now();
    const s = await tty(name, ["bash", "-c", `seq -f 'L%09g' 1 ${n}`], "&max_run_after_disconnect=30s", t0);
    let atExit = -1; const iv = setInterval(() => { if (s.exit && atExit < 0) atExit = s.bytes; }, 1);
    await until(() => s.closed, 30000); clearInterval(iv);
    const lines = strip(s.out).split("\r\n").filter((l) => /^L\d{9}$/.test(l));
    log(JSON.stringify({ n, expect_bytes: n * 12, bytes: s.bytes, bytes_at_exit_frame: atExit, lines: lines.length, last: lines.at(-1),
      exit: s.exit, close: s.close, frames: s.frames.map((f) => f.text.slice(0, 120)) }));
  },
  // Replay on reattach: does it resend bytes this client already received?
  async replaycheck() {
    const name = guard(rest[0]);
    const s = await shell(name, "&max_run_after_disconnect=10m"); await ready(s);
    await sh(s, "echo SEEN-$((1+1))-BEFORE");
    const id = s.info.session_id, live = s.bytes; s.ws.close(); await sleep(1000);
    for (const q of ["?since=298", "?offset=298", "?replay=false", "?scrollback=0", "?history=false"]) {
      const v = await attach(name, id, q); await until(() => v.info !== null, 5000); await sleep(800);
      log(q, "replay_bytes", v.bytes, JSON.stringify(v.frames.filter((f) => !f.text.includes("session_info")).map((f) => f.text)));
      v.ws.close(); await sleep(300);
    }
    const a = await attach(name, id); await until(() => a.info !== null, 5000); await sleep(1500);
    const r = strip(a.out);
    log(JSON.stringify({ live_bytes: live, replay_bytes: a.bytes, replay_has_seen_output: r.includes("SEEN-2-BEFORE"), frames: a.frames.map((f) => f.text) }));
    send(a.ws, "exit\n"); await sleep(300); a.ws.close();
  },
  // Is max_run_after_disconnect enforced? Detach a shell with a short limit and look for it later.
  async maxrun() {
    const name = guard(rest[0]); const lim = rest[1] ?? "5s"; const wait = Number(rest[2] ?? 12000);
    const s = await shell(name, `&max_run_after_disconnect=${lim}`); await ready(s);
    const pid = await sh(s, "echo $$"); const id = s.info.session_id; s.ws.close();
    await sleep(wait);
    const w = await tty(name, ["bash", "-c", `ps -o pid,stat,tty,etime,comm -p ${pid} || echo GONE`], "&max_run_after_disconnect=10s");
    await until(() => w.exit !== null || w.closed, 10000);
    log(JSON.stringify({ limit: lim, waited_ms: wait, id, pid, ps: strip(w.out).trim(),
      listed: JSON.stringify((await api("GET", `/${name}/exec`)).body).includes(`"id":"${id}"`) }));
    try { const a = await attach(name, id); await sleep(800); log("attach:", JSON.stringify(a.info), JSON.stringify(a.frames.map((f) => f.text)), JSON.stringify(a.close)); a.ws.close(); }
    catch (e) { log("attach:", String(e)); }
  },
  // Does polling GET /exec keep a sprite awake, and does last_activity show output made while detached?
  async pollexec() {
    const name = guard(rest[0]);
    const s = await shell(name, "&max_run_after_disconnect=10m"); await ready(s);
    send(s.ws, "sleep 8; echo LATE-OUTPUT\n"); await sleep(300);
    const id = s.info.session_id; s.ws.close();
    const t0 = now(); let last = "", lastAct = "";
    while (now() - t0 < 70000) {
      const l = (await api("GET", `/${name}/exec`)).body.sessions.find((x: any) => x.id === id);
      const st = await status(name);
      if (st !== last) { log(`status ${st} at ${Math.round(now() - t0)}ms`); last = st; }
      if (l && l.last_activity !== lastAct) { log(`last_activity ${l.last_activity} at ${Math.round(now() - t0)}ms`); lastAct = l.last_activity; }
      if (st !== "running") break;
      await sleep(2000);
    }
    const a = await attach(name, id); await until(() => a.info !== null, 30000); await sleep(500);
    log("replay has LATE-OUTPUT:", strip(a.out).includes("LATE-OUTPUT\r\n"));
    send(a.ws, "exit\n"); await sleep(300); a.ws.close();
  },
  // Does exec take an initial size in the query (so no resize is needed after session_info)?
  async initsize() {
    const name = guard(rest[0]); await ensure(name);
    const s = await tty(name, ["bash", "-l"], "&max_run_after_disconnect=30s&cols=133&rows=37", now(), null); await ready(s);
    log(JSON.stringify(s.info), "stty:", await sh(s, "stty size"));
    send(s.ws, "exit\n"); await sleep(300); s.ws.close();
    await deleteAndConfirm(name);
  },
  // Q1 aside: does create alone boot the sprite? Fine-grained status, then an exec.
  async createwatch() {
    const name = `${PREFIX}cw-${Date.now() % 1000000}`; const t0 = now();
    const w = watchStatus(name, t0, 10);
    const c = await create(name); log("create", c.status, c.body.status, Math.round(now() - t0));
    await sleep(3000); log("3s after create, no exec:", JSON.stringify(w.tr));
    const t1 = now(); const s = await shell(name, "&max_run_after_disconnect=1m", t1); await ready(s);
    log(JSON.stringify({ ws_open_ms: Math.round(s.openedAt), session_info_ms: Math.round(s.infoAt), prompt_ms: Math.round(now() - t1) }));
    await sleep(200); w.stop(); log("transitions:", JSON.stringify(w.tr.map((x) => ({ ...x, ms: Math.round(x.ms - (t1 - t0)) }))));
    s.ws.close(); await deleteAndConfirm(name);
  },
  // Q1: create to first prompt, N fresh sprites.
  async firstprompt() {
    const n = Number(rest[0] ?? 5); const rows: any[] = [];
    for (let i = 1; i <= n; i++) {
      const name = `${PREFIX}fp-${Date.now() % 1000000}`;
      const t0 = now();
      const w = watchStatus(name, t0, 100);
      const c = await create(name); const tCreate = now() - t0;
      const s = await shell(name, "&max_run_after_disconnect=1m", t0);
      const tWs = now() - t0;
      await ready(s, 60000); const tPrompt = now() - t0;
      const r = await sh(s, "echo READY-$((40+2))"); const tUsable = now() - t0;
      await sleep(300); w.stop();
      const row = { name, create_status: c.status, create_body_status: c.body?.status, create_ms: +tCreate.toFixed(1), ws_open_ms: +tWs.toFixed(1),
        session_info_ms: +s.infoAt.toFixed(1), first_byte_ms: +s.firstByte.toFixed(1), prompt_ms: +tPrompt.toFixed(1),
        usable_ms: +tUsable.toFixed(1), echo_ok: r === "READY-42", transitions: w.tr };
      log(JSON.stringify(row)); rows.push(row);
      s.ws.close(); await sleep(200);
      await deleteAndConfirm(name);
      await sleep(1000);
    }
    for (const k of ["create_ms", "ws_open_ms", "session_info_ms", "first_byte_ms", "prompt_ms", "usable_ms"])
      log(k, JSON.stringify(stats(rows.map((r) => r[k]))));
  },

  // Q2: resize after reattach. Tries several attach variants on one session.
  async resize() {
    const name = guard(rest[0]); await ensure(name);
    const s = await shell(name, "&max_run_after_disconnect=10m", now(), [100, 30]);
    await ready(s);
    log("initial", JSON.stringify(s.info), "stty:", await sh(s, "stty size"), "tty:", await sh(s, "tty"));
    const id = s.info.session_id;
    s.ws.close(); await sleep(1500);
    const variants: [string, string, boolean][] = [
      ["plain", "", false],
      ["plain, resize before session_info", "", true],
      ["?tty=true&cols=140&rows=45", "?tty=true&cols=140&rows=45", false],
      ["?tty=true&stdin=true&cols=140&rows=45", "?tty=true&stdin=true&cols=140&rows=45", false],
      ["?owner=true", "?owner=true", false],
      ["?takeover=true", "?takeover=true", false],
      ["?is_owner=true", "?is_owner=true", false],
    ];
    let size = 140;
    for (const [label, q, early] of variants) {
      const cols = size++, rows = 45;
      const t0 = now();
      let a: Sess;
      try { a = await attach(name, id, q, t0, null); } catch (e) { log(label, "attach failed", String(e)); continue; }
      if (early) a.ws.send(JSON.stringify({ type: "resize", cols, rows }));
      await until(() => a.info !== null, 5000);
      if (!early) a.ws.send(JSON.stringify({ type: "resize", cols, rows }));
      await sleep(400);
      const got = await sh(a, "stty size");
      log(JSON.stringify({ variant: label, sent: `${rows} ${cols}`, stty: got, info: a.info,
        other_frames: a.frames.filter((f) => !f.text.includes("session_info")).map((f) => f.text) }));
      a.ws.close(); await sleep(800);
    }
    // Workaround: a second exec sets the size on the first one's PTY and sends SIGWINCH to its foreground group.
    const a = await attach(name, id, "", now(), null); await until(() => a.info !== null, 5000); await sleep(300);
    const pts = (await sh(a, "tty")).trim();
    const t0 = now();
    const w = await tty(name, ["bash", "-c", `stty -F ${pts} cols 150 rows 50 && pkill -WINCH -t ${pts.replace("/dev/", "")}; echo rc=$?`], "&max_run_after_disconnect=10s", t0, null);
    await until(() => w.exit !== null || w.closed, 10000);
    const tWork = now() - t0;
    log(JSON.stringify({ workaround: `stty -F ${pts} cols 150 rows 50 + pkill -WINCH`, exec_ms: +tWork.toFixed(1), out: strip(w.out).trim(), exit: w.exit,
      stty_after: await sh(a, "stty size"), tput: await sh(a, "tput cols; tput lines | tr '\\n' ' '") }));
    // Does a later owner-less resize frame now undo it? (Is the frame ignored, or applied?)
    a.ws.send(JSON.stringify({ type: "resize", cols: 90, rows: 20 })); await sleep(400);
    log("after a non-owner resize frame 90x20:", await sh(a, "stty size"));
    send(a.ws, "exit\n"); await sleep(500); a.ws.close();
  },

  // Q2b: does a foreground program get SIGWINCH from a reattached resize; does a non-owner resize while the owner is attached?
  async winch() {
    const name = guard(rest[0]); await ensure(name);
    const s = await shell(name, "&max_run_after_disconnect=10m", now(), [100, 30]); await ready(s);
    // the trap body is in double quotes inside single quotes, so \$ must reach bash unexpanded: use \\$
    send(s.ws, `bash -c 'trap "echo GOT-WINCH \\$(stty size)" WINCH; echo TRAP-ON; while :; do sleep 0.05; done'\n`);
    await until(() => s.out.includes("TRAP-ON\r\n"), 5000);
    const id = s.info.session_id;
    // second client while the owner is still attached
    const b = await attach(name, id); await until(() => b.info !== null, 5000);
    let from = s.out.length;
    b.ws.send(JSON.stringify({ type: "resize", cols: 111, rows: 33 })); await sleep(600);
    log("owner attached, non-owner sends 111x33:", JSON.stringify(b.info), "owner saw:", JSON.stringify(strip(s.out.slice(from)).trim()));
    from = s.out.length;
    s.ws.send(JSON.stringify({ type: "resize", cols: 122, rows: 44 })); await sleep(600);
    log("owner sends 122x44:", JSON.stringify(strip(s.out.slice(from)).trim()), "non-owner saw:", JSON.stringify(strip(b.out).slice(-60)));
    b.ws.close(); s.ws.close(); await sleep(1500);
    const a = await attach(name, id); await until(() => a.info !== null, 5000); await sleep(300);
    from = a.out.length;
    a.ws.send(JSON.stringify({ type: "resize", cols: 140, rows: 45 })); await sleep(600);
    log("reattached (owner gone) sends 140x45:", JSON.stringify(a.info), JSON.stringify(strip(a.out.slice(from)).trim()));
    // A second reattach while the first reattacher is attached: who wins?
    const c = await attach(name, id); await until(() => c.info !== null, 5000);
    from = a.out.length;
    c.ws.send(JSON.stringify({ type: "resize", cols: 90, rows: 20 })); await sleep(600);
    log("third client sends 90x20:", JSON.stringify(c.info), JSON.stringify(strip(a.out.slice(from)).trim()));
    // devpts visibility from another exec
    const w = await tty(name, ["bash", "-c", "ls -l /dev/pts; id; ps -eo pid,tty,user,comm | head -20"], "&max_run_after_disconnect=10s");
    await until(() => w.exit !== null || w.closed, 10000);
    log("other exec view:\n" + strip(w.out));
    send(a.ws, "\x03exit\n"); await sleep(500); a.ws.close(); c.ws.close();
  },
  // Q3a: seq over exec TTY.
  async seqexec() {
    const name = guard(rest[0]); const n = Number(rest[1] ?? 5); await ensure(name);
    const res: number[] = [], mbps: number[] = [];
    for (let i = 0; i < n; i++) {
      const s = await shell(name, "&max_run_after_disconnect=1m"); await ready(s);
      const r = await seqRun(s, (t) => send(s.ws, t)); res.push(r.ms); mbps.push(r.mbps);
      log(JSON.stringify(r)); send(s.ws, "exit\n"); await sleep(300); s.ws.close(); await sleep(500);
    }
    log("exec seq ms", JSON.stringify(stats(res)), "MB/s", JSON.stringify(stats(mbps)));
  },
  // Install s4-probe and leave it running as a sprite-env service.
  async install() { await ensure(rest[0]); await installProbe(rest[0], rest[1]); },

  // Q3b: seq over the TCP proxy to s4-probe.
  async seqproxy() {
    const name = guard(rest[0]); const n = Number(rest[1] ?? 5);
    const res: number[] = [], mbps: number[] = [];
    for (let i = 0; i < n; i++) {
      const p = await proxy(name);
      const r = await seqRun(p, (t) => send(p.ws, t)); res.push(r.ms); mbps.push(r.mbps);
      log(JSON.stringify(r)); send(p.ws, "exit\n"); await sleep(300); p.ws.close(); await sleep(500);
    }
    log("proxy seq ms", JSON.stringify(stats(res)), "MB/s", JSON.stringify(stats(mbps)));
  },

  // Q4: idle -> paused -> keystroke echo. mode: attached (keep the exec WS open) or detached.
  async pause() {
    const name = guard(rest[0]); const mode = rest[1] ?? "attached"; const n = Number(rest[2] ?? 1); await ensure(name);
    const echo: number[] = [];
    for (let i = 0; i < n; i++) {
      let s = await shell(name, "&max_run_after_disconnect=2h"); await ready(s);
      await sh(s, "true");
      const id = s.info.session_id;
      if (mode === "detached") { s.ws.close(); }
      const t0 = now(); let st = ""; let pausedAt = -1;
      while (now() - t0 < 120000) {
        st = await status(name);
        if (st !== "running") { pausedAt = now() - t0; break; }
        await sleep(1000);
      }
      log(JSON.stringify({ mode, idle_until_not_running_ms: Math.round(pausedAt), status: st, ws_closed: s.closed, close: s.close }));
      if (pausedAt < 0) { log("did not pause in 120s"); s.ws.close(); continue; }
      await sleep(3000); const before = await status(name);
      const t1 = now(); let tAttach = 0, tInfo = 0;
      if (mode === "detached" || s.closed) {
        s = await attach(name, id, "", t1, null); tAttach = now() - t1;
        await until(() => s.info !== null, 30000); tInfo = now() - t1;
      }
      const from = s.out.length; const t2 = now();
      send(s.ws, "x");
      const ok = await until(() => s.out.slice(from).includes("x"), 30000, 1);
      const tEcho = now() - t2, tTotal = now() - t1;
      echo.push(tTotal);
      log(JSON.stringify({ mode, status_before: before, attach_ms: +tAttach.toFixed(1), session_info_ms: +tInfo.toFixed(1),
        keystroke_echo_ms: +tEcho.toFixed(1), total_ms: +tTotal.toFixed(1), echoed: ok, status_after: await status(name) }));
      send(s.ws, "\x15exit\n"); await sleep(500); s.ws.close(); await sleep(1000);
    }
    log(mode, "wake-to-echo ms", JSON.stringify(stats(echo)));
  },

  // Q5: two execs on one sprite.
  async two() {
    const name = guard(rest[0]); await ensure(name);
    const a = await shell(name, "&max_run_after_disconnect=1h", now(), [100, 30]);
    const b = await shell(name, "&max_run_after_disconnect=1h", now(), [140, 45]);
    await ready(a); await ready(b);
    await sh(a, "export WHO=alpha"); await sh(b, "export WHO=bravo");
    const probe = "echo $WHO $(tty) $(stty size) pid=$$ sid=$(ps -o sid= -p $$ | tr -d ' ') ppid=$PPID parent=$(ps -o comm= -p $PPID)";
    log("A", JSON.stringify(a.info), await sh(a, probe));
    log("B", JSON.stringify(b.info), await sh(b, probe));
    log("A sees", await sh(a, "ps -eo pid,ppid,sid,tty,comm --forest | grep -v ' ps$' | tail -n 15"));
    log("sessions", JSON.stringify((await api("GET", `/${name}/exec`)).body));
    // detach both, reattach independently
    const ia = a.info.session_id, ib = b.info.session_id;
    a.ws.close(); b.ws.close(); await sleep(1000);
    let ra = await attach(name, ia); let rb = await attach(name, ib);
    await until(() => ra.info && rb.info, 5000);
    log("reattach A", JSON.stringify(ra.info), await sh(ra, probe));
    log("reattach B", JSON.stringify(rb.info), await sh(rb, probe));
    // pause both, resume both
    ra.ws.close(); rb.ws.close();
    const t0 = now(); let st = "";
    while (now() - t0 < 120000) { st = await status(name); if (st !== "running") break; await sleep(1000); }
    log(`after ${Math.round(now() - t0)}ms detached: ${st}`); await sleep(2000);
    ra = await attach(name, ia); rb = await attach(name, ib);
    await until(() => ra.info && rb.info, 30000);
    log("resumed A", await sh(ra, probe)); log("resumed B", await sh(rb, probe));
    // kill A, B must survive
    const k = await api("POST", `/${name}/exec/${ia}/kill`);
    await until(() => ra.exit !== null || ra.closed, 10000);
    log("kill A:", k.status, JSON.stringify(k.body).slice(0, 200), "A exit:", JSON.stringify(ra.exit), "A close:", JSON.stringify(ra.close));
    log("B after kill A:", await sh(rb, probe));
    log("sessions", JSON.stringify((await api("GET", `/${name}/exec`)).body));
    send(rb.ws, "exit\n"); await until(() => rb.exit !== null || rb.closed, 5000);
    log("B exit", JSON.stringify(rb.exit), JSON.stringify(rb.close));
  },

  // Q6: delete the sprite under an attached exec (and a proxy connection).
  async gone() {
    const name = guard(rest[0]); const busy = rest[1] === "busy"; await ensure(name);
    const s = await shell(name, "&max_run_after_disconnect=10m"); await ready(s);
    let p: Sess | null = null;
    if (rest[2]) await installProbe(name, rest[2]); // also hold a proxy connection to s4-probe
    try { if (rest[2]) p = await proxy(name); } catch (e) { log("proxy", String(e)); }
    if (busy) send(s.ws, "while :; do date +%T.%N; sleep 0.05; done\n");
    await sleep(1000);
    const id = s.info.session_id;
    const from = s.frames.length, fromOut = s.out.length;
    const t0 = now();
    const d = await del(name); const tDel = now() - t0;
    await until(() => s.closed && (!p || p.closed), 30000);
    log(JSON.stringify({ busy, delete_status: d.status, delete_ms: +tDel.toFixed(1),
      exec: { close: s.close && { ...s.close, ms: Math.round(s.close.ms - (t0 - s.t0)) }, frames_after: s.frames.slice(from).map((f) => ({ ...f, ms: Math.round(f.ms - (t0 - s.t0)) })), exit: s.exit, tail: JSON.stringify(strip(s.out.slice(fromOut)).slice(-160)) },
      proxy: p && { close: p.close && { ...p.close, ms: Math.round(p.close.ms - (t0 - p.t0)) }, frames: p.frames.map((f) => f.text) } }));
    try { const a = await attach(name, id); await sleep(1000); log("reattach after delete:", JSON.stringify(a.frames), JSON.stringify(a.close)); }
    catch (e) { log("reattach after delete:", String(e)); }
    const r = await fetch(`${BASE}/v1/sprites/${name}/exec/${id}`, { headers: auth });
    log("plain GET exec after delete:", r.status, (await r.text()).slice(0, 200));
    log("GET sprite:", (await api("GET", `/${name}`)).status);
  },
};

async function installProbe(name: string, path: string) {
  const bin = await Bun.file(path).arrayBuffer();
  log(JSON.stringify(await apiOk("PUT", `/${guard(name)}/fs/write`, bin, `?path=/home/sprite/s4-probe&mode=0755&mkdir=true`)));
  const s = await tty(name, ["sprite-env", "services", "create", "s4", "--cmd", "/home/sprite/s4-probe",
    "--args", "serve,127.0.0.1:7681", "--duration", "3s"], "&max_run_after_disconnect=30s");
  await until(() => s.exit !== null || s.closed, 30000);
  log("install:", JSON.stringify(strip(s.out).slice(-300)), "exit", JSON.stringify(s.exit));
}
// Proxy connection to s4-probe in the sprite, wrapped like an exec session.
async function proxy(name: string) {
  const t0 = now();
  const ws = await open(`${WS_BASE}/v1/sprites/${guard(name)}/proxy`);
  const p = wrap(ws, t0, null);
  ws.send(JSON.stringify({ host: "localhost", port: 7681 }));
  p.info = {};
  await until(() => promptVisible(p.out), 30000);
  return p;
}
// seq 1 1000000 and check every line arrived in order.
async function seqRun(s: Sess, put: (t: string) => void) {
  const from = s.out.length; const fromBytes = s.bytes;
  const t0 = now();
  put("seq 1 1000000; echo SEQ-$((40+2))-DONE\n");
  const ok = await until(() => s.out.includes("SEQ-42-DONE"), 300000, 2);
  const ms = now() - t0; const bytes = s.bytes - fromBytes;
  const seg = s.out.slice(from);
  const lines = strip(seg).split("\r\n").map((l) => l.replace(/^.*\r/, "")); let expect = 1, bad = 0;
  for (const l of lines) { if (/^\d+$/.test(l)) { if (Number(l) === expect) expect++; else bad++; } }
  return { ok, ms: +ms.toFixed(0), bytes, mbps: +(bytes / 1e6 / (ms / 1000)).toFixed(2), last_in_order: expect - 1, out_of_order: bad };
}

if (!cmds[cmd]) { console.error(`commands: ${Object.keys(cmds).join(" ")}`); process.exit(2); }
await cmds[cmd]();
process.exit(0);
