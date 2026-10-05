// A headless approving device: what a signed-in browser or phone does on
// control, without a browser, for tests that need someone to approve
// something. It signs in through control's GitHub sign-in (against
// fakes.ts's GitHub), enrolls its device keys, approves join codes, checks
// the account's devices, and reaches a daemon end to end, through control's
// relay or directly. It uses the web client's own e2e code (src/e2e), so it
// is the browser's behaviour, not a copy of it.
//
//   const me = await Device.signIn({ control: "http://127.0.0.1:7690", login: "alice" });
//   await me.approveJoin("ABCDE-FGHIJ");
//   const box = await me.waitOnline("box");
//   await me.roundTrip(box.id, "MARKER");
//
// `device-cli.ts` is the same from a shell, keeping the device in a file.
// docs/testing.md describes both.

import { readFileSync, writeFileSync } from "node:fs";
import { certBody, evaluate, joinCode, unhex, type Cert, type Revocation } from "../src/e2e/cert.ts";
import { E2ESocket } from "../src/e2e/channel.ts";
import { generateKeys, signText, type DeviceKeys } from "../src/e2e/keys.ts";

const subtle = globalThis.crypto.subtle;
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

export interface DeviceOptions {
  /** Control's public URL, as it knows itself (its `--public-url`). Sent as
   * the Origin, and where requests go unless `via` says otherwise. */
  control: string;
  /** Where requests for a URL prefix really go, for a control or fake
   * GitHub whose public URL the test can't reach as written (a container's
   * address): `{ "http://10.229.80.10:8080": "http://127.0.0.1:22980" }`. */
  via?: Record<string, string>;
}

export interface SignInOptions extends DeviceOptions {
  /** Who the fake GitHub signs in [default: "stranger"]. */
  login?: string;
  /** This device's name on the account [default: "laptop"]. */
  name?: string;
  /** `browser` (the default) or `cli`. */
  kind?: Cert["kind"];
}

/** A daemon as control's directory lists it. */
export interface Listed {
  id: string;
  name: string;
  online: boolean;
  urls: string[];
}

/** What `save` writes and `load` reads: the keys (private halves as JWK),
 * the session and the pinned account. Test data only: it holds a usable
 * device key. */
interface Saved {
  control: string;
  via: Record<string, string>;
  cookie: string;
  account: string;
  login: string;
  root: string;
  name: string;
  approved: boolean;
  keys: { id: string; noisePub: string; signPub: string; noise: JsonWebKey; sign: JsonWebKey };
}

export class Device {
  readonly control: string;
  readonly via: Record<string, string>;
  keys: DeviceKeys;
  /** The session cookie (`ilg_session=…`). */
  cookie = "";
  /** The account's id on control, and the GitHub login it signed in as. */
  account = "";
  login = "";
  /** The account root this device pinned when it enrolled: what every
   * certificate it accepts has to chain back to. */
  root = "";
  name = "";
  /** Whether the account trusts this device yet (the first one is trusted
   * on enrollment; later ones wait for an approval). */
  approved = false;

  private constructor(opts: DeviceOptions, keys: DeviceKeys) {
    this.control = opts.control.replace(/\/$/, "");
    this.via = opts.via ?? {};
    this.keys = keys;
  }

  /** Sign in, make device keys and enroll them. The account's first device
   * signs itself and is trusted at once; a later one is left waiting for
   * `approveDevice` from a trusted one. */
  static async signIn(opts: SignInOptions): Promise<Device> {
    const d = new Device(opts, await generateKeys(true));
    await d.session(opts.login ?? "stranger");
    d.name = opts.name ?? "laptop";
    const me = await d.api<{ account: string; login: string }>("/api/me");
    d.account = me.account;
    d.login = me.login;
    const first = await d.api<{ trust: { root: string } | null }>("/api/devices");
    const kind = opts.kind ?? "browser";
    // The first device signs its own certificate; any other asks.
    const own = !first.trust;
    const c = own ? await d.sign(d.keys, kind, d.name) : { ...(await d.sign(d.keys, kind, d.name)), approver: "", sig: "" };
    const r = await d.api<{ approved: boolean }>("/api/devices", { cert: c });
    d.approved = r.approved;
    d.root = own ? d.keys.id : first.trust!.root;
    return d;
  }

  /** Sign in again with the same keys (a new session): after control
   * restarted with a fresh database, say, or to switch who's signed in. */
  async session(login = this.login || "stranger"): Promise<void> {
    let url = `${this.control}/auth/github?next=/`;
    const jar: Record<string, string> = {};
    for (let i = 0; i < 6; i++) {
      // The fake GitHub signs in whoever the authorize URL names.
      const u = new URL(url);
      if (u.pathname === "/login/oauth/authorize") u.searchParams.set("login", login);
      const res = await fetch(this.reach(u.href), {
        redirect: "manual",
        headers: { cookie: Object.entries(jar).map(([k, v]) => `${k}=${v}`).join("; ") },
      });
      for (const c of res.headers.getSetCookie()) {
        const [kv] = c.split(";");
        const at = kv.indexOf("=");
        jar[kv.slice(0, at)] = kv.slice(at + 1);
      }
      const loc = res.headers.get("location");
      if (!loc) {
        if (!jar.ilg_session) throw new Error(`signing in as ${login}: ${res.status} ${(await res.text()).slice(0, 200)}`);
        break;
      }
      url = new URL(loc, url).href;
    }
    if (!jar.ilg_session) throw new Error(`signing in as ${login}: no session cookie`);
    this.cookie = `ilg_session=${jar.ilg_session}`;
  }

  /** A URL as this test can reach it (`via`). */
  reach(url: string): string {
    for (const [from, to] of Object.entries(this.via)) if (url.startsWith(from)) return to + url.slice(from.length);
    return url;
  }

  /** Control's API as this device's session: GET without a body, POST with
   * one. Throws with control's status and error on anything but 2xx. */
  async api<T>(path: string, body?: unknown, method?: string): Promise<T> {
    const res = await fetch(this.reach(this.control + path), {
      method: method ?? (body === undefined ? "GET" : "POST"),
      headers: { cookie: this.cookie, origin: this.control, ...(body === undefined ? {} : { "content-type": "application/json" }) },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    const j = await res.json().catch(() => ({}));
    if (!res.ok) throw new Error(`${path}: ${res.status} ${JSON.stringify(j)}`);
    return j as T;
  }

  /** A certificate for `k`, signed by `by` (this device unless given). */
  async sign(k: DeviceKeys, kind: Cert["kind"], name: string, by: DeviceKeys = this.keys): Promise<Cert> {
    const c: Cert = { v: 1, account: this.account, device: k.id, kind, name, noise: k.noisePub, sign: k.signPub, created: Date.now(), approver: by.id, sig: "" };
    c.sig = await signText(by, certBody(c));
    return c;
  }

  /** Approve another device waiting on the account (one that signed in
   * and enrolled after the first). */
  async approveDevice(other: Device): Promise<void> {
    await this.api(`/api/devices/${other.keys.id}/approve`, { cert: await this.sign(other.keys, "browser", other.name) });
    other.approved = true;
  }

  /** Approve a daemon's join code (what `illogicald join` prints after
   * `#join=`) into this account, as the approve page does: the code must be
   * the one the waiting daemon's key gives. Returns its certificate. */
  async approveJoin(code: string): Promise<Cert> {
    const shown = await this.api<{ cert: Cert }>(`/api/joins/${code}`);
    const want = await joinCode(shown.cert);
    if (want !== code) throw new Error(`the daemon waiting on ${code} has a key whose code is ${want}`);
    const c: Cert = { ...shown.cert, account: this.account, approver: this.keys.id, sig: "" };
    c.sig = await signText(this.keys, certBody(c));
    await this.api(`/api/joins/${code}/approve`, { cert: c });
    return c;
  }

  /** The account's devices and machines that chain back to the root this
   * device pinned, by device id. */
  async trusted(): Promise<Map<string, Cert>> {
    const all = await this.api<{ trust: { account: string; root: string }; certs: Cert[]; revocations?: Revocation[] }>("/api/devices");
    return evaluate({ account: this.account, root: this.root }, all.certs, all.revocations ?? []);
  }

  async directory(): Promise<Listed[]> {
    return (await this.api<{ daemons: Listed[] }>("/api/directory")).daemons;
  }

  /** Wait until the directory lists a daemon (by name or id) as online. */
  async waitOnline(daemon: string, timeoutMs = 30_000): Promise<Listed> {
    const until = Date.now() + timeoutMs;
    let seen: Listed | undefined;
    for (;;) {
      seen = (await this.directory()).find((d) => d.id === daemon || d.name === daemon);
      if (seen?.online) return seen;
      if (Date.now() > until) throw new Error(`${daemon} isn't online after ${timeoutMs / 1000}s: ${JSON.stringify(seen ?? "not listed")}`);
      await sleep(250);
    }
  }

  /** An end-to-end socket to a daemon the account trusts: through
   * control's relay, or `direct` (its own `/e2e` URL). Its Noise key comes
   * from a certificate that chains to this device's root, never from the
   * directory alone. */
  async connect(daemonId: string, direct?: string, timeoutMs = 5000): Promise<E2ESocket> {
    const cert = (await this.trusted()).get(daemonId);
    if (cert?.kind !== "daemon") throw new Error(`${daemonId} isn't a daemon this account trusts`);
    const ws = this.reach(this.control).replace(/^http/, "ws");
    const url = direct ?? `${ws}/api/relay/c/${daemonId}`;
    const headers = direct ? {} : { cookie: this.cookie, origin: this.control };
    const orig = globalThis.WebSocket;
    // Node's WebSocket (undici) takes headers as a second argument; the
    // socket is made synchronously inside connect, so swapping the global
    // for the call is enough.
    globalThis.WebSocket = class extends orig {
      constructor(u: string | URL) {
        super(u, { headers } as unknown as string[]);
      }
    } as typeof WebSocket;
    try {
      return await E2ESocket.connect([{ url, timeoutMs }], { id: daemonId, noise: cert.noise }, this.keys);
    } finally {
      globalThis.WebSocket = orig;
    }
  }

  /** Type `echo <marker>-$((6*7))` into the daemon's first pane and wait
   * for `<marker>-42` to come back: a pane reached and working, both ways.
   * Returns what the pane printed. Through the relay unless `direct`. */
  async roundTrip(daemonId: string, marker: string, opts: { direct?: string; timeoutMs?: number } = {}): Promise<string> {
    const sock = await this.connect(daemonId, opts.direct);
    try {
      const texts: string[] = [];
      let out = "";
      sock.onText = (t) => texts.push(t);
      sock.onBinary = (b) => {
        if (b[0] === 1 || b[0] === 2) out += new TextDecoder().decode(b.subarray(13));
      };
      sock.start();
      const until = Date.now() + (opts.timeoutMs ?? 15_000);
      const wait = async (what: string, ok: () => boolean) => {
        while (!ok()) {
          if (Date.now() > until) throw new Error(`timed out waiting for ${what}; the pane showed ${JSON.stringify(out.slice(-300))}`);
          await sleep(100);
        }
      };
      await wait("the daemon's hello", () => texts.some((t) => t.includes('"hello"')));
      const hello = JSON.parse(texts.find((t) => t.includes('"hello"'))!);
      const pane: number | undefined = hello.state?.panes?.[0]?.id;
      if (pane === undefined) throw new Error("the daemon has no panes");
      sock.sendText(JSON.stringify({ type: "attach", panes: [{ pane, offset: null }] }));
      const input = new TextEncoder().encode(`echo ${marker}-$((6*7))\n`);
      const frame = new Uint8Array(13 + input.length);
      frame[0] = 3;
      new DataView(frame.buffer).setUint32(1, pane);
      frame.set(input, 13);
      sock.sendBinary(frame);
      await wait(`${marker}-42`, () => out.includes(`${marker}-42`));
      return out;
    } finally {
      sock.close();
    }
  }

  /** Keep this device in a file (keys, session, pinned root), for a test
   * that runs in steps. */
  async save(file: string): Promise<void> {
    const jwk = (k: CryptoKey) => subtle.exportKey("jwk", k);
    const s: Saved = {
      control: this.control,
      via: this.via,
      cookie: this.cookie,
      account: this.account,
      login: this.login,
      root: this.root,
      name: this.name,
      approved: this.approved,
      keys: {
        id: this.keys.id,
        noisePub: this.keys.noisePub,
        signPub: this.keys.signPub,
        noise: await jwk(this.keys.noise.privateKey),
        sign: await jwk(this.keys.sign.privateKey),
      },
    };
    writeFileSync(file, JSON.stringify(s, null, 2), { mode: 0o600 });
  }

  static async load(file: string): Promise<Device> {
    const s = JSON.parse(readFileSync(file, "utf8")) as Saved;
    const pair = async (alg: string, priv: JsonWebKey, pub: string, use: KeyUsage[], pubUse: KeyUsage[]): Promise<CryptoKeyPair> => ({
      privateKey: await subtle.importKey("jwk", priv, { name: alg }, true, use),
      publicKey: await subtle.importKey("raw", unhex(pub), { name: alg }, true, pubUse),
    });
    const keys: DeviceKeys = {
      id: s.keys.id,
      noisePub: s.keys.noisePub,
      signPub: s.keys.signPub,
      noise: await pair("X25519", s.keys.noise, s.keys.noisePub, ["deriveBits"], []),
      sign: await pair("Ed25519", s.keys.sign, s.keys.signPub, ["sign"], ["verify"]),
    };
    const d = new Device({ control: s.control, via: s.via }, keys);
    Object.assign(d, { cookie: s.cookie, account: s.account, login: s.login, root: s.root, name: s.name, approved: s.approved });
    return d;
  }
}
