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

import { certBody, evaluate, joinCode, normalizeCode, type Cert, type Revocation, revocationBody } from "./e2e/cert.ts";
import { forget, loadEnrollment, loadKeys, saveEnrollment, signText, type DeviceKeys, type Enrollment } from "./e2e/keys.ts";
import type { E2ETarget } from "./client";

export interface ControlInfo {
  control: true;
  url: string;
  github: boolean;
}

export interface DirDaemon {
  id: string;
  name: string;
  urls: string[];
  online: boolean;
  last_seen: number | null;
  cert: Cert;
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

export type Phase = "loading" | "signed-out" | "waiting" | "ready" | "error";

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
      const me = await api<{ account: string; login: string; root: string | null }>("/api/me").catch((e) => {
        if (e instanceof HttpError && e.status === 401) return null;
        throw e;
      });
      if (!me) {
        // Signed out, but still enrolled: the cached directory keeps known
        // daemons reachable directly while control is down.
        return this.set("signed-out");
      }
      this.login = me.login;
      this.account = me.account;
      if (this.enrollment && this.enrollment.account !== me.account) {
        // Signed in as someone else: this browser's place was in another
        // account. Start over as a new device of this one.
        this.enrollment = undefined;
      }
      if (!this.enrollment) await this.enroll(me.root);
      if (!this.enrollment) return;
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
    // Pin the root as control reports it now, and check that our own
    // certificate chains back to it.
    const all = await api<{ trust: { account: string; root: string }; certs: Cert[]; revocations: Revocation[] }>("/api/devices");
    const trusted = await evaluate(all.trust, all.certs, all.revocations);
    const mine = trusted.get(k.id);
    if (!mine) throw new Error("control approved this device, but the approval doesn't check out");
    this.enrollment = { control: location.origin, account: this.account, root: all.trust.root, cert: mine };
    await saveEnrollment(this.enrollment);
  }

  /** Devices and the directory, checked against the pinned root. */
  async refresh() {
    const e = this.enrollment;
    if (!e) return;
    try {
      const [devs, dir] = await Promise.all([
        api<{ trust: { account: string; root: string } | null; certs: Cert[]; revocations: Revocation[]; pending: Cert[] }>("/api/devices"),
        api<{ daemons: Omit<DirDaemon, "cert">[] }>("/api/directory"),
      ]);
      this.rootMismatch = !!devs.trust && devs.trust.root !== e.root;
      this.trusted = await evaluate({ account: e.account, root: e.root }, devs.certs, devs.revocations);
      this.revocations = devs.revocations;
      this.pending = devs.pending;
      this.daemons = dir.daemons.flatMap((d) => {
        const cert = this.trusted.get(d.id);
        return cert?.kind === "daemon" ? [{ ...d, cert }] : [];
      });
      this.stale = false;
      try {
        localStorage.setItem(DIR_KEY, JSON.stringify(this.daemons));
      } catch {
        // not remembered
      }
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

  /** How a Client reaches daemon `id`. */
  target(id: string): E2ETarget | undefined {
    const d = this.daemons.find((x) => x.id === id);
    if (!d) return undefined;
    const ws = this.info.url.replace(/^http/, "ws");
    return { daemon: { id: d.id, noise: d.cert.noise }, direct: d.urls, relay: `${ws}/api/relay/c/${d.id}`, keys: this.keys };
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
   * recomputed from the key control shows, so control can't swap it. */
  async showJoin(code: string): Promise<{ code: string; cert: Cert; urls: string[] }> {
    const c = normalizeCode(code);
    if (!c) throw new Error("a code is ten letters and digits");
    const j = await api<{ code: string; cert: Cert; urls: string[] }>(`/api/joins/${c}`);
    if ((await joinCode(j.cert)) !== c) throw new Error("that request doesn't match its code: not approving it");
    return j;
  }

  async approveJoin(code: string, c: Cert) {
    const signed: Cert = { ...c, account: this.account, approver: this.keys.id, sig: "" };
    signed.sig = await signText(this.keys, certBody(signed));
    await api(`/api/joins/${code}/approve`, { cert: signed });
    await this.refresh();
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
