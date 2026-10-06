// Control end to end without a browser UI: a fake GitHub, illogical-control,
// a daemon that joins it, and "browsers" (fixtures/device.ts, the web
// client's own e2e code without a page) that sign in, enroll, approve the
// daemon's code, and reach the daemon both directly and through the relay.
//   just control-smoke

import { execFileSync, spawn, type ChildProcess } from "node:child_process";
import { createServer } from "node:http";
import { createServer as createTcp, connect } from "node:net";
import { mkdtempSync, readdirSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { joinCode } from "./src/e2e/cert.ts";
import { generateKeys, signText } from "./src/e2e/keys.ts";
import { E2ESocket } from "./src/e2e/channel.ts";
import { signRoster } from "./src/e2e/team.ts";
import { Device } from "./fixtures/device.ts";
import { fakeGithub, fakePush, fakeStripe } from "./fixtures/fakes.ts";
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
const [CONTROL, GITHUB, DAEMON, SPY, PUSH, STRIPE, SPRITES, DAEMON2, DAEMON3] = await Promise.all(Array.from({ length: 9 }, freePort));
const WHSEC = "whsec_smoke";
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

// Every process the smoke started, however deep. A daemon's panes run in
// sessions of their own on purpose, so a process group would miss them:
// find them by the temp dirs on their command lines (the daemon, and each
// pane's shim), then walk down to their children. Taken before anything
// dies, since orphans move to init.
function smokeProcesses(): number[] {
  const all = new Map<number, { ppid: number; command: string }>();
  for (const line of execFileSync("ps", ["-axo", "pid=,ppid=,command="], { encoding: "utf8" }).split("\n")) {
    const m = line.trim().match(/^(\d+)\s+(\d+)\s+(.*)$/);
    if (m) all.set(Number(m[1]), { ppid: Number(m[2]), command: m[3] });
  }
  const found = [...all].filter(([, p]) => dirs.some((d) => p.command.includes(d))).map(([pid]) => pid);
  for (const pid of found) for (const [c, p] of all) if (p.ppid === pid && !found.includes(c)) found.push(c);
  return found.filter((pid) => pid !== process.pid);
}

const alive = (pid: number) => {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
};

// Kill it all and wait for it to be gone, so nothing is still writing when
// the dirs are deleted. The waits are bounded.
async function cleanup() {
  const pids = smokeProcesses();
  const exits = procs.filter((p) => p.exitCode === null && p.signalCode === null).map((p) => new Promise((r) => p.once("exit", r)));
  for (const p of procs) p.kill("SIGKILL");
  for (const pid of pids) {
    try {
      process.kill(pid, "SIGKILL");
    } catch {
      // gone already
    }
  }
  await Promise.race([Promise.all(exits), sleep(5000)]);
  for (let i = 0; i < 50 && pids.some(alive); i++) await sleep(100);
}

// A fake GitHub: authorize redirects straight back, signing in whoever
// the device asks for.
const gh = fakeGithub(GITHUB);

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
  const me = await Device.signIn({ control: base, login: "stranger", name: "laptop" });
  check("signed in with (fake) GitHub", me.login === "stranger", me.account);
  check("first device trusted on enrollment", me.approved);
  const laptop = me.keys;

  // 2. A phone asks; only an approval from the laptop lets it in. It talks
  // to control through the spy, so the relay leg below is on the wire.
  const phoneDev = await Device.signIn({ control: base, via: { [base]: `http://127.0.0.1:${SPY}` }, login: "stranger", name: "phone" });
  const phone = phoneDev.keys;
  check("second device waits for approval", !phoneDev.approved);
  const forged = await phoneDev.sign(phone, "browser", "phone");
  const refused = await me.api(`/api/devices/${phone.id}/approve`, { cert: forged }).then(() => false, () => true);
  check("a self-approval is refused", refused);
  await me.approveDevice(phoneDev);
  check("phone approved by the laptop", (await me.trusted()).has(phone.id));

  // 3. A daemon joins with a code. It takes the account only if its
  // fingerprint is the one the person expects (`--account`, else it asks).
  // The phone approves it.
  const joinAs = async (name: string, state: string, account: string, by: Device = phoneDev) => {
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
    const approved = await by.approveJoin(code);
    check("join code matches the daemon's key", (await joinCode(approved)) === code, code);
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
  const d = await me.waitOnline("box", 10_000).catch(() => undefined);
  check("directory lists the daemon, online", d?.online === true, JSON.stringify(d));
  const dcert = (await me.trusted()).get(d!.id);
  check("the daemon's certificate chains to our root", dcert?.kind === "daemon");

  // 5. Reach it through the relay (with the session cookie), then directly.
  for (const [how, direct] of [
    ["relayed", undefined],
    ["direct", `ws://127.0.0.1:${DAEMON}/e2e`],
  ] as const) {
    const sock = await phoneDev.connect(d!.id, direct, 3000);
    const host = await sock.request("GET", "/api/host");
    check(`${how}: API through the channel`, host.ok, host.text().slice(0, 60));
    const texts: string[] = [];
    sock.onText = (t) => texts.push(t);
    sock.start();
    for (let i = 0; i < 30 && !texts.some((t) => t.includes('"hello"')); i++) await sleep(100);
    check(`${how}: the protocol's hello`, texts.some((t) => t.includes('"hello"')));
    sock.close();
  }
  // Type into a pane through the relay and read the output.
  const echoed = await phoneDev.roundTrip(d!.id, "SECRET-MARKER", { timeoutMs: 5000 }).catch((e: Error) => e.message);
  check("relayed: a command's output comes back", echoed.includes("SECRET-MARKER-42"));

  // 5b. The CLI (M49): `illogical login` shows a code, the laptop approves
  // it, and with no daemon of its own (so nothing in any hosts.json) it
  // reaches the account's machines by name: "box" straight at its URL, and
  // "box2", which lists none, through the relay.
  {
    const home = temp("cli");
    const env = { ...process.env, HOME: home, XDG_CONFIG_HOME: join(home, ".config"), XDG_STATE_HOME: join(home, ".state"), ILLOGICAL_SOCK: join(home, "no-daemon.sock"), ILLOGICAL_VERBOSE: "1" };
    const cli = (args: string[]) =>
      new Promise<{ code: number; out: string; err: string }>((res) => {
        const p = spawn(`${target}/illogical`, args, { env, stdio: ["ignore", "pipe", "pipe"] });
        procs.push(p);
        let out = "";
        let err = "";
        p.stdout!.on("data", (d) => (out += d));
        p.stderr!.on("data", (d) => (err += d));
        p.on("exit", (code) => res({ code: code ?? 1, out, err }));
      });
    const state2b = temp("daemon-box2");
    check("illogicald join finished (box2)", (await joinAs("box2", state2b, laptop.id)) === 0);
    procs.push(
      spawn(`${target}/illogicald`, [
        ...["--listen", "127.0.0.1:0", "--name", "box2", "--state-dir", state2b],
        ...["--shell", "bash --norc --noprofile", "--no-manager-env", "--tailscale-socket", "/nonexistent", "--no-claude-ide"],
      ], { stdio: "ignore" }),
    );
    await me.waitOnline("box2", 10_000).catch(() => undefined);
    const before = await cli(["--host", "box", "ls"]);
    check("before logging in, the CLI can't find box and says how to", before.code !== 0 && before.err.includes("illogical login"), before.err.trim());
    const login = spawn(`${target}/illogical`, ["login", base, "--name", "smoke cli", "--account", laptop.id], { env, stdio: ["ignore", "pipe", "inherit"] });
    procs.push(login);
    let said = "";
    const cliCode = await new Promise<string>((res) => {
      login.stdout!.on("data", (d) => {
        said += d;
        const m = said.match(/#join=([A-Z0-9]{5}-[A-Z0-9]{5})/);
        if (m) res(m[1]);
      });
    });
    const approvedCli = await me.approveJoin(cliCode);
    check("the CLI's code is its key's, and it's a cli device", approvedCli.kind === "cli" && (await joinCode(approvedCli)) === cliCode, cliCode);
    const loginExit = await new Promise<number>((r) => login.on("exit", (c) => r(c ?? 1)));
    check("illogical login finished", loginExit === 0 && said.includes("Logged in."), said.split("\n").slice(-3).join(" "));
    check("the CLI is one of the account's devices", (await me.trusted()).get(approvedCli.device)?.kind === "cli");
    const hosts = await cli(["hosts"]);
    check("illogical hosts lists control's machines, marked", /box\s.*direct.*\(control: /.test(hosts.out) && /box2\s.*relayed.*\(control: /.test(hosts.out), hosts.out.trim());
    for (const [name, how] of [
      ["box", "direct"],
      ["box2", "relayed"],
    ] as const) {
      const run = await cli(["--host", name, "--json", "run", "--", `echo M49-${name}-$((6*7))`]);
      const pane = run.code === 0 ? (JSON.parse(run.out) as { pane: number }).pane : undefined;
      check(`--host ${name} run: ${how}`, pane !== undefined && run.err.includes(`${name}: ${how}`), (run.err + run.out).trim().slice(0, 200));
      const ls = await cli(["--host", name, "ls"]);
      check(`--host ${name} ls`, ls.code === 0 && ls.out.includes(`%${pane}`), (ls.err + ls.out).trim().slice(0, 200));
      let cap = { code: 1, out: "", err: "" };
      for (let i = 0; i < 30 && !cap.out.includes(`M49-${name}-42`); i++) {
        cap = await cli(["--host", name, "capture", `${pane}`]);
        await sleep(200);
      }
      check(`--host ${name} capture`, cap.out.includes(`M49-${name}-42`), (cap.err + cap.out).trim().slice(-200));
      // Streams (#254): an answer with no end comes back as it's written.
      const follow = (args: string[]) => {
        const p = spawn(`${target}/illogical`, ["--host", name, ...args], { env, stdio: ["pipe", "pipe", "pipe"] });
        procs.push(p);
        const got = { out: "", err: "", code: null as number | null };
        p.stdout!.on("data", (d) => (got.out += d));
        p.stderr!.on("data", (d) => (got.err += d));
        p.on("exit", (c) => (got.code = c ?? 1));
        return { p, got };
      };
      const until = async (ok: () => boolean, ms = 15_000) => {
        for (let t = 0; t < ms && !ok(); t += 100) await sleep(100);
        return ok();
      };
      const ev = follow(["events", "--follow", "--type", "bell"]);
      await until(() => ev.got.err.includes(`${name}: `));
      await sleep(500);
      await cli(["--host", name, "run", "--", "sleep 1; printf '\\a'"]);
      check(`--host ${name} events --follow streams`, (await until(() => ev.got.out.includes('"bell"'))) && ev.got.code === null, (ev.got.err + ev.got.out).trim().slice(-200));
      ev.p.kill();
      const counting = await cli(["--host", name, "--json", "run", "--", `for i in 1 2 3; do sleep 1; echo TAIL-${name}-$((40+i)); done; sleep 30`]);
      const tl = follow(["tail", "--follow", `${JSON.parse(counting.out).pane}`]);
      check(`--host ${name} tail --follow streams`, (await until(() => tl.got.out.includes(`TAIL-${name}-43`))) && tl.got.code === null, (tl.got.err + tl.got.out).trim().slice(-200));
      tl.p.kill();
      // attach: the channel's own protocol messages, as /ws.
      const at = follow(["attach", `${pane}`]);
      await until(() => at.got.err.includes(`${name}: `));
      await sleep(1000);
      // The pane's command ended: Enter gives it a shell.
      at.p.stdin!.write("\r");
      await sleep(1500);
      at.p.stdin!.write(`echo ATTACH-${name}-$((6*7))\r`);
      const typed = await until(() => at.got.out.includes(`ATTACH-${name}-42`));
      at.p.stdin!.write("\x1d");
      const detached = await until(() => at.got.code !== null, 5000);
      check(`--host ${name} attach: typed and seen, then detached`, typed && detached && at.got.code === 0, (at.got.err + at.got.out).trim().slice(-200));
      at.p.kill();
    }
  }

  // 6. A device the account doesn't trust gets nowhere.
  const stranger = await generateKeys();
  const nope = await E2ESocket.connect([{ url: `ws://127.0.0.1:${DAEMON}/e2e`, timeoutMs: 3000 }], { id: d!.id, noise: dcert!.noise }, stranger).then(
    () => false,
    () => true,
  );
  check("an untrusted device is refused", nope);
  // 6b. Push through control (M21): the phone subscribes once (signed by
  // its device key); a pane that needs you reaches it, encrypted for it
  // alone by the daemon.
  const { server: pushServer, pushed } = fakePush(PUSH);
  const subKeys = (await crypto.subtle.generateKey({ name: "ECDH", namedCurve: "P-256" }, true, ["deriveBits"])) as CryptoKeyPair;
  const uaPublic = new Uint8Array(await crypto.subtle.exportKey("raw", subKeys.publicKey));
  const authSecret = crypto.getRandomValues(new Uint8Array(16));
  const b64u = (b: Uint8Array) => Buffer.from(b).toString("base64url");
  const sub = { v: 1, account: me.account, device: phone.id, endpoint: `http://127.0.0.1:${PUSH}/push/phone`, p256dh: b64u(uaPublic), auth: b64u(authSecret), at: Date.now(), sig: "" };
  sub.sig = await signText(phone, `illogical push v1\naccount ${sub.account}\ndevice ${sub.device}\nendpoint ${sub.endpoint}\np256dh ${sub.p256dh}\nauth ${sub.auth}\nat ${sub.at}\n`);
  await phoneDev.api("/api/push/subscribe", { sub });
  const swapped = { ...sub, p256dh: b64u(crypto.getRandomValues(new Uint8Array(65))) };
  check("a subscription with swapped keys is refused", await phoneDev.api("/api/push/subscribe", { sub: swapped }).then(() => false, () => true));
  await sleep(1500); // the daemon fetches it (control nudges it)
  {
    const sock = await me.connect(d!.id, undefined, 3000);
    const panes = (await (await sock.request("GET", "/api/panes")).json<{ id: number }[]>());
    const r = await sock.request("POST", `/api/panes/${panes[0].id}/attention`, { state: "needs_input" });
    check("set a pane to need you", r.ok);
    sock.close();
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
    check("the phone reads it: Needs you, which pane, which daemon", msg.title === "Needs you" && msg.daemon === d!.id, JSON.stringify(msg));
  }
  pushServer.close();

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
  const bill = await me.api<{ relay: { warning: boolean; slowed: boolean; bytes: number } }>("/api/billing");
  check("a free account over its relay allowance sees the warning", bill.relay.warning && bill.relay.bytes > 0, JSON.stringify(bill.relay));
  // A fake Stripe: records what control asks for.
  const { server: stripeServer, calls: stripeCalls } = fakeStripe(STRIPE);
  // A fake Sprites API that can't make anything (a sandbox still counts
  // from when it's asked for until it's deleted).
  const fakeSprites = createServer((req, res) => res.writeHead(req.method === "DELETE" ? 204 : 500).end()).listen(SPRITES, "127.0.0.1");
  // The team: the laptop's account owns it.
  const team = "0123456789abcdef";
  const v1 = await signRoster(
    { v: 1, team, name: "Acme", version: 1, at: Date.now(), members: [{ account: me.account, root: laptop.id, role: "owner", name: "stranger" }] },
    laptop,
  );
  await me.api("/api/teams", { roster: v1 });
  // Hosted VMs need a paid plan once billing is on.
  check("hosted VMs need a paid plan", await me.api("/api/sandboxes", { device: laptop.id }).then(() => false, (e: Error) => /paid plan/.test(e.message)));
  const co = await me.api<{ url: string }>("/api/billing/checkout", { team });
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
  check("the team upgraded", done.ok && (await me.api<{ teams: { plan: string }[] }>("/api/billing")).teams[0].plan === "team");
  // A second person joins (by an invite they accept): the seats follow
  // the roster.
  const invite = await me.api<{ code: string }>(`/api/teams/${team}/invites`, { role: "editor" });
  const them = await Device.signIn({ control: base, login: "colleague", name: "their laptop" });
  const theirs = them.keys;
  await them.api(`/api/invites/${team}/${invite.code}/accept`, {});
  const v2 = await signRoster(
    { ...v1, version: 2, at: Date.now(), members: [...v1.members, { account: them.account, root: theirs.id, role: "editor", name: "colleague" }] },
    laptop,
  );
  await me.api(`/api/teams/${team}/roster`, { roster: v2 });
  const seats = stripeCalls.filter((c) => c.path === "/v1/subscription_items/si_seat").at(-1)?.form.get("quantity");
  check("adding a member adds a seat", seats === "2", `${seats}`);
  // Sandbox minutes, now on the team's plan: one asked for and deleted.
  const sbx = await me.api<{ id: string }>("/api/sandboxes", { device: laptop.id });
  await sleep(500);
  await me.api(`/api/sandboxes/${sbx.id}`, undefined, "DELETE").catch(() => {});
  await me.api("/api/billing/report", {});
  const meter = stripeCalls.filter((c) => c.path === "/v1/billing/meter_events");
  const minutes = meter.reduce((n, c) => n + Number(c.form.get("payload[value]")), 0);
  check("sandbox minutes reported to Stripe", minutes === 1 && meter[0]?.form.get("payload[stripe_customer_id]") === "cus_1", `${minutes}`);
  // What Stripe would invoice at $8 a seat and 1¢ a minute.
  const invoice = Number(seats) * 800 + minutes * 1;
  check("the invoice adds up: 2 seats and 1 minute", invoice === 1601, `${invoice}¢`);
  stripeServer.close();
  fakeSprites.close();

  // 9. A machine expecting another account (as if control swapped in one
  // of its own) doesn't take the approval, and pins nothing.
  const elsewhere = temp("elsewhere");
  check("a join into an account other than the one expected is refused", (await joinAs("elsewhere", elsewhere, "0123-4567-89ab-cdef")) !== 0);
  check("... and pins nothing", !readdirSync(elsewhere).includes("control.json"));

  // 9b. A machine removed from a browser (#330): its key never counts
  // again. The daemon hears so, sets the key aside, makes a new one and
  // asks to join again; /api/setup shows why, with the new code. Approved
  // into the same account, it's back without anyone checking a
  // fingerprint again.
  {
    const st = temp("removed");
    check("illogicald join finished (removable)", (await joinAs("removable", st, laptop.id)) === 0);
    procs.push(
      spawn(`${target}/illogicald`, [
        ...["--listen", `127.0.0.1:${DAEMON3}`, "--name", "removable", "--state-dir", st],
        ...["--shell", "bash --norc --noprofile", "--no-manager-env", "--tailscale-socket", "/nonexistent", "--no-claude-ide"],
      ], { stdio: "ignore" }),
    );
    const was = await me.waitOnline("removable", 10_000);
    await me.revoke(was.id);
    const token = readFileSync(join(st, "local-token"), "utf8").trim();
    type Setup = { control: { pending?: { code: string }; removed?: { said: string; by?: string; old_key: string; new_key: string; kept: string } } };
    let setup: Setup | undefined;
    for (let i = 0; i < 150 && !setup?.control.pending; i++) {
      await sleep(200);
      setup = await fetch(`http://127.0.0.1:${DAEMON3}/api/setup?part=control`, { headers: { authorization: `Bearer ${token}` } }).then((r) => r.json() as Promise<Setup>, () => undefined);
    }
    const removed = setup?.control.removed;
    check("removed from a browser, the daemon asks to join again with a new key", !!setup?.control.pending && !!removed, JSON.stringify(setup));
    check("... and says why: removed, by which device", !!removed?.said.includes("removed") && removed?.by === "laptop", removed?.said);
    check("... keeping the old key aside", readdirSync(st).some((f) => f.startsWith("daemon.key.removed-")) && removed!.old_key !== removed!.new_key);
    const again = await me.approveJoin(setup!.control.pending!.code);
    check("the new code is for a new key", again.device !== was.id);
    const back = await me.waitOnline(again.device, 15_000).catch(() => undefined);
    check("approved once, it's back online", back?.online === true, JSON.stringify(back));
    const listed = (await me.directory()).filter((d) => d.name === "removable");
    check("listed once, with the new key", listed.length === 1 && listed[0].id === again.device, JSON.stringify(listed));
    check("the old key stays out", !(await me.trusted()).has(was.id));
  }

  // 10. Deleting an account (#173): someone with a machine leaves. The
  // machine is refused from then on, and nothing of theirs is left.
  const leaver = await Device.signIn({ control: base, login: "leaver", name: "laptop" });
  const lk = leaver.keys;
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
  await leaver.approveJoin(code2);
  await new Promise((r) => joining2.on("exit", r));
  const daemon2Log: Buffer[] = [];
  const daemon2 = spawn(`${target}/illogicald`, [
    ...["--listen", `127.0.0.1:${DAEMON2}`, "--name", "leaving-box", "--state-dir", state2],
    ...["--shell", "bash --norc --noprofile", "--no-manager-env", "--tailscale-socket", "/nonexistent", "--no-claude-ide"],
  ], { stdio: ["ignore", "pipe", "pipe"], env: { ...process.env, RUST_LOG: "info" } });
  procs.push(daemon2);
  daemon2.stdout!.on("data", (d: Buffer) => daemon2Log.push(d));
  daemon2.stderr!.on("data", (d: Buffer) => daemon2Log.push(d));
  const d2 = await leaver.waitOnline("leaving-box", 10_000).catch(() => undefined);
  check("the leaver's machine is online", d2?.online === true);
  const sessions = await leaver.api<{ sessions: { id: string; current: boolean; agent: string }[] }>("/api/me/sessions");
  check("their sessions list this one, with what signed in", sessions.sessions.some((x) => x.current && x.agent.length > 0), JSON.stringify(sessions));
  const preview = await leaver.api<{ confirm: string; blockers: string[]; machines: number }>("/api/me/delete");
  check("deleting asks for the login, and nothing stands in the way", preview.confirm === "leaver" && preview.blockers.length === 0 && preview.machines === 1, JSON.stringify(preview));
  check("the wrong word is refused", await leaver.api("/api/me/delete", { confirm: "stranger" }).then(() => false, () => true));
  const before = Buffer.concat(daemon2Log).length;
  await leaver.api("/api/me/delete", { confirm: "leaver" });
  check("signed out: the account is gone", await leaver.api("/api/me").then(() => false, (e: Error) => / 401 /.test(e.message)));
  // Hung up on, and refused when it dials again.
  const since = () => Buffer.concat(daemon2Log).subarray(before).toString().replace(/\x1b\[[0-9;]*m/g, "");
  // It's told why (#208).
  const refused2 = /can't reach control's relay.*401.*this machine's account was deleted/;
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
  // A cleanup error is noise: the checks have already said what they said.
  try {
    await cleanup();
  } catch (e) {
    console.log("cleanup:", e);
  }
  gh.close();
  for (const d of dirs) {
    try {
      rmSync(d, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
    } catch (e) {
      console.log("cleanup:", e);
    }
  }
}
process.exit(failed ? 1 : 0);
