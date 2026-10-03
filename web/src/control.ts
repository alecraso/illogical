// The page served by illogical control (M17): who you are, this browser's
// place in your account, and the daemons you can reach.
//
// Signing in (GitHub) only opens control's API. What lets this browser
// reach a daemon is its device key, approved by a device the account
// already trusts (the first one is trusted on enrollment). When this
// browser is enrolled it pins the account's root device, and from then on
// believes only certificates that chain back to it: control hands out the
// directory and the keys, but it can't slip in a daemon of its own (and a
// daemon can't be reached by a device it hasn't approved).

import { certBody, deviceId, evaluate, hex, joinCode, normalizeCode, type Cert, type Revocation, revocationBody, unhex } from "./e2e/cert.ts";
import { forget, loadEnrollment, loadKeys, saveEnrollment, saveWorkerDirectory, signText, type DeviceKeys, type Enrollment } from "./e2e/keys.ts";
import type { E2ETarget } from "./client";
import { follows, signRoster, teamJoinBody, word, type AccountCerts, type Roster, type TeamPin, type TeamRole } from "./e2e/team.ts";

export interface ControlInfo {
  control: true;
  url: string;
  github: boolean;
  /** WebAuthn works here (control has a domain name, not an IP). */
  passkeys: boolean;
  /** Control's VAPID public key (M21). */
  vapid: string;
}

export interface DirDaemon {
  id: string;
  name: string;
  urls: string[];
  online: boolean;
  last_seen: number | null;
  cert: Cert;
  /** A hosted sandbox's id (M20), if it is one. */
  sandbox?: string | null;
  /** Someone else's (M19): its owner account and their login, and the
   * team it belongs to, if any. */
  account?: string;
  owner_name?: string;
  team?: string | null;
}

/** Another account's machine as the directory lists it, with that account's
 * certificates to check it by. */
interface ForeignEntry extends Omit<DirDaemon, "cert"> {
  chain?: { trust: { account: string; root: string } | null; certs: Cert[]; revocations: Revocation[] };
}

export interface JoinRequest {
  code: string;
  cert: Cert;
  urls: string[];
  team: { team: string; name: string } | null;
}

export interface Team {
  team: string;
  pin: TeamPin;
  locked: boolean;
  roster: Roster;
  role: TeamRole | null;
  requests: { account: string; root: string; name: string; role: TeamRole; created: number }[];
  certs: AccountCerts;
  /** The founder is the one this browser pinned on first sight. */
  verified: boolean;
}

const PINS_KEY = "illogical.control.pins";

/** First sight of another account's root (trust on first use): pinned
 * here, and a different one later is refused, not believed. */
function pins(): Record<string, string> {
  try {
    return JSON.parse(localStorage.getItem(PINS_KEY) ?? "{}");
  } catch {
    return {};
  }
}

function pin(account: string, root: string): boolean {
  const p = pins();
  if (p[account] && p[account] !== root) return false;
  if (!p[account]) {
    p[account] = root;
    try {
      localStorage.setItem(PINS_KEY, JSON.stringify(p));
    } catch {
      // not remembered
    }
  }
  return true;
}

/** Served by control? (A daemon answers 404.) */
export async function detectControl(): Promise<ControlInfo | null> {
  try {
    const res = await fetch("/control.json", { cache: "no-store" });
    if (!res.ok) return null;
    const j = (await res.json()) as Partial<ControlInfo>;
    return j.control ? (j as ControlInfo) : null;
  } catch {
    return null;
  }
}

class HttpError extends Error {
  status: number;
  constructor(status: number, message: string) {
    super(message);
    this.status = status;
  }
}

async function api<T>(path: string, body?: unknown): Promise<T> {
  const res = await fetch(path, {
    method: body === undefined ? "GET" : "POST",
    headers: body === undefined ? {} : { "Content-Type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const j = (await res.json().catch(() => ({}))) as T & { error?: string };
  if (!res.ok) throw new HttpError(res.status, j.error ?? `HTTP ${res.status}`);
  return j;
}

const DIR_KEY = "illogical.control.directory";

const b64u = (b: ArrayBuffer | Uint8Array) =>
  btoa(String.fromCharCode(...new Uint8Array(b instanceof Uint8Array ? b : new Uint8Array(b))))
    .replace(/\+/g, "-")
    .replace(/\//g, "_")
    .replace(/=+$/, "");
const unb64u = (s: string) => Uint8Array.from(atob(s.replace(/-/g, "+").replace(/_/g, "/")), (c) => c.charCodeAt(0));

/** Sign in with a passkey (any registered on this control). */
export async function passkeySignIn(): Promise<void> {
  const o = await api<{ challenge: string; rpId: string; userVerification: UserVerificationRequirement; timeout: number }>("/auth/passkey/login", {});
  const cred = (await navigator.credentials.get({
    publicKey: { challenge: unb64u(o.challenge), rpId: o.rpId, userVerification: o.userVerification, timeout: o.timeout },
  })) as PublicKeyCredential | null;
  if (!cred) throw new Error("no passkey chosen");
  const r = cred.response as AuthenticatorAssertionResponse;
  await api("/auth/passkey/login/finish", {
    id: b64u(cred.rawId),
    clientDataJSON: b64u(r.clientDataJSON),
    authenticatorData: b64u(r.authenticatorData),
    signature: b64u(r.signature),
  });
}

/** Make a passkey: for the account signed in, or a new account. */
export async function passkeyRegister(): Promise<void> {
  type Options = {
    challenge: string;
    rp: PublicKeyCredentialRpEntity;
    user: { id: string; name: string; displayName: string };
    pubKeyCredParams: PublicKeyCredentialParameters[];
    authenticatorSelection: AuthenticatorSelectionCriteria;
    attestation: AttestationConveyancePreference;
    timeout: number;
  };
  const o = await api<Options>("/auth/passkey/register", {});
  const cred = (await navigator.credentials.create({
    publicKey: { ...o, challenge: unb64u(o.challenge), user: { ...o.user, id: unb64u(o.user.id) } },
  })) as PublicKeyCredential | null;
  if (!cred) throw new Error("no passkey made");
  const r = cred.response as AuthenticatorAttestationResponse;
  await api("/auth/passkey/register/finish", {
    id: b64u(cred.rawId),
    clientDataJSON: b64u(r.clientDataJSON),
    attestationObject: b64u(r.attestationObject),
  });
}

export type Phase = "loading" | "signed-out" | "waiting" | "ready" | "error";

// ---- recovery codes: an Ed25519 seed each, on paper only.

const B32 = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
const PKCS8_ED25519 = unhex("302e020100300506032b657004220420");

function toCode(seed: Uint8Array): string {
  let bits = 0;
  let val = 0;
  let out = "";
  for (const b of seed) {
    val = (val << 8) | b;
    bits += 8;
    while (bits >= 5) {
      out += B32[(val >>> (bits - 5)) & 31];
      bits -= 5;
    }
  }
  if (bits) out += B32[(val << (5 - bits)) & 31];
  return out.match(/.{1,4}/g)!.join("-");
}

function fromCode(code: string): Uint8Array<ArrayBuffer> | null {
  const c = code.toUpperCase().replace(/[^A-Z2-7]/g, "");
  if (c.length !== 52) return null;
  const out: number[] = [];
  let bits = 0;
  let val = 0;
  for (const ch of c) {
    val = (val << 5) | B32.indexOf(ch);
    bits += 5;
    if (bits >= 8) {
      out.push((val >>> (bits - 8)) & 255);
      bits -= 8;
    }
  }
  return Uint8Array.from(out.slice(0, 32));
}

const subtle = globalThis.crypto.subtle;

async function recoveryKey(seed: Uint8Array): Promise<CryptoKey> {
  const pkcs8 = new Uint8Array(48);
  pkcs8.set(PKCS8_ED25519);
  pkcs8.set(seed, 16);
  return subtle.importKey("pkcs8", pkcs8, { name: "Ed25519" }, false, ["sign"]);
}

const NO_NOISE = "0".repeat(64);

/** A browser's name in its account's device list. */
function deviceName(): string {
  const ua = navigator.userAgent;
  const os = /iPhone/.test(ua) ? "iPhone" : /iPad/.test(ua) ? "iPad" : /Android/.test(ua) ? "Android" : /Mac/.test(ua) ? "Mac" : /Windows/.test(ua) ? "Windows" : /Linux/.test(ua) ? "Linux" : "browser";
  const app = /Edg\//.test(ua) ? "Edge" : /Firefox\//.test(ua) ? "Firefox" : /Chrome\//.test(ua) ? "Chrome" : /Safari\//.test(ua) ? "Safari" : "";
  return `${app ? `${app} on ` : ""}${os}`;
}

export class ControlSession {
  phase: Phase = "loading";
  error = "";
  login = "";
  account = "";
  /** Passkeys registered to the account. */
  passkeys = 0;
  /** Shown once, right after the account's first device enrolls. */
  recoveryCodes: string[] | null = null;
  keys!: DeviceKeys;
  enrollment: Enrollment | undefined;
  /** Every device the account trusts, by this browser's reckoning. */
  trusted = new Map<string, Cert>();
  /** Devices asking to join, for this one to approve. */
  pending: Cert[] = [];
  revocations: Revocation[] = [];
  daemons: DirDaemon[] = [];
  /** The directory is the saved one: control didn't answer. */
  stale = false;
  /** Control says the account's root is a different device than the one
   * this browser pinned: don't trust anything new from it. */
  rootMismatch = false;
  private listeners = new Set<() => void>();
  private timer: number | undefined;
  readonly info: ControlInfo;

  constructor(info: ControlInfo) {
    this.info = info;
  }

  subscribe(fn: () => void): () => void {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  }

  private emit() {
    for (const fn of this.listeners) fn();
  }

  private set(phase: Phase, error = "") {
    this.phase = phase;
    this.error = error;
    this.emit();
  }

  /** Sign-in state, enrollment, then the directory, kept fresh. */
  async boot() {
    try {
      this.keys = await loadKeys();
      this.enrollment = await loadEnrollment(location.origin);
      const me = await api<{ account: string; login: string; root: string | null; passkeys: number }>("/api/me").catch((e) => {
        if (e instanceof HttpError && e.status === 401) return null;
        throw e;
      });
      if (!me) {
        // Signed out, but still enrolled: the cached directory keeps known
        // daemons reachable directly while control is down.
        return this.set("signed-out");
      }
      this.login = me.login || "you";
      this.account = me.account;
      this.passkeys = me.passkeys;
      if (this.enrollment && this.enrollment.account !== me.account) {
        // Signed in as someone else: this browser's place was in another
        // account. Start over as a new device of this one.
        this.enrollment = undefined;
      }
      if (!this.enrollment) await this.enroll(me.root);
      if (!this.enrollment) return;
      if (this.usedRecovery) {
        // Spent: nobody gets in with it again.
        await this.revoke(this.usedRecovery).catch(() => {});
        this.usedRecovery = null;
      }
      await this.refresh();
      this.set("ready");
      this.timer = window.setInterval(() => void this.refresh(), 10_000);
    } catch (e) {
      this.set("error", String((e as Error).message ?? e));
    }
  }

  private async enroll(root: string | null) {
    const k = this.keys;
    const cert: Cert = {
      v: 1,
      account: this.account,
      device: k.id,
      kind: "browser",
      name: deviceName(),
      noise: k.noisePub,
      sign: k.signPub,
      created: Date.now(),
      approver: "",
      sig: "",
    };
    if (root === null) {
      // The account's first device signs itself.
      cert.approver = k.id;
      cert.sig = await signText(k, certBody(cert));
    }
    let r = await api<{ approved: boolean; cert?: Cert; root: string | null }>("/api/devices", { cert });
    if (!r.approved) {
      this.set("waiting");
      while (!r.approved) {
        await new Promise((res) => setTimeout(res, 2000));
        r = await api<{ approved: boolean; cert: Cert }>(`/api/devices/${k.id}`).then(
          (x) => ({ ...x, root: null }),
          (e) => {
            if (e instanceof HttpError && e.status === 404) throw new Error("this device's request was turned down");
            return { approved: false, root: null };
          },
        );
      }
    }
    if (root === null) await this.makeRecoveryCodes();
    // Pin the root as control reports it now, and check that our own
    // certificate chains back to it.
    const all = await api<{ trust: { account: string; root: string }; certs: Cert[]; revocations: Revocation[] }>("/api/devices");
    const trusted = await evaluate(all.trust, all.certs, all.revocations);
    const mine = trusted.get(k.id);
    if (!mine) throw new Error("control approved this device, but the approval doesn't check out");
    this.enrollment = { control: location.origin, account: this.account, root: all.trust.root, cert: mine };
    await saveEnrollment(this.enrollment);
  }

  /** Two recovery codes, signed by this (the first) device. */
  private async makeRecoveryCodes() {
    const codes: string[] = [];
    const certs: Cert[] = [];
    for (let i = 1; i <= 2; i++) {
      const kp = (await subtle.generateKey({ name: "Ed25519" }, true, ["sign", "verify"])) as CryptoKeyPair;
      const seed = new Uint8Array(await subtle.exportKey("pkcs8", kp.privateKey)).slice(16);
      const sign = new Uint8Array(await subtle.exportKey("raw", kp.publicKey));
      const c: Cert = {
        v: 1,
        account: this.account,
        device: await deviceId(unhex(NO_NOISE), sign),
        kind: "recovery",
        name: `recovery code ${i}`,
        noise: NO_NOISE,
        sign: hex(sign),
        created: Date.now(),
        approver: this.keys.id,
        sig: "",
      };
      c.sig = await signText(this.keys, certBody(c));
      certs.push(c);
      codes.push(toCode(seed));
    }
    await api("/api/recovery", { certs });
    this.recoveryCodes = codes;
  }

  savedRecoveryCodes() {
    this.recoveryCodes = null;
    this.emit();
  }

  /** While waiting for approval: approve this browser with a recovery
   * code instead, then retire the code. */
  async useRecoveryCode(code: string) {
    const seed = fromCode(code);
    if (!seed) throw new Error("a recovery code is 52 letters and digits");
    const key = await recoveryKey(seed);
    const devs = await api<{ trust: { account: string; root: string } | null; certs: Cert[]; revocations: Revocation[] }>("/api/devices");
    // Only codes still good (a used one was revoked).
    const live = devs.trust ? [...(await evaluate(devs.trust, devs.certs, devs.revocations)).values()] : [];
    const probe = new TextEncoder().encode("illogical recovery probe");
    const sig = new Uint8Array(await subtle.sign("Ed25519", key, probe));
    let mine: Cert | undefined;
    for (const c of live.filter((c) => c.kind === "recovery")) {
      const pub = await subtle.importKey("raw", unhex(c.sign), { name: "Ed25519" }, false, ["verify"]);
      if (await subtle.verify("Ed25519", pub, sig, probe)) mine = c;
    }
    if (!mine) throw new Error("that isn't one of this account's recovery codes (or it was used)");
    const k = this.keys;
    const cert: Cert = {
      v: 1,
      account: this.account,
      device: k.id,
      kind: "browser",
      name: deviceName(),
      noise: k.noisePub,
      sign: k.signPub,
      created: Date.now(),
      approver: mine.device,
      sig: "",
    };
    cert.sig = hex(new Uint8Array(await subtle.sign("Ed25519", key, new TextEncoder().encode(certBody(cert)))));
    await api(`/api/devices/${k.id}/approve`, { cert });
    this.usedRecovery = mine.device;
  }

  /** The recovery code that let this browser in, to retire once enrolled. */
  private usedRecovery: string | null = null;

  /** Devices and the directory, checked against the pinned root. */
  async refresh() {
    const e = this.enrollment;
    if (!e) return;
    try {
      const [devs, dir] = await Promise.all([
        api<{ trust: { account: string; root: string } | null; certs: Cert[]; revocations: Revocation[]; pending: Cert[] }>("/api/devices"),
        api<{ daemons: ForeignEntry[] }>("/api/directory"),
      ]);
      this.rootMismatch = !!devs.trust && devs.trust.root !== e.root;
      this.trusted = await evaluate({ account: e.account, root: e.root }, devs.certs, devs.revocations);
      this.revocations = devs.revocations;
      this.pending = devs.pending;
      const daemons: DirDaemon[] = [];
      for (const d of dir.daemons) {
        const { chain, ...entry } = d;
        let cert = this.trusted.get(d.id);
        // Someone else's machine: by their account's certificates, from
        // the root this browser pinned for them on first sight.
        if (!cert && d.account && chain?.trust && pin(d.account, chain.trust.root)) {
          cert = (await evaluate(chain.trust, chain.certs, chain.revocations)).get(d.id);
        }
        if (cert?.kind === "daemon") daemons.push({ ...entry, cert });
      }
      this.daemons = daemons;
      await this.loadTeams();
      await this.loadSandboxes();
      await this.loadBilling();
      this.stale = false;
      try {
        localStorage.setItem(DIR_KEY, JSON.stringify(this.daemons));
      } catch {
        // not remembered
      }
      // The service worker answers notifications over the same channels
      // (M29), from what this page checked.
      const ws = this.info.url.replace(/^http/, "ws");
      void saveWorkerDirectory(
        this.daemons.map((d) => ({ id: d.id, noise: d.cert.noise, direct: d.urls, relay: `${ws}/api/relay/c/${d.id}` })),
      ).catch(() => {});
    } catch (err) {
      if (err instanceof HttpError && err.status === 401) {
        window.clearInterval(this.timer);
        return this.set("signed-out");
      }
      this.stale = true;
      try {
        // Daemons this browser already checked stay reachable directly.
        this.daemons = (JSON.parse(localStorage.getItem(DIR_KEY) ?? "[]") as DirDaemon[]).map((d) => ({ ...d, online: false }));
      } catch {
        // nothing saved
      }
    }
    this.emit();
  }

  // ---- billing (M22)

  billing: {
    billing: boolean;
    plan: string;
    relay: { bytes: number; allowance: number; warning: boolean; slowed: boolean };
    sandbox_minutes: number;
    teams: { team: string; name: string; owner: boolean; seats: number; plan: string; sandbox_minutes: number }[];
  } | null = null;

  async loadBilling() {
    this.billing = await api<NonNullable<ControlSession["billing"]>>("/api/billing").catch(() => null);
  }

  /** Off to Stripe Checkout to upgrade (a team, or this account). */
  async upgrade(team?: string) {
    const r = await api<{ url: string }>("/api/billing/checkout", { team });
    location.href = r.url;
  }

  // ---- hosted sandboxes (M20)

  sandboxes: { id: string; state: string; device: string; join: { code: string; cert: Cert } | null }[] = [];
  /** Hosted sandboxes are open to this account. */
  sandboxesOpen = false;
  /** One this browser asked for, to show when it's up. */
  starting: string | null = null;

  async loadSandboxes() {
    const r = await api<{ sandboxes: ControlSession["sandboxes"]; open: boolean }>("/api/sandboxes").catch(() => null);
    if (!r) return;
    this.sandboxes = r.sandboxes;
    this.sandboxesOpen = r.open;
    // The device that asked approves its sandbox's daemon by itself: the
    // code is recomputed from the key, as for any join.
    for (const s of r.sandboxes) {
      if (!s.join || s.device !== this.keys.id) continue;
      if ((await joinCode(s.join.cert)) !== s.join.code) continue;
      const signed: Cert = { ...s.join.cert, account: this.account, approver: this.keys.id, sig: "" };
      signed.sig = await signText(this.keys, certBody(signed));
      await api(`/api/joins/${s.join.code}/approve`, { cert: signed }).catch(() => {});
    }
  }

  /** A hosted VM (M20); resolves with its id once asked for. */
  async startSandbox(): Promise<string> {
    const r = await api<{ id: string }>("/api/sandboxes", { device: this.keys.id });
    this.starting = r.id;
    this.emit();
    // Poll quickly while it starts: approve its join, then wait for its
    // daemon in the directory.
    void (async () => {
      for (let i = 0; i < 180 && this.starting === r.id; i++) {
        await new Promise((res) => setTimeout(res, 1000));
        await this.refresh();
        if (this.daemons.some((d) => d.sandbox === r.id)) break;
        const s = this.sandboxes.find((x) => x.id === r.id);
        if (s?.state.startsWith("failed")) break;
      }
      this.emit();
    })();
    return r.id;
  }

  async deleteSandbox(id: string) {
    await fetch(`/api/sandboxes/${id}`, { method: "DELETE" });
    await this.refresh();
  }

  // ---- teams and people (M19)

  teams: Team[] = [];

  async loadTeams() {
    const r = await api<{ teams: Omit<Team, "verified">[] }>("/api/teams").catch(() => ({ teams: [] as Omit<Team, "verified">[] }));
    const out: Team[] = [];
    for (const t of r.teams) {
      // The founder pinned on first sight. The team's daemons check each
      // roster version against it; this browser checks the ones it signs.
      const ok = pin(`team:${t.team}`, `${t.pin.founder}.${t.pin.founder_root}`);
      out.push({ ...t, verified: ok });
    }
    this.teams = out;
  }

  private myMember(): { account: string; root: string; name: string } {
    return { account: this.account, root: this.enrollment!.root, name: word(this.login) };
  }

  async createTeam(name: string) {
    const team = Array.from(crypto.getRandomValues(new Uint8Array(8)), (b) => b.toString(16).padStart(2, "0")).join("");
    const roster = await signRoster(
      { v: 1, team, name: name.trim().slice(0, 80) || "team", version: 1, at: Date.now(), members: [{ ...this.myMember(), role: "owner" }] },
      this.keys,
    );
    await api("/api/teams", { roster });
    await this.refresh();
  }

  /** Sign and send the team's next roster, as `change` makes it. */
  async changeTeam(team: string, change: (members: Roster["members"]) => Roster["members"], name?: string) {
    const t = this.teams.find((x) => x.team === team);
    if (!t) throw new Error("no such team");
    const prev = t.roster;
    const next = await signRoster(
      { v: 1, team, name: name ?? prev.name, version: prev.version + 1, at: Date.now(), members: change(prev.members.map((m) => ({ ...m }))) },
      this.keys,
    );
    if (!(await follows(next, prev, t.pin, t.certs))) throw new Error("that change doesn't check out (are you an owner here?)");
    await api(`/api/teams/${team}/roster`, { roster: next });
    await this.refresh();
  }

  async admit(team: string, req: Team["requests"][number]) {
    await this.changeTeam(team, (ms) => [...ms.filter((m) => m.account !== req.account), { account: req.account, root: req.root, role: req.role, name: word(req.name) }]);
  }

  async invite(team: string, role: TeamRole): Promise<string> {
    const r = await api<{ link: string }>(`/api/teams/${team}/invites`, { role });
    return r.link;
  }

  async showInvite(team: string, code: string) {
    return api<{ team: string; name: string; role: TeamRole }>(`/api/invites/${team}/${code}`);
  }

  async acceptInvite(team: string, code: string) {
    await api(`/api/invites/${team}/${code}/accept`, {});
  }

  async rejectRequest(team: string, account: string) {
    await api(`/api/teams/${team}/requests/${account}/reject`, {});
    await this.refresh();
  }

  async lockTeam(team: string, locked: boolean) {
    await api(`/api/teams/${team}/lock`, { locked });
    await this.refresh();
  }

  /** Someone on control, by their login: whom to share with, and the root
   * device to pin for them (compare its fingerprint with them). */
  async person(login: string) {
    return api<{ account: string; name: string; root: string }>(`/api/people?login=${encodeURIComponent(login)}`);
  }

  /** Hand control a push subscription, signed by this device (M21). */
  async subscribePush(sub: { endpoint: string; p256dh: string; auth: string }) {
    const s = { v: 1, account: this.account, device: this.keys.id, endpoint: sub.endpoint, p256dh: sub.p256dh, auth: sub.auth, at: Date.now(), sig: "" };
    const body = `illogical push v1\naccount ${s.account}\ndevice ${s.device}\nendpoint ${s.endpoint}\np256dh ${s.p256dh}\nauth ${s.auth}\nat ${s.at}\n`;
    s.sig = await signText(this.keys, body);
    await api("/api/push/subscribe", { sub: s });
  }

  async unsubscribePush(endpoint: string) {
    await api("/api/push/unsubscribe", { endpoint });
  }

  /** How a Client reaches daemon `id`. */
  target(id: string): E2ETarget | undefined {
    const d = this.daemons.find((x) => x.id === id);
    if (!d) return undefined;
    const ws = this.info.url.replace(/^http/, "ws");
    // Hosted sandboxes go through their provider, a socket each; the rest
    // share the page's one socket to the relay (M25).
    const mux = d.sandbox ? undefined : `${ws}/api/relay/m`;
    return { daemon: { id: d.id, noise: d.cert.noise }, direct: d.urls, relay: `${ws}/api/relay/c/${d.id}`, mux, keys: this.keys };
  }

  /** A read-only link to a session on daemon `id` (M19): a one-off key, its
   * private half only in the link's fragment. */
  async makeLink(request: (m: string, p: string, b?: unknown) => Promise<{ ok: boolean; json<T>(): Promise<T> }>, id: string, session: number, ttlSecs: number, history: boolean): Promise<string> {
    const d = this.daemons.find((x) => x.id === id);
    if (!d) throw new Error("no such machine");
    const kp = (await crypto.subtle.generateKey({ name: "X25519" }, true, ["deriveBits"])) as CryptoKeyPair;
    const seed = new Uint8Array(await crypto.subtle.exportKey("pkcs8", kp.privateKey)).slice(16);
    const pub = new Uint8Array(await crypto.subtle.exportKey("raw", kp.publicKey));
    const res = await request("POST", "/api/links", { session, key: hex(pub), ttl_secs: ttlSecs, history });
    if (!res.ok) throw new Error((await res.json<{ error?: string }>().catch(() => null))?.error ?? "couldn't make a link");
    return `${this.info.url}/#link=${d.id}.${d.cert.noise}.${hex(seed)}.${hex(pub)}`;
  }

  /** Approve another device's request: sign its certificate. */
  async approve(c: Cert) {
    const signed: Cert = { ...c, account: this.account, approver: this.keys.id, sig: "" };
    signed.sig = await signText(this.keys, certBody(signed));
    await api(`/api/devices/${c.device}/approve`, { cert: signed });
    await this.refresh();
  }

  async reject(c: Cert) {
    await api(`/api/devices/${c.device}/reject`, {});
    await this.refresh();
  }

  /** A daemon's join request, by the code it printed. The code is
   * recomputed from the key control shows, so control can't swap it.
   * `team` is the one it asked for (`--team`), if any. */
  async showJoin(code: string): Promise<JoinRequest> {
    const c = normalizeCode(code);
    if (!c) throw new Error("a code is ten letters and digits");
    const j = await api<JoinRequest>(`/api/joins/${c}`);
    if ((await joinCode(j.cert)) !== c) throw new Error("that request doesn't match its code: not approving it");
    return j;
  }

  /** Approve a daemon into this account, or into `team` (one I own): this
   * device signs the team in, so control can't pick one (#100). */
  async approveJoin(code: string, c: Cert, team: string | null = null) {
    const signed: Cert = { ...c, account: this.account, approver: this.keys.id, sig: "" };
    signed.sig = await signText(this.keys, certBody(signed));
    let teamSig: string | null = null;
    if (team) {
      const t = this.teams.find((x) => x.team === team && x.role === "owner" && x.verified);
      if (!t) throw new Error("only the team's owners add its machines");
      teamSig = await signText(this.keys, teamJoinBody(c.device, t.pin));
    }
    await api(`/api/joins/${code}/approve`, { cert: signed, team, team_sig: teamSig });
    await this.refresh();
  }

  /** Turn a daemon's join down: it stops waiting. */
  async rejectJoin(code: string) {
    await api(`/api/joins/${code}/reject`, { device: this.keys.id });
  }

  async revoke(id: string) {
    const r: Revocation = { v: 1, account: this.account, device: id, at: Date.now(), by: this.keys.id, sig: "" };
    r.sig = await signText(this.keys, revocationBody(r));
    await api("/api/revocations", { revocation: r });
    await this.refresh();
  }

  async signOut(forgetDevice: boolean) {
    await api("/auth/logout", {}).catch(() => {});
    if (forgetDevice) await forget(location.origin);
    location.href = "/";
  }
}
