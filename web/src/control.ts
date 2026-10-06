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
import { desktopApp } from "./desktop";
import {
  follows,
  inviteKey,
  moveBody,
  newInviteKey,
  redeemInvite,
  signInvite,
  signRoster,
  teamJoinBody,
  word,
  type AccountCerts,
  type Invite,
  type Roster,
  type TeamPin,
  type TeamRole,
} from "./e2e/team.ts";

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
  /** Members' names as they set them, by account (#208): the roster's
   * one-word form ("Sam-Stranger") is only what's signed. */
  names?: Record<string, string>;
  /** The founder is the one this browser pinned on first sight. */
  verified: boolean;
}

/** A one-click invite link control still holds (#134). */
export interface PresignedInvite {
  key: string;
  role: TeamRole;
  expires: number;
  by: string;
  by_name: string;
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
  body: Record<string, unknown>;
  constructor(status: number, message: string, body: Record<string, unknown> = {}) {
    super(message);
    this.status = status;
    this.body = body;
  }
}

/** An approval control refused (#327): its reason code, and what to do. */
export class RefusedError extends Error {
  reason: string;
  constructor(reason: string, message: string) {
    super(message);
    this.reason = reason;
  }
}

/** The machine was approved, but a device approved with it wasn't
 * (#326): the machine's prompt is done, and that device asks on its own. */
export class AlongsideError extends Error {
  constructor(name: string, why: string) {
    super(`The machine is approved, but not ${name}: ${why}`);
  }
}

export async function api<T>(path: string, body?: unknown): Promise<T> {
  const res = await fetch(path, {
    method: body === undefined ? "GET" : "POST",
    headers: body === undefined ? {} : { "Content-Type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const j = (await res.json().catch(() => ({}))) as T & { error?: string };
  if (!res.ok) throw new HttpError(res.status, j.error ?? `HTTP ${res.status}`, j as Record<string, unknown>);
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

/** Make a passkey: for the account signed in, or a new account called
 * `name` (#102: what teammates see). */
export async function passkeyRegister(name?: string): Promise<void> {
  type Options = {
    challenge: string;
    rp: PublicKeyCredentialRpEntity;
    user: { id: string; name: string; displayName: string };
    pubKeyCredParams: PublicKeyCredentialParameters[];
    authenticatorSelection: AuthenticatorSelectionCriteria;
    attestation: AttestationConveyancePreference;
    timeout: number;
  };
  const o = await api<Options>("/auth/passkey/register", name === undefined ? {} : { name });
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

/** What a signed-out page may know about an invite (#103): the team's
 * name and who made it. */
export async function previewInvite(team: string, code: string, presigned: boolean): Promise<{ name: string; by: string } | null> {
  if (presigned) {
    const key = await inviteKey(code).then((k) => k.key, () => null);
    return key ? api<{ name: string; by: string }>(`/api/presigned/${team}/${key}/preview`).catch(() => null) : null;
  }
  return api<{ name: string; by: string }>(`/api/invites/${team}/${code}/preview`).catch(() => null);
}

/** A team invite in a link's fragment: `#invite=<team>.<code>` asks an
 * owner first; `#pinvite=<team>.<seed>` is presigned and carries its
 * one-time key's seed. A page from before presigned invites doesn't know
 * the second, so it never sends the seed to control as a code. */
export function inviteInHash(hash: string): { team: string; code: string; presigned: boolean } | null {
  const m = /^#(p?)invite=([0-9a-f]+)\.([0-9a-f]+)$/.exec(hash);
  return m ? { team: m[2], code: m[3], presigned: m[1] === "p" } : null;
}

const PENDING_INVITE = "illogical:presigned-invite";

/** Where signing in with GitHub comes back to: this page, but never with a
 * presigned invite's seed, which control mustn't see. That waits in this
 * tab's sessionStorage for `restoreInvite`. */
export function signInNext(): string {
  if (!inviteInHash(location.hash)?.presigned) return location.pathname + location.hash;
  try {
    sessionStorage.setItem(PENDING_INVITE, location.hash);
  } catch {
    // Without storage the link has to be opened again after signing in.
  }
  return location.pathname;
}

/** Back from signing in: the presigned invite link `signInNext` kept. */
export function restoreInvite() {
  try {
    const hash = sessionStorage.getItem(PENDING_INVITE);
    sessionStorage.removeItem(PENDING_INVITE);
    if (hash && !location.hash && inviteInHash(hash)?.presigned) history.replaceState(null, "", location.pathname + location.search + hash);
  } catch {
    // Nothing kept.
  }
}

/** How long a presigned invite lasts unless the owner says otherwise. */
const PRESIGNED_TTL_MS = 24 * 3600_000;

export type Phase = "loading" | "signed-out" | "waiting" | "turned-down" | "lost-key" | "untrusted" | "ready" | "error";

/** What the person reads when control refuses an approval (#327), by its
 * reason code: which check failed, and what to do next. `revoked` isn't
 * here: control's own words say it (#330), and a removed machine asks
 * again with a new key by itself. */
const REFUSED: Record<string, string> = {
  approver_untrusted:
    "This browser isn't one your account trusts any more, so its approvals are refused. Forget it and enroll it again (another of your devices or a recovery code approves it), then try again.",
  bad_signature: "This browser's key isn't the one your account approved. Forget it and enroll it again, then try again.",
  cant_approve: "A machine can't approve devices. Approve from a browser, phone or the illogical CLI.",
  recovery_for_machine: "A recovery code approves browsers and phones, not machines. Approve the machine from one of your devices.",
  no_chain: "Control's records for your account don't add up from here (the approval doesn't chain to your first device). Reload and try again; if it keeps happening, approve from another device.",
  no_devices: "This account has no devices yet. Reload: this browser becomes its first.",
};

/** Refusals that mean this browser itself isn't trusted. */
const SELF_REFUSED = new Set(["approver_untrusted", "bad_signature"]);

/** Where `enrollAgain` leaves a note for the next page: open the recovery
 * form (#327). */
const RECOVER_KEY = "illogical.control.recover";

/** The page came from forgetting a stale browser: offer the recovery code
 * at once. Read once. */
export function cameToRecover(): boolean {
  try {
    const yes = sessionStorage.getItem(RECOVER_KEY) === "1";
    sessionStorage.removeItem(RECOVER_KEY);
    return yes;
  } catch {
    return false;
  }
}

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
export function deviceName(): string {
  // M48: the desktop app says what it is ("illogical app on jake-air").
  const app0 = (globalThis as { __illogicalApp?: { name?: string } }).__illogicalApp?.name;
  if (app0) return app0;
  const ua = navigator.userAgent;
  const os = /iPhone/.test(ua) ? "iPhone" : /iPad/.test(ua) ? "iPad" : /Android/.test(ua) ? "Android" : /Mac/.test(ua) ? "Mac" : /Windows/.test(ua) ? "Windows" : /Linux/.test(ua) ? "Linux" : "browser";
  const app = /Edg\//.test(ua) ? "Edge" : /Firefox\//.test(ua) ? "Firefox" : /Chrome\//.test(ua) ? "Chrome" : /Safari\//.test(ua) ? "Safari" : "";
  return `${app ? `${app} on ` : ""}${os}`;
}

/** The machine's join code this page was opened to approve
 * (`#join=CODE`), if any. */
export function joinInHash(hash: string = location.hash): string | null {
  const m = /^#join=([A-Za-z0-9-]+)$/.exec(hash);
  return m ? normalizeCode(m[1]) : null;
}

/** Whether this page is the desktop app's window (M48). */
export function inApp(): boolean {
  return !!desktopApp();
}

/** M48: the desktop app's window, after the person allowed its sign-in in
 * their browser: `#app-redeem=<ticket>.<grant>.<verifier>` from the app,
 * posted here for the session cookie. Only in the app's own window (a link
 * to this in a browser would sign it in as someone else). */
async function redeemAppLogin() {
  const m = /^#app-redeem=([0-9a-f]+)\.([0-9a-f]+)\.([0-9a-f]+)$/.exec(location.hash);
  if (!m) return;
  history.replaceState(null, "", location.pathname + location.search);
  if (!inApp()) return;
  await api(`/auth/app/${m[1]}/redeem`, { grant: m[2], verifier: m[3] }).catch(() => {});
}

/** Someone offering to share a session on their machine with this
 * account: it isn't listed, or let in, until accepted. */
export interface ShareOffer {
  daemon: string;
  /** The machine's name, as its owner's machine says. */
  name: string;
  account: string;
  owner_name: string;
  owner_login: string;
}

export class ControlSession {
  phase: Phase = "loading";
  error = "";
  /** What to call the account here: its name, else its login. */
  login = "";
  /** What other people see (#102): the name it chose, or its GitHub
   * login. Empty for a passkey account made before names. */
  name = "";
  account = "";
  /** Passkeys registered to the account. */
  passkeys = 0;
  /** Shown once, right after the account's first device enrolls (or new
   * ones are made). */
  recoveryCodes: string[] | null = null;
  /** While turned down: the name of the device that did it. */
  turnedDownBy = "";
  /** Asks again after a turn-down. */
  private askAgain: (() => void) | null = null;
  keys!: DeviceKeys;
  enrollment: Enrollment | undefined;
  /** Every device the account trusts, by this browser's reckoning. */
  trusted = new Map<string, Cert>();
  /** Devices asking to join, for this one to approve. */
  pending: Cert[] = [];
  /** #326: the machine's join code a waiting device came to approve, by
   * the device's id, while that join is open: approved together. */
  pendingJoins = new Map<string, string>();
  revocations: Revocation[] = [];
  daemons: DirDaemon[] = [];
  /** Shares waiting for this account's yes. */
  offers: ShareOffer[] = [];
  /** The directory is the saved one: control didn't answer. */
  stale = false;
  /** Control says the account's root is a different device than the one
   * this browser pinned: don't trust anything new from it. */
  rootMismatch = false;
  /** Why the account doesn't trust this browser (#327), in the
   * `untrusted` phase: a device removed it, or it's missing. */
  untrustedWhy: "removed" | "missing" = "missing";
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
      await redeemAppLogin();
      this.keys = await loadKeys();
      this.enrollment = await loadEnrollment(location.origin);
      if (this.enrollment && this.enrollment.cert.device !== this.keys.id) {
        // #94: the key this browser enrolled with didn't come back from
        // its storage (WebKit loses X25519 keys there), so it would sign
        // approvals nobody trusts. Say so, not fail later.
        return this.set("lost-key");
      }
      const me = await api<{ account: string; login: string; name?: string; root: string | null; passkeys: number }>("/api/me").catch((e) => {
        if (e instanceof HttpError && e.status === 401) return null;
        throw e;
      });
      if (!me) {
        // Signed out, but still enrolled: the cached directory keeps known
        // daemons reachable directly while control is down.
        return this.set("signed-out");
      }
      this.name = me.name ?? "";
      this.login = this.name || me.login || "you";
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
      // #327: a browser the account no longer trusts says so at once.
      this.set(this.phase === "untrusted" ? "untrusted" : "ready");
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
    this.request = cert;
    // #326: here to approve a machine, which this browser can't until it's
    // one of the account's devices: the device that approves it is shown
    // the machine alongside, and approves both at once.
    const join = joinInHash();
    const ask = () => api<{ approved: boolean; cert?: Cert }>("/api/devices", { cert, join });
    let r = await ask();
    if (!r.approved) this.brought = join;
    while (!r.approved) {
      this.set("waiting");
      await new Promise((res) => setTimeout(res, 2000));
      try {
        r = await api<{ approved: boolean; cert: Cert }>(`/api/devices/${k.id}`);
      } catch (e) {
        if (!(e instanceof HttpError && e.status === 404)) continue;
        // Turned down (#105): say so, until this browser asks again.
        this.turnedDownBy = typeof e.body.by === "string" ? e.body.by : "";
        this.set("turned-down");
        await new Promise<void>((res) => (this.askAgain = res));
        this.askAgain = null;
        r = await ask();
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

  /** Two recovery codes, signed by this device (the first, or one making
   * new ones). */
  private async makeRecoveryCodes(revocations: Revocation[] = []) {
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
    await api("/api/recovery", { certs, revocations });
    this.recoveryCodes = codes;
  }

  /** This browser's request, as it asks to join. */
  private request: Cert | null = null;

  /** #326: the machine's join code this browser waited with, which the
   * device that approved it may have approved alongside. */
  brought: string | null = null;

  /** After a turn-down: ask again. */
  tryAgain() {
    this.askAgain?.();
  }

  /** Recovery codes still good. */
  get recoveryLeft(): number {
    return [...this.trusted.values()].filter((c) => c.kind === "recovery").length;
  }

  /** New recovery codes, signed by this device; the old ones are revoked
   * in the same request, so they stop working (#106). */
  async newRecoveryCodes() {
    const old = [...this.trusted.values()].filter((c) => c.kind === "recovery");
    const revocations: Revocation[] = [];
    for (const c of old) {
      const r: Revocation = { v: 1, account: this.account, device: c.device, at: Date.now(), by: this.keys.id, sig: "" };
      r.sig = await signText(this.keys, revocationBody(r));
      revocations.push(r);
    }
    await this.makeRecoveryCodes(revocations);
    await this.refresh();
  }

  /** A passkey for the account signed in. */
  async addPasskey() {
    await passkeyRegister();
    await this.refreshMe();
  }

  /** How many passkeys the account has now. */
  async refreshMe() {
    this.passkeys = (await api<{ passkeys: number }>("/api/me")).passkeys;
    this.emit();
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
    // Turned down: ask again first, so there's a request to approve.
    if (this.phase === "turned-down" && this.request) await api("/api/devices", { cert: this.request });
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
    this.tryAgain();
  }

  /** The recovery code that let this browser in, to retire once enrolled. */
  private usedRecovery: string | null = null;

  /** Devices and the directory, checked against the pinned root. */
  async refresh() {
    const e = this.enrollment;
    if (!e) return;
    try {
      const [devs, dir] = await Promise.all([
        api<{ trust: { account: string; root: string } | null; certs: Cert[]; revocations: Revocation[]; pending: Cert[]; joins?: Record<string, string> }>("/api/devices"),
        api<{ daemons: ForeignEntry[]; offers?: ShareOffer[] }>("/api/directory"),
      ]);
      this.rootMismatch = !!devs.trust && devs.trust.root !== e.root;
      this.trusted = await evaluate({ account: e.account, root: e.root }, devs.certs, devs.revocations);
      this.revocations = devs.revocations;
      // #327: this browser's own place, checked like any other device's.
      // (A different root is said elsewhere, and isn't fixed by enrolling
      // again under control's.)
      const mine = this.trusted.has(this.keys.id) || this.rootMismatch;
      if (!mine) this.untrustedWhy = devs.revocations.some((r) => r.device === this.keys.id) ? "removed" : "missing";
      if (!mine && (this.phase === "ready" || this.phase === "loading")) this.phase = "untrusted";
      else if (mine && this.phase === "untrusted") this.phase = "ready";
      this.pending = devs.pending;
      this.pendingJoins = new Map(Object.entries(devs.joins ?? {}));
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
      this.offers = dir.offers ?? [];
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
  /** Teams this account asked to join, waiting on an owner (#103). */
  asked: { team: string; name: string; owners: string[] }[] = [];
  /** A team that took this account in while this page watched, to say so. */
  joined: { team: string; name: string } | null = null;
  /** What control kept to tell this account once (#206: a team it was in
   * was deleted), oldest first. */
  notices: { id: number; title: string; body: string }[] = [];

  /** Whether the last load of the teams worked: an empty list is then
   * this account's, not a failure. */
  teamsLoaded = false;

  async loadTeams() {
    let loaded = true;
    const r = await api<{ teams: Omit<Team, "verified">[]; asked?: ControlSession["asked"]; notices?: ControlSession["notices"] }>(
      "/api/teams",
    ).catch(() => {
      loaded = false;
      return { teams: [] as Omit<Team, "verified">[], asked: this.asked, notices: this.notices };
    });
    this.teamsLoaded = loaded;
    this.notices = r.notices ?? [];
    const out: Team[] = [];
    for (const t of r.teams) {
      // The founder pinned on first sight. The team's daemons check each
      // roster version against it; this browser checks the ones it signs.
      const ok = pin(`team:${t.team}`, `${t.pin.founder}.${t.pin.founder_root}`);
      out.push({ ...t, verified: ok });
    }
    const now = r.asked ?? [];
    const yes = this.asked.find((a) => !now.some((x) => x.team === a.team) && out.some((t) => t.team === a.team));
    if (yes) this.joined = { team: yes.team, name: yes.name };
    this.asked = now;
    this.teams = out;
  }

  /** The teams this browser pinned and checked, for a machine of this
   * account's to check their rosters by (#233): `<founder>.<founder's
   * root>` by team. */
  teamPins(): Record<string, string> {
    const out: Record<string, string> = {};
    for (const t of this.teams) if (t.verified && t.role !== null) out[t.team] = `${t.pin.founder}.${t.pin.founder_root}`;
    return out;
  }

  /** A machine of this account's own: not a team's, not someone else's. */
  owns(id: string): boolean {
    const d = this.daemons.find((x) => x.id === id);
    return !!d && (!d.account || d.account === this.account) && !d.team;
  }

  sawJoined() {
    this.joined = null;
    this.emit();
  }

  /** Seen: control forgets it. */
  async sawNotice(id: number) {
    this.notices = this.notices.filter((n) => n.id !== id);
    this.emit();
    await api(`/api/me/notices/${id}/seen`, {}).catch(() => {});
  }

  private myMember(): { account: string; root: string; name: string } {
    // Never "you": teammates see this.
    return { account: this.account, root: this.enrollment!.root, name: word(this.name || `account-${this.account.slice(0, 6)}`) };
  }

  /** Change what other people see (#102). */
  async setName(name: string) {
    const r = await api<{ name: string }>("/api/me/name", { name });
    this.name = r.name;
    this.login = r.name;
    this.emit();
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
    // Used presigned invites stay listed until they'd have expired anyway,
    // so none works twice.
    const now = Date.now();
    const spent = (prev.spent ?? []).filter((x) => x.expires > now);
    const next = await signRoster(
      {
        v: spent.length ? 2 : 1,
        team,
        name: name ?? prev.name,
        version: prev.version + 1,
        at: now,
        members: change(prev.members.map((m) => ({ ...m }))),
        ...(spent.length ? { spent } : {}),
      },
      this.keys,
    );
    if (!(await follows(next, prev, t.pin, t.certs))) throw new Error("that change doesn't check out (are you an owner here?)");
    await api(`/api/teams/${team}/roster`, { roster: next });
    await this.refresh();
  }

  async admit(team: string, req: Team["requests"][number]) {
    await this.changeTeam(team, (ms) => [...ms.filter((m) => m.account !== req.account), { account: req.account, root: req.root, role: req.role, name: word(req.name) }]);
  }

  /** An invite link. Presigned unless `askFirst` (or for an owner): this
   * device signs it now, and whoever opens it is in as soon as they accept.
   * The one-time key's seed goes only into the link's fragment. */
  async invite(team: string, role: TeamRole, askFirst = false): Promise<string> {
    return (await this.makeInvite(team, role, askFirst)).link;
  }

  /** As `invite`, saying whether the link asks an owner first, and why
   * when it does though it wasn't asked to: control makes a presigned one
   * only once every machine checking the team's rosters understands it. */
  async makeInvite(team: string, role: TeamRole, askFirst = false): Promise<{ link: string; asks: boolean; why?: string }> {
    const old = async (why?: string) => {
      const r = await api<{ link: string }>(`/api/teams/${team}/invites`, { role });
      return { link: r.link, asks: true, why };
    };
    if (askFirst || role === "owner") return old();
    const { seed, key } = await newInviteKey();
    const presigned = await signInvite({ team, role, expires: Date.now() + PRESIGNED_TTL_MS, key }, this.keys);
    try {
      await api(`/api/teams/${team}/invites`, { role, presigned });
    } catch (e) {
      if (e instanceof HttpError && e.status === 409) return old(e.message);
      throw e;
    }
    return { link: `${location.origin}/#pinvite=${team}.${seed}`, asks: false };
  }

  /** What a presigned invite link says, as control has it. */
  async showPresigned(team: string, seed: string) {
    const { key } = await inviteKey(seed);
    return api<{ invite: Invite; name: string; pin: TeamPin; roster: Roster; certs: AccountCerts }>(`/api/presigned/${team}/${key}`);
  }

  /** Join a team with a presigned invite: write its next version, adding
   * this account, signed here and by the link's one-time key. */
  async redeem(team: string, seed: string) {
    for (let attempt = 0; ; attempt++) {
      const p = await this.showPresigned(team, seed);
      if (!pin(`team:${team}`, `${p.pin.founder}.${p.pin.founder_root}`)) throw new Error("this team isn't the one this browser saw before");
      const next = await redeemInvite(p.roster, p.invite, seed, this.myMember());
      const mine = await api<{ certs: Cert[]; revocations: Revocation[] }>("/api/devices");
      const certs: AccountCerts = { ...p.certs, [this.account]: [mine.certs, mine.revocations] };
      if (!(await follows(next, p.roster, p.pin, certs))) throw new Error("that invite doesn't check out");
      try {
        await api(`/api/teams/${team}/roster`, { roster: next });
        break;
      } catch (e) {
        // Someone else joined first: build on their version.
        if (!(e instanceof HttpError && e.status === 409) || attempt >= 2) throw e;
      }
    }
    await this.refresh();
  }

  async showInvite(team: string, code: string) {
    return api<{ team: string; name: string; role: TeamRole }>(`/api/invites/${team}/${code}`);
  }

  async acceptInvite(team: string, code: string) {
    await api(`/api/invites/${team}/${code}/accept`, {});
    await this.refresh();
  }

  async rejectRequest(team: string, account: string) {
    await api(`/api/teams/${team}/requests/${account}/reject`, {});
    await this.refresh();
  }

  /** A team's one-click links nobody has used yet (owners only, #134). */
  async presignedInvites(team: string) {
    return (await api<{ invites: PresignedInvite[] }>(`/api/teams/${team}/presigned`)).invites;
  }

  /** Cancel one of them: control refuses it from now on. */
  async cancelPresigned(team: string, key: string) {
    const res = await fetch(`/api/teams/${team}/presigned/${key}`, { method: "DELETE" });
    if (!res.ok && res.status !== 404) {
      const j = (await res.json().catch(() => ({}))) as { error?: string };
      throw new HttpError(res.status, j.error ?? `HTTP ${res.status}`, j);
    }
  }

  async lockTeam(team: string, locked: boolean) {
    await api(`/api/teams/${team}/lock`, { locked });
    await this.refresh();
  }

  /** Someone on control, by their login or name: whom to share with, and the root
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
    await this.approveDevice(c);
    await this.refresh();
  }

  private async approveDevice(c: Cert) {
    const signed: Cert = { ...c, account: this.account, approver: this.keys.id, sig: "" };
    signed.sig = await signText(this.keys, certBody(signed));
    await this.approving(api(`/api/devices/${c.device}/approve`, { cert: signed }));
  }

  /** An approval's request: refused, it says which check failed and what
   * to do (#327); one that says this browser isn't trusted shows that
   * screen. */
  private async approving(req: Promise<unknown>) {
    try {
      await req;
    } catch (e) {
      const reason = e instanceof HttpError && typeof e.body.reason === "string" ? e.body.reason : "";
      if (!reason) throw e;
      // Checked here too: if this browser's own reckoning agrees, refresh
      // switches to the screen that says so.
      if (SELF_REFUSED.has(reason)) await this.refresh();
      throw new RefusedError(reason, REFUSED[reason] ?? (e as Error).message);
    }
  }

  async reject(c: Cert) {
    await api(`/api/devices/${c.device}/reject`, { by: this.keys.id });
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

  /** Teams I can put my machines in (#332): any I'm in, but only those I
   * own while they're locked. */
  addableTeams(): Team[] {
    return this.teams.filter((t) => t.verified && (t.role === "owner" || !t.locked));
  }

  /** Approve a daemon into this account, or into `team` (one I'm in): this
   * device signs the team in, so control can't pick one (#100). `devices`
   * waiting with its code (#326) are approved with it, before the page
   * hears of either, so its prompt stays up until both are done. */
  async approveJoin(code: string, c: Cert, team: string | null = null, devices: Cert[] = []) {
    const signed: Cert = { ...c, account: this.account, approver: this.keys.id, sig: "" };
    signed.sig = await signText(this.keys, certBody(signed));
    let teamSig: string | null = null;
    if (team) {
      const t = this.addableTeams().find((x) => x.team === team);
      if (!t) throw new Error("only the team's members add machines to it");
      teamSig = await signText(this.keys, teamJoinBody(c.device, t.pin));
    }
    await this.approving(api(`/api/joins/${code}/approve`, { cert: signed, team, team_sig: teamSig }));
    try {
      for (const d of devices) {
        try {
          await this.approveDevice(d);
        } catch (e) {
          throw new AlongsideError(d.name, (e as Error).message);
        }
      }
    } finally {
      await this.refresh();
    }
  }

  /** Move a machine of this account into `team` (one I'm in), or back to
   * the account (null). This device signs it for the daemon to check. A
   * team's owner takes someone else's machine out of it the same way. */
  async moveDaemon(daemon: string, team: string | null) {
    let pin: TeamPin | null = null;
    if (team) {
      const t = this.addableTeams().find((x) => x.team === team);
      if (!t) throw new Error("only the team's members add machines to it");
      pin = t.pin;
    }
    const at = Date.now();
    const sig = await signText(this.keys, moveBody(daemon, pin, at));
    await api(`/api/daemons/${daemon}/team`, { team: pin, at, by: this.keys.id, sig });
    await this.refresh();
  }

  /** Turn a daemon's join down: it stops waiting. */
  /** M48: a desktop app asking to sign in as this account (`#app=`):
   * where it asked from, and whether that's this browser's network. */
  async showAppLogin(id: string): Promise<{ name: string; code: string; allowed: boolean; from: string; same_network: boolean }> {
    return api(`/api/app-login/${encodeURIComponent(id)}`);
  }

  /** Allow it: control answers with the app's loopback address, which this
   * browser hands the grant to (only the app on this computer hears it). */
  async allowAppLogin(id: string): Promise<string> {
    const r = await api<{ redirect: string }>(`/api/app-login/${encodeURIComponent(id)}/allow`, {});
    return r.redirect;
  }

  /** Answer a share someone offered: accepted, it's listed and reachable. */
  async answerShare(daemon: string, accept: boolean) {
    await api(`/api/shares/${encodeURIComponent(daemon)}`, { accept });
    this.offers = this.offers.filter((o) => o.daemon !== daemon);
    this.emit();
    if (accept) {
      // The machine fetches this account's certificates when control
      // nudges it; the directory lists it once it lets this account in.
      setTimeout(() => void this.refresh(), 1500);
    }
  }

  async rejectJoin(code: string) {
    await api(`/api/joins/${code}/reject`, { device: this.keys.id });
  }

  async revoke(id: string) {
    const r: Revocation = { v: 1, account: this.account, device: id, at: Date.now(), by: this.keys.id, sig: "" };
    r.sig = await signText(this.keys, revocationBody(r));
    await api("/api/revocations", { revocation: r });
    await this.refresh();
  }

  /** After a lost key (#94): forget this browser's place in the account
   * and ask to join again, as a new device. One the account stopped
   * trusting (#327) asks with the recovery form open; a lost key asks as
   * before, the form a button away. */
  async enrollAgain(recover = false) {
    await forget(location.origin);
    if (recover)
      try {
        sessionStorage.setItem(RECOVER_KEY, "1");
      } catch {
        // The form stays a button away.
      }
    location.reload();
  }

  async signOut(forgetDevice: boolean) {
    await api("/auth/logout", {}).catch(() => {});
    if (forgetDevice) await forget(location.origin);
    location.href = "/";
  }
}
