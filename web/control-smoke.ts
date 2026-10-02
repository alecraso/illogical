// Control end to end without a browser UI: a fake GitHub, illogical-control,
// a daemon that joins it, and a "browser" (this script, with the web
// client's own e2e code) that signs in, enrolls, approves the daemon's
// code, and reaches the daemon both directly and through the relay.
//   just control-smoke

import { spawn, type ChildProcess } from "node:child_process";
import { createServer } from "node:http";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { certBody, evaluate, joinCode, type Cert } from "./src/e2e/cert.ts";
import { generateKeys, signText, type DeviceKeys } from "./src/e2e/keys.ts";
import { E2ESocket } from "./src/e2e/channel.ts";

const CONTROL = 7791;
const GITHUB = 7792;
const DAEMON = 7793;
const base = `http://127.0.0.1:${CONTROL}`;
const target = process.env.TARGET_DIR ?? "../target/debug";
const procs: ChildProcess[] = [];
const dirs: string[] = [];
let failed = 0;
const check = (what: string, ok: boolean, detail = "") => {
  console.log(`${ok ? "ok  " : "FAIL"} ${what}${detail ? `: ${detail}` : ""}`);
  if (!ok) failed++;
};
const temp = (w: string) => {
  const d = mkdtempSync(join(tmpdir(), `illogical-smoke-${w}-`));
  dirs.push(d);
  return d;
};
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

// A fake GitHub: authorize redirects straight back; one user.
const gh = createServer((req, res) => {
  const u = new URL(req.url!, `http://127.0.0.1:${GITHUB}`);
  if (u.pathname === "/login/oauth/authorize") {
    const back = new URL(u.searchParams.get("redirect_uri")!);
    back.searchParams.set("code", "c0de");
    back.searchParams.set("state", u.searchParams.get("state")!);
    res.writeHead(302, { location: back.href }).end();
  } else if (u.pathname === "/login/oauth/access_token") {
    res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify({ access_token: "gho_test" }));
  } else if (u.pathname === "/user") {
    res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify({ id: 4242, login: "stranger" }));
  } else res.writeHead(404).end();
}).listen(GITHUB, "127.0.0.1");

async function up(url: string) {
  for (let i = 0; i < 100; i++) {
    try {
      if ((await fetch(url)).status < 500) return;
    } catch {
      // not yet
    }
    await sleep(100);
  }
  throw new Error(`${url} didn't come up`);
}

let cookie = "";
async function api<T>(path: string, body?: unknown): Promise<T> {
  const res = await fetch(base + path, {
    method: body === undefined ? "GET" : "POST",
    headers: { cookie, origin: base, ...(body === undefined ? {} : { "content-type": "application/json" }) },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const j = await res.json().catch(() => ({}));
  if (!res.ok) throw new Error(`${path}: ${res.status} ${JSON.stringify(j)}`);
  return j as T;
}

async function signIn() {
  // Follow the redirects by hand, keeping cookies.
  let url = `${base}/auth/github?next=/`;
  let jar: Record<string, string> = {};
  for (let i = 0; i < 5; i++) {
    const res = await fetch(url, { redirect: "manual", headers: { cookie: Object.entries(jar).map(([k, v]) => `${k}=${v}`).join("; ") } });
    for (const c of res.headers.getSetCookie()) {
      const [kv] = c.split(";");
      const [k, v] = kv.split("=");
      jar = { ...jar, [k]: v };
    }
    const loc = res.headers.get("location");
    if (!loc) break;
    url = new URL(loc, url).href;
  }
  cookie = `ilg_session=${jar.ilg_session}`;
}

async function cert(k: DeviceKeys, by: DeviceKeys, account: string, kind: Cert["kind"], name: string): Promise<Cert> {
  const c: Cert = { v: 1, account, device: k.id, kind, name, noise: k.noisePub, sign: k.signPub, created: Date.now(), approver: by.id, sig: "" };
  c.sig = await signText(by, certBody(c));
  return c;
}

try {
  const db = join(temp("control"), "control.db");
  procs.push(
    spawn(`${target}/illogical-control`, [
      ...["--listen", `127.0.0.1:${CONTROL}`, "--public-url", base, "--db", db],
      ...["--github-client-id", "id", "--github-client-secret", "secret"],
      ...["--github-url", `http://127.0.0.1:${GITHUB}`, "--github-api", `http://127.0.0.1:${GITHUB}`],
    ], { stdio: ["ignore", "ignore", "inherit"] }),
  );
  await up(`${base}/control.json`);

  // 1. Sign in; the first device is self-signed.
  await signIn();
  const me = await api<{ account: string; login: string }>("/api/me");
  check("signed in with (fake) GitHub", me.login === "stranger", me.account);
  const laptop = await generateKeys();
  const root = await cert(laptop, laptop, me.account, "browser", "laptop");
  const first = await api<{ approved: boolean }>("/api/devices", { cert: root });
  check("first device trusted on enrollment", first.approved);

  // 2. A phone asks; only an approval from the laptop lets it in.
  const phone = await generateKeys();
  const ask: Cert = { v: 1, account: me.account, device: phone.id, kind: "browser", name: "phone", noise: phone.noisePub, sign: phone.signPub, created: Date.now(), approver: "", sig: "" };
  const pending = await api<{ approved: boolean }>("/api/devices", { cert: ask });
  check("second device waits for approval", !pending.approved);
  const forged = await cert(phone, phone, me.account, "browser", "phone");
  const refused = await api(`/api/devices/${phone.id}/approve`, { cert: forged }).then(() => false, () => true);
  check("a self-approval is refused", refused);
  await api(`/api/devices/${phone.id}/approve`, { cert: await cert(phone, laptop, me.account, "browser", "phone") });
  const devs = await api<{ trust: { account: string; root: string }; certs: Cert[] }>("/api/devices");
  check("phone approved by the laptop", (await evaluate(devs.trust, devs.certs)).has(phone.id));

  // 3. A daemon joins with a code.
  const state = temp("daemon");
  const joining = spawn(`${target}/illogicald`, ["join", base, "--name", "box", "--state-dir", state], { stdio: ["ignore", "pipe", "inherit"] });
  procs.push(joining);
  const code = await new Promise<string>((res) => {
    let out = "";
    joining.stdout!.on("data", (d) => {
      out += d;
      const m = out.match(/#join=([A-Z0-9]{5}-[A-Z0-9]{5})/);
      if (m) res(m[1]);
    });
  });
  const shown = await api<{ cert: Cert }>(`/api/joins/${code}`);
  check("join code matches the daemon's key", (await joinCode(shown.cert)) === code, code);
  const dk = { ...shown.cert, account: me.account, approver: phone.id, sig: "" };
  dk.sig = await signText(phone, certBody(dk));
  await api(`/api/joins/${code}/approve`, { cert: dk });
  const joined = await new Promise<number>((r) => joining.on("exit", r));
  check("illogicald join finished", joined === 0);

  // 4. The daemon runs, picks up the enrollment and dials the relay.
  procs.push(
    spawn(`${target}/illogicald`, [
      ...["--listen", `127.0.0.1:${DAEMON}`, "--name", "box", "--state-dir", state],
      ...["--shell", "bash --norc --noprofile", "--no-manager-env", "--tailscale-socket", "/nonexistent"],
      ...["--direct-url", `http://127.0.0.1:${DAEMON}`],
    ], { stdio: process.env.DAEMON_LOG ? ["ignore", "inherit", "inherit"] : "ignore" }),
  );
  let dir: { daemons: { id: string; name: string; online: boolean; urls: string[] }[] } = { daemons: [] };
  for (let i = 0; i < 50 && !dir.daemons[0]?.online; i++) {
    await sleep(200);
    dir = await api("/api/directory");
  }
  check("directory lists the daemon, online", dir.daemons[0]?.online === true, JSON.stringify(dir.daemons[0]));
  const d = dir.daemons[0];
  const all = await api<{ trust: { account: string; root: string }; certs: Cert[] }>("/api/devices");
  const dcert = (await evaluate(all.trust, all.certs)).get(d.id);
  check("the daemon's certificate chains to our root", dcert?.kind === "daemon");

  // 5. Reach it through the relay (with the session cookie), then directly.
  const relayUrl = `ws://127.0.0.1:${CONTROL}/api/relay/c/${d.id}`;
  for (const [how, url, headers] of [
    ["relayed", relayUrl, { cookie, origin: base }],
    ["direct", `ws://127.0.0.1:${DAEMON}/e2e`, {}],
  ] as const) {
    const orig = globalThis.WebSocket;
    // Node's WebSocket (undici) takes headers as a second argument.
    globalThis.WebSocket = class extends orig {
      constructor(u: string | URL) {
        super(u, { headers } as unknown as string[]);
      }
    } as typeof WebSocket;
    try {
      const sock = await E2ESocket.connect([{ url, timeoutMs: 3000 }], { id: d.id, noise: dcert!.noise }, phone);
      const host = await sock.request("GET", "/api/host");
      check(`${how}: API through the channel`, host.ok, host.text().slice(0, 60));
      const texts: string[] = [];
      sock.onText = (t) => texts.push(t);
      sock.start();
      for (let i = 0; i < 30 && !texts.some((t) => t.includes('"hello"')); i++) await sleep(100);
      check(`${how}: the protocol's hello`, texts.some((t) => t.includes('"hello"')));
      sock.close();
    } finally {
      globalThis.WebSocket = orig;
    }
  }

  // 6. A device the account doesn't trust gets nowhere.
  const stranger = await generateKeys();
  const nope = await E2ESocket.connect([{ url: `ws://127.0.0.1:${DAEMON}/e2e`, timeoutMs: 3000 }], { id: d.id, noise: dcert!.noise }, stranger).then(
    () => false,
    () => true,
  );
  check("an untrusted device is refused", nope);
} catch (e) {
  console.log("FAIL", e);
  failed++;
} finally {
  for (const p of procs) p.kill("SIGKILL");
  gh.close();
  for (const d of dirs) rmSync(d, { recursive: true, force: true });
}
process.exit(failed ? 1 : 0);
