// Control end to end without a browser UI: a fake GitHub, illogical-control,
// a daemon that joins it, and a "browser" (this script, with the web
// client's own e2e code) that signs in, enrolls, approves the daemon's
// code, and reaches the daemon both directly and through the relay.
//   just control-smoke

import { spawn, type ChildProcess } from "node:child_process";
import { createServer } from "node:http";
import { createServer as createTcp, connect } from "node:net";
import { mkdtempSync, readdirSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { certBody, evaluate, joinCode, type Cert } from "./src/e2e/cert.ts";
import { generateKeys, signText, type DeviceKeys } from "./src/e2e/keys.ts";
import { E2ESocket } from "./src/e2e/channel.ts";
import { signRoster } from "./src/e2e/team.ts";
import { createHmac } from "node:crypto";
import { DatabaseSync } from "node:sqlite";

// Ports the OS hands out, so runs side by side (CI and a worktree's
// `just check` on one machine) don't collide.
async function freePort(): Promise<number> {
  const s = createTcp().listen(0, "127.0.0.1");
  await new Promise((ok) => s.once("listening", ok));
  const port = (s.address() as { port: number }).port;
  await new Promise((ok) => s.close(ok));
  return port;
}
const [CONTROL, GITHUB, DAEMON, SPY, PUSH, STRIPE, SPRITES, DAEMON2] = await Promise.all(Array.from({ length: 8 }, freePort));
const WHSEC = "whsec_smoke";
// Who the fake GitHub signs in next.
let asUser = "stranger";
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
    const id = [...asUser].reduce((h, c) => h * 31 + c.charCodeAt(0), 7);
    res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify({ id, login: asUser }));
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
  const controlLog: Buffer[] = [];
  procs.push(
    spawn(`${target}/illogical-control`, [
      ...["--listen", `127.0.0.1:${CONTROL}`, "--public-url", base, "--db", db],
      ...["--github-client-id", "id", "--github-client-secret", "secret"],
      ...["--github-url", `http://127.0.0.1:${GITHUB}`, "--github-api", `http://127.0.0.1:${GITHUB}`],
      ...["--push-host", `127.0.0.1:${PUSH}`],
      // Billing against a fake Stripe, with no free relay allowance (M22),
      // and hosted sandboxes against a fake Sprites API (M20).
      ...["--stripe-api", `http://127.0.0.1:${STRIPE}`, "--stripe-seat-price", "price_seat", "--stripe-minutes-price", "price_min"],
      ...["--relay-free-mb", "0", "--sprites-url", `http://127.0.0.1:${SPRITES}`, "--sandbox-binary", "/bin/true"],
    ], {
      stdio: ["ignore", "pipe", "pipe"],
      env: { ...process.env, RUST_LOG: "illogical_control=debug", STRIPE_SECRET_KEY: "sk_test_x", STRIPE_WEBHOOK_SECRET: WHSEC, SPRITES_TOKEN: "t" },
    }),
  );
  procs.at(-1)!.stdout!.on("data", (d: Buffer) => controlLog.push(d));
  procs.at(-1)!.stderr!.on("data", (d: Buffer) => controlLog.push(d));
  await up(`${base}/control.json`);
  // Everything a client sends to and gets from control, as on the wire.
  const wire: Buffer[] = [];
  const spy = createTcp((c) => {
    const up = connect(CONTROL, "127.0.0.1");
    c.on("data", (d) => (wire.push(d), up.write(d)));
    up.on("data", (d) => (wire.push(d), c.write(d)));
    c.on("close", () => up.destroy());
    up.on("close", () => c.destroy());
    c.on("error", () => up.destroy());
    up.on("error", () => c.destroy());
  }).listen(SPY, "127.0.0.1");

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

  // 3. A daemon joins with a code. It takes the account only if its
  // fingerprint is the one the person expects (`--account`, else it asks).
  const joinAs = async (name: string, state: string, account: string) => {
    const joining = spawn(`${target}/illogicald`, ["join", base, "--name", name, "--state-dir", state, "--account", account], { stdio: ["ignore", "pipe", "inherit"] });
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
    return new Promise<number>((r) => joining.on("exit", r));
  };
  const state = temp("daemon");
  check("illogicald join finished", (await joinAs("box", state, laptop.id)) === 0);

  // 4. The daemon runs, picks up the enrollment and dials the relay.
  procs.push(
    spawn(`${target}/illogicald`, [
      ...["--listen", `127.0.0.1:${DAEMON}`, "--name", "box", "--state-dir", state],
      ...["--shell", "bash --norc --noprofile", "--no-manager-env", "--tailscale-socket", "/nonexistent"],
      ...["--direct-url", `http://127.0.0.1:${DAEMON}`, "--no-claude-ide"],
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
  const relayUrl = `ws://127.0.0.1:${SPY}/api/relay/c/${d.id}`;
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
      if (how === "relayed") {
        // Type into a pane through the relay and read the output.
        const hello = JSON.parse(texts.find((t) => t.includes('"hello"'))!);
        const pane: number = hello.state.panes[0].id;
        let out = "";
        sock.onBinary = (b) => {
          if (b[0] === 1 || b[0] === 2) out += new TextDecoder().decode(b.subarray(13));
        };
        sock.sendText(JSON.stringify({ type: "attach", panes: [{ pane, offset: null }] }));
        const input = new TextEncoder().encode("echo SECRET-MARKER-$((6*7))\n");
        const frame = new Uint8Array(13 + input.length);
        frame[0] = 3;
        new DataView(frame.buffer).setUint32(1, pane);
        frame.set(input, 13);
        sock.sendBinary(frame);
        for (let i = 0; i < 50 && !out.includes("SECRET-MARKER-42"); i++) await sleep(100);
        check("relayed: a command's output comes back", out.includes("SECRET-MARKER-42"));
      }
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
  // 6b. Push through control (M21): the phone subscribes once (signed by
  // its device key); a pane that needs you reaches it, encrypted for it
  // alone by the daemon.
  const pushed: { headers: Record<string, string | string[] | undefined>; body: Buffer }[] = [];
  const fakePush = createServer((req, res) => {
    const chunks: Buffer[] = [];
    req.on("data", (c) => chunks.push(c));
    req.on("end", () => {
      pushed.push({ headers: req.headers, body: Buffer.concat(chunks) });
      res.writeHead(201).end();
    });
  }).listen(PUSH, "127.0.0.1");
  const subKeys = (await crypto.subtle.generateKey({ name: "ECDH", namedCurve: "P-256" }, true, ["deriveBits"])) as CryptoKeyPair;
  const uaPublic = new Uint8Array(await crypto.subtle.exportKey("raw", subKeys.publicKey));
  const authSecret = crypto.getRandomValues(new Uint8Array(16));
  const b64u = (b: Uint8Array) => Buffer.from(b).toString("base64url");
  const sub = { v: 1, account: me.account, device: phone.id, endpoint: `http://127.0.0.1:${PUSH}/push/phone`, p256dh: b64u(uaPublic), auth: b64u(authSecret), at: Date.now(), sig: "" };
  sub.sig = await signText(phone, `illogical push v1\naccount ${sub.account}\ndevice ${sub.device}\nendpoint ${sub.endpoint}\np256dh ${sub.p256dh}\nauth ${sub.auth}\nat ${sub.at}\n`);
  await api("/api/push/subscribe", { sub });
  const swapped = { ...sub, p256dh: b64u(crypto.getRandomValues(new Uint8Array(65))) };
  check("a subscription with swapped keys is refused", await api("/api/push/subscribe", { sub: swapped }).then(() => false, () => true));
  await sleep(1500); // the daemon fetches it (control nudges it)
  {
    const orig = globalThis.WebSocket;
    globalThis.WebSocket = class extends orig {
      constructor(u: string | URL) {
        super(u, { headers: { cookie, origin: base } } as unknown as string[]);
      }
    } as typeof WebSocket;
    try {
      const sock = await E2ESocket.connect([{ url: relayUrl, timeoutMs: 3000 }], { id: d.id, noise: dcert!.noise }, laptop);
      const panes = (await (await sock.request("GET", "/api/panes")).json<{ id: number }[]>());
      const r = await sock.request("POST", `/api/panes/${panes[0].id}/attention`, { state: "needs_input" });
      check("set a pane to need you", r.ok);
      sock.close();
    } finally {
      globalThis.WebSocket = orig;
    }
  }
  for (let i = 0; i < 50 && !pushed.length; i++) await sleep(100);
  check("the push service got one notification", pushed.length === 1, `${pushed.length}`);
  if (pushed[0]) {
    const p = pushed[0];
    check("aes128gcm, with control's VAPID", p.headers["content-encoding"] === "aes128gcm" && String(p.headers.authorization).startsWith("vapid t="));
    // Decrypt as the phone would (RFC 8291): only its key opens it.
    const body = new Uint8Array(p.body);
    const salt = body.subarray(0, 16);
    const idlen = body[20];
    const asPublic = body.subarray(21, 21 + idlen);
    const ct = body.subarray(21 + idlen);
    const asKey = await crypto.subtle.importKey("raw", asPublic, { name: "ECDH", namedCurve: "P-256" }, false, []);
    const shared = new Uint8Array(await crypto.subtle.deriveBits({ name: "ECDH", public: asKey }, subKeys.privateKey, 256));
    const hkdf = async (salt_: Uint8Array, ikm: Uint8Array, info: Uint8Array, len: number) => {
      const k = await crypto.subtle.importKey("raw", ikm, "HKDF", false, ["deriveBits"]);
      return new Uint8Array(await crypto.subtle.deriveBits({ name: "HKDF", hash: "SHA-256", salt: salt_, info }, k, len * 8));
    };
    const te = new TextEncoder();
    const ikm = await hkdf(authSecret, shared, new Uint8Array([...te.encode("WebPush: info\0"), ...uaPublic, ...asPublic]), 32);
    const cek = await hkdf(salt, ikm, te.encode("Content-Encoding: aes128gcm\0"), 16);
    const nonce = await hkdf(salt, ikm, te.encode("Content-Encoding: nonce\0"), 12);
    const aes = await crypto.subtle.importKey("raw", cek, "AES-GCM", false, ["decrypt"]);
    const plain = new Uint8Array(await crypto.subtle.decrypt({ name: "AES-GCM", iv: nonce }, aes, ct));
    const msg = JSON.parse(new TextDecoder().decode(plain.subarray(0, plain.lastIndexOf(2))));
    check("the phone reads it: Needs you, which pane, which daemon", msg.title === "Needs you" && msg.daemon === d.id, JSON.stringify(msg));
  }
  fakePush.close();

  // 7. Control never saw it: not on the wire, not in its database, not in
  // its logs.
  await sleep(500);
  const onWire = Buffer.concat(wire).toString("latin1");
  check("the relay leg carried traffic", onWire.length > 2000, `${onWire.length} bytes`);
  check("no terminal content on control's wire", !onWire.includes("SECRET-MARKER"));
  const dbDir = db.slice(0, db.lastIndexOf("/"));
  const stored = readdirSync(dbDir).map((f) => readFileSync(join(dbDir, f)).toString("latin1")).join("");
  check("no terminal content in control's database", stored.length > 0 && !stored.includes("SECRET-MARKER"));
  const logs = Buffer.concat(controlLog).toString();
  check("no terminal content in control's logs", logs.length > 0 && !logs.includes("SECRET-MARKER"), `${logs.length} bytes of log`);
  check("no notification text in control's logs", logs.includes("push relayed") && !logs.includes("Needs you"));
  // #94: the self-approval refused in 2 is logged, with why and whose,
  // and not its signature.
  const refusal = logs.split("\n").find((l) => l.includes("refused") && l.includes(phone.id)) ?? "";
  check("a refused approval is logged with its reason and ids", refusal.includes("isn't a device this account trusts") && refusal.includes(me.account), refusal);
  check("but not its signature", !logs.includes(forged.sig));
  spy.close();

  // 8. Billing (M22). The free account used the relay past its allowance
  // (none, here): it's told.
  const bill = await api<{ relay: { warning: boolean; slowed: boolean; bytes: number } }>("/api/billing");
  check("a free account over its relay allowance sees the warning", bill.relay.warning && bill.relay.bytes > 0, JSON.stringify(bill.relay));
  // A fake Stripe: records what control asks for.
  const stripeCalls: { path: string; form: URLSearchParams }[] = [];
  const fakeStripe = createServer((req, res) => {
    let body = "";
    req.on("data", (d) => (body += d));
    req.on("end", () => {
      const path = req.url!.split("?")[0];
      stripeCalls.push({ path, form: new URLSearchParams(body) });
      const reply = (v: unknown) => res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify(v));
      if (path === "/v1/customers") reply({ id: "cus_1" });
      else if (path === "/v1/checkout/sessions") reply({ id: "cs_1", url: "https://checkout.stripe.test/cs_1" });
      else if (path === "/v1/subscriptions/sub_1")
        reply({ id: "sub_1", items: { data: [{ id: "si_seat", price: { id: "price_seat" } }, { id: "si_min", price: { id: "price_min" } }] } });
      else reply({});
    });
  }).listen(STRIPE, "127.0.0.1");
  // A fake Sprites API that can't make anything (a sandbox still counts
  // from when it's asked for until it's deleted).
  const fakeSprites = createServer((req, res) => res.writeHead(req.method === "DELETE" ? 204 : 500).end()).listen(SPRITES, "127.0.0.1");
  // The team: the laptop's account owns it.
  const team = "0123456789abcdef";
  const v1 = await signRoster(
    { v: 1, team, name: "Acme", version: 1, at: Date.now(), members: [{ account: me.account, root: laptop.id, role: "owner", name: "stranger" }] },
    laptop,
  );
  await api("/api/teams", { roster: v1 });
  // Hosted VMs need a paid plan once billing is on.
  check("hosted VMs need a paid plan", await api("/api/sandboxes", { device: laptop.id }).then(() => false, (e: Error) => /paid plan/.test(e.message)));
  const co = await api<{ url: string }>("/api/billing/checkout", { team });
  const cs = stripeCalls.find((c) => c.path === "/v1/checkout/sessions")!.form;
  check("checkout for the team: 1 seat, and metered minutes", co.url.includes("checkout") && cs.get("line_items[0][quantity]") === "1" && cs.get("line_items[1][price]") === "price_min");
  // Stripe says it's done (a signed webhook).
  const hook = async (event: unknown) => {
    const body = JSON.stringify(event);
    const t = Math.floor(Date.now() / 1000);
    const sig = createHmac("sha256", WHSEC).update(`${t}.${body}`).digest("hex");
    return fetch(`${base}/api/stripe/webhook`, { method: "POST", headers: { "stripe-signature": `t=${t},v1=${sig}` }, body });
  };
  const unsigned = await fetch(`${base}/api/stripe/webhook`, { method: "POST", headers: { "stripe-signature": "t=1,v1=00" }, body: "{}" });
  check("an unsigned webhook is refused", unsigned.status === 400);
  const done = await hook({ type: "checkout.session.completed", data: { object: { customer: "cus_1", subscription: "sub_1", metadata: { owner: `team:${team}` } } } });
  check("the team upgraded", done.ok && (await api<{ teams: { plan: string }[] }>("/api/billing")).teams[0].plan === "team");
  // A second person joins (by an invite they accept): the seats follow
  // the roster.
  const invite = await api<{ code: string }>(`/api/teams/${team}/invites`, { role: "editor" });
  const laptopCookie = cookie;
  asUser = "colleague";
  await signIn();
  const them = await api<{ account: string }>("/api/me");
  const theirs = await generateKeys();
  await api("/api/devices", { cert: await cert(theirs, theirs, them.account, "browser", "their laptop") });
  await api(`/api/invites/${team}/${invite.code}/accept`, {});
  cookie = laptopCookie;
  const v2 = await signRoster(
    { ...v1, version: 2, at: Date.now(), members: [...v1.members, { account: them.account, root: theirs.id, role: "editor", name: "colleague" }] },
    laptop,
  );
  await api(`/api/teams/${team}/roster`, { roster: v2 });
  const seats = stripeCalls.filter((c) => c.path === "/v1/subscription_items/si_seat").at(-1)?.form.get("quantity");
  check("adding a member adds a seat", seats === "2", `${seats}`);
  // Sandbox minutes, now on the team's plan: one asked for and deleted.
  const sbx = await api<{ id: string }>("/api/sandboxes", { device: laptop.id });
  await sleep(500);
  await fetch(`${base}/api/sandboxes/${sbx.id}`, { method: "DELETE", headers: { cookie, origin: base } });
  await api("/api/billing/report", {});
  const meter = stripeCalls.filter((c) => c.path === "/v1/billing/meter_events");
  const minutes = meter.reduce((n, c) => n + Number(c.form.get("payload[value]")), 0);
  check("sandbox minutes reported to Stripe", minutes === 1 && meter[0]?.form.get("payload[stripe_customer_id]") === "cus_1", `${minutes}`);
  // What Stripe would invoice at $8 a seat and 1¢ a minute.
  const invoice = Number(seats) * 800 + minutes * 1;
  check("the invoice adds up: 2 seats and 1 minute", invoice === 1601, `${invoice}¢`);
  fakeStripe.close();
  fakeSprites.close();

  // 9. A machine expecting another account (as if control swapped in one
  // of its own) doesn't take the approval, and pins nothing.
  cookie = laptopCookie;
  const elsewhere = temp("elsewhere");
  check("a join into an account other than the one expected is refused", (await joinAs("elsewhere", elsewhere, "0123-4567-89ab-cdef")) !== 0);
  check("... and pins nothing", !readdirSync(elsewhere).includes("control.json"));

  // 10. Deleting an account (#173): someone with a machine leaves. The
  // machine is refused from then on, and nothing of theirs is left.
  asUser = "leaver";
  await signIn();
  const leaver = await api<{ account: string }>("/api/me");
  const lk = await generateKeys();
  await api("/api/devices", { cert: await cert(lk, lk, leaver.account, "browser", "laptop") });
  const state2 = temp("daemon2");
  const joining2 = spawn(`${target}/illogicald`, ["join", base, "--name", "leaving-box", "--state-dir", state2, "--account", lk.id], { stdio: ["ignore", "pipe", "inherit"] });
  procs.push(joining2);
  const code2 = await new Promise<string>((res) => {
    let out = "";
    joining2.stdout!.on("data", (d) => {
      out += d;
      const m = out.match(/#join=([A-Z0-9]{5}-[A-Z0-9]{5})/);
      if (m) res(m[1]);
    });
  });
  const shown2 = await api<{ cert: Cert }>(`/api/joins/${code2}`);
  const dk2 = { ...shown2.cert, account: leaver.account, approver: lk.id, sig: "" };
  dk2.sig = await signText(lk, certBody(dk2));
  await api(`/api/joins/${code2}/approve`, { cert: dk2 });
  await new Promise((r) => joining2.on("exit", r));
  const daemon2Log: Buffer[] = [];
  const daemon2 = spawn(`${target}/illogicald`, [
    ...["--listen", `127.0.0.1:${DAEMON2}`, "--name", "leaving-box", "--state-dir", state2],
    ...["--shell", "bash --norc --noprofile", "--no-manager-env", "--tailscale-socket", "/nonexistent", "--no-claude-ide"],
  ], { stdio: ["ignore", "pipe", "pipe"], env: { ...process.env, RUST_LOG: "info" } });
  procs.push(daemon2);
  daemon2.stdout!.on("data", (d: Buffer) => daemon2Log.push(d));
  daemon2.stderr!.on("data", (d: Buffer) => daemon2Log.push(d));
  let dir2: typeof dir = { daemons: [] };
  for (let i = 0; i < 50 && !dir2.daemons[0]?.online; i++) {
    await sleep(200);
    dir2 = await api("/api/directory");
  }
  check("the leaver's machine is online", dir2.daemons[0]?.online === true);
  const sessions = await api<{ sessions: { id: string; current: boolean; agent: string }[] }>("/api/me/sessions");
  check("their sessions list this one, with what signed in", sessions.sessions.some((x) => x.current && x.agent.length > 0), JSON.stringify(sessions));
  const preview = await api<{ confirm: string; blockers: string[]; machines: number }>("/api/me/delete");
  check("deleting asks for the login, and nothing stands in the way", preview.confirm === "leaver" && preview.blockers.length === 0 && preview.machines === 1, JSON.stringify(preview));
  check("the wrong word is refused", await api("/api/me/delete", { confirm: "stranger" }).then(() => false, () => true));
  const before = Buffer.concat(daemon2Log).length;
  await api("/api/me/delete", { confirm: "leaver" });
  check("signed out: the account is gone", await api("/api/me").then(() => false, (e: Error) => / 401 /.test(e.message)));
  // Hung up on, and refused when it dials again.
  const since = () => Buffer.concat(daemon2Log).subarray(before).toString().replace(/\x1b\[[0-9;]*m/g, "");
  const refused2 = /can't reach control's relay.*401.*not an enrolled daemon/;
  for (let i = 0; i < 100 && !refused2.test(since()); i++) await sleep(100);
  check("its machine is refused from then on", refused2.test(since()), since().split("\n").filter((l) => /relay/.test(l)).slice(-1)[0]);
  const rows = new DatabaseSync(db, { readOnly: true });
  const left: string[] = [];
  for (const { name } of rows.prepare("SELECT name FROM sqlite_master WHERE type = 'table'").all() as { name: string }[]) {
    for (const { name: col } of rows.prepare(`PRAGMA table_info(${name})`).all() as { name: string }[]) {
      const n = rows.prepare(`SELECT COUNT(*) AS n FROM ${name} WHERE CAST(${col} AS TEXT) LIKE ?`).get(`%${leaver.account}%`) as { n: number };
      if (n.n) left.push(`${name}.${col}`);
    }
  }
  rows.close();
  check("no row in control's database mentions the account", left.length === 0, left.join(", "));
} catch (e) {
  console.log("FAIL", e);
  failed++;
} finally {
  for (const p of procs) p.kill("SIGKILL");
  gh.close();
  for (const d of dirs) rmSync(d, { recursive: true, force: true });
}
process.exit(failed ? 1 : 0);
