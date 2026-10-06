// Phones for the e2e specs (#214 section 6): the device profiles, and a
// fake web-push service.
//
// A Pixel 7 runs in Chrome; an iPhone runs in Playwright's WebKit (the
// `*.webkit.spec.ts` project, or `webkit.launch()` from a Chrome spec).
//
// The fake push service stands where FCM or Apple's push service would. A
// phone subscribes with keys this module made, so what the daemon sends
// can be decrypted here (RFC 8291, aes128gcm) and its VAPID token checked
// (RFC 8292). Chrome's page gets a PushManager that hands out that
// subscription, and a payload that arrived is passed on to the page's
// service worker through the DevTools protocol, where a tap on one of its
// actions is a dispatched `notificationclick`. WebKit has no PushManager
// or Notification in Playwright, so an iPhone's subscription is posted to
// the daemon the way the page would, and the test checks what arrived.

import { spawn, type ChildProcess } from "node:child_process";
import { createECDH, createHmac, createDecipheriv, createPublicKey, randomBytes, verify } from "node:crypto";
import { createServer, type Server } from "node:http";
import { devices, expect, webkit, type Browser, type BrowserContext, type Page } from "@playwright/test";
import { ANY, daemonPort, listen } from "./ports";
import { labs } from "./labs";

type Device = (typeof devices)[string];
const phone = (d: Device) => {
  const { viewport, userAgent, deviceScaleFactor, isMobile, hasTouch } = d;
  return { viewport, userAgent, deviceScaleFactor, isMobile, hasTouch };
};
/** Context options for a Pixel 7 (Chrome). */
export const pixel7 = phone(devices["Pixel 7"]);
/** Context options for an iPhone (WebKit; Playwright has no iOS Chrome). */
export const iphone = phone(devices["iPhone 15"]);

/** WebKit from a Chrome spec: without the project's `chrome` channel. */
export const launchWebkit = (): Promise<Browser> => webkit.launch({ channel: undefined });

const b64 = (b: Buffer) => b.toString("base64url");

/** RFC 8291: decrypt an aes128gcm body sent to the subscription whose
 * private key and auth secret are given. */
export function decrypt(body: Buffer, ua: { ecdh: ReturnType<typeof createECDH>; auth: Buffer }): Buffer {
  const salt = body.subarray(0, 16);
  const rs = body.readUInt32BE(16);
  const idlen = body[20];
  const asPublic = body.subarray(21, 21 + idlen);
  const data = body.subarray(21 + idlen);
  if (data.length > rs) throw new Error("more than one record");
  const hmac = (key: Buffer, ...parts: Buffer[]) => {
    const h = createHmac("sha256", key);
    for (const p of parts) h.update(p);
    return h.digest();
  };
  const secret = ua.ecdh.computeSecret(asPublic);
  const prkKey = hmac(ua.auth, secret);
  const keyInfo = Buffer.concat([Buffer.from("WebPush: info\0"), ua.ecdh.getPublicKey(), asPublic]);
  const ikm = hmac(prkKey, keyInfo, Buffer.from([1]));
  const prk = hmac(salt, ikm);
  const cek = hmac(prk, Buffer.from("Content-Encoding: aes128gcm\0"), Buffer.from([1])).subarray(0, 16);
  const nonce = hmac(prk, Buffer.from("Content-Encoding: nonce\0"), Buffer.from([1])).subarray(0, 12);
  const d = createDecipheriv("aes-128-gcm", cek, nonce);
  d.setAuthTag(data.subarray(data.length - 16));
  const plain = Buffer.concat([d.update(data.subarray(0, data.length - 16)), d.final()]);
  // The last record ends with 0x02, then any padding of zeros.
  let end = plain.length - 1;
  while (end >= 0 && plain[end] === 0) end--;
  if (plain[end] !== 2) throw new Error("no last-record delimiter");
  return plain.subarray(0, end);
}

/** RFC 8292: `vapid t=JWT, k=KEY`, signed by KEY; its claims. */
export function vapid(header: string | undefined): { aud: string; sub: string; exp: number } {
  const m = /^vapid t=([^,\s]+),\s*k=([A-Za-z0-9_-]+)$/.exec(header ?? "");
  if (!m) throw new Error(`not a VAPID header: ${header}`);
  const [h, c, s] = m[1].split(".");
  const raw = Buffer.from(m[2], "base64url");
  const key = createPublicKey({ key: { kty: "EC", crv: "P-256", x: b64(raw.subarray(1, 33)), y: b64(raw.subarray(33, 65)) }, format: "jwk" });
  const ok = verify("sha256", Buffer.from(`${h}.${c}`), { key, dsaEncoding: "ieee-p1363" }, Buffer.from(s, "base64url"));
  if (!ok) throw new Error("VAPID signature doesn't verify");
  return JSON.parse(Buffer.from(c, "base64url").toString());
}

/** What one subscription received, decrypted. */
export interface Pushed {
  /** The subscription's name. */
  to: string;
  payload: {
    title: string;
    body: string;
    pane: number;
    tag: string;
    reason?: { kind: string; actions: string[]; bundle?: string };
    approve?: { id: string; title?: string };
    ask?: { id: string; field: string; options: string[] };
    [k: string]: unknown;
  };
  aud: string;
  urgency?: string;
}

/** A web-push service on loopback. */
export class FakePush {
  readonly got: Pushed[] = [];
  /** Requests it refused (a bad body or token), with why. */
  readonly refused: string[] = [];
  private keys = new Map<string, { ecdh: ReturnType<typeof createECDH>; auth: Buffer }>();
  private constructor(
    private server: Server,
    readonly origin: string,
  ) {}

  static async start(): Promise<FakePush> {
    let self: FakePush | null = null;
    const server = createServer((req, res) => {
      const chunks: Buffer[] = [];
      req.on("data", (c: Buffer) => chunks.push(c));
      req.on("end", () => self!.take(req.url!, req.headers, Buffer.concat(chunks), res));
    });
    self = new FakePush(server, `http://127.0.0.1:${await listen(server)}`);
    return self;
  }

  private take(path: string, headers: Record<string, string | string[] | undefined>, body: Buffer, res: import("node:http").ServerResponse) {
    const name = decodeURIComponent(path.replace(/^\/push\//, ""));
    const ua = this.keys.get(name);
    if (!ua) return void res.writeHead(410).end();
    try {
      if (headers["content-encoding"] !== "aes128gcm") throw new Error(`content-encoding ${headers["content-encoding"]}`);
      const { aud, exp } = vapid(headers.authorization as string);
      if (aud !== this.origin) throw new Error(`VAPID aud ${aud}, not ${this.origin}`);
      if (exp * 1000 < Date.now()) throw new Error("VAPID token expired");
      const payload = JSON.parse(decrypt(body, ua).toString("utf8"));
      this.got.push({ to: name, payload, aud, urgency: headers.urgency as string | undefined });
      res.writeHead(201).end();
    } catch (e) {
      this.refused.push(String(e));
      res.writeHead(400).end(String(e));
    }
  }

  /** A new subscription (as `PushSubscription.toJSON()` gives it). */
  subscription(name: string): { endpoint: string; keys: { p256dh: string; auth: string } } {
    const ecdh = createECDH("prime256v1");
    ecdh.generateKeys();
    const auth = randomBytes(16);
    this.keys.set(name, { ecdh, auth });
    return { endpoint: `${this.origin}/push/${encodeURIComponent(name)}`, keys: { p256dh: b64(ecdh.getPublicKey()), auth: b64(auth) } };
  }

  /** Chrome: the page's PushManager hands out a subscription of ours, so
   * *Notify this device* subscribes here. */
  async stub(page: Page, name: string) {
    await page.addInitScript((sub) => {
      let current: PushSubscription | null = null;
      PushManager.prototype.subscribe = async function () {
        current = { endpoint: sub.endpoint, toJSON: () => sub, unsubscribe: async () => ((current = null), true) } as unknown as PushSubscription;
        return current;
      };
      PushManager.prototype.getSubscription = async () => current;
    }, this.subscription(name));
  }

  /** The first push to `to` that `match` accepts, waiting for it. */
  async next(to: string, match: (p: Pushed["payload"]) => boolean = () => true, timeout = 15_000): Promise<Pushed["payload"]> {
    let found: Pushed | undefined;
    await expect
      .poll(() => (found = this.got.find((g) => g.to === to && match(g.payload))) !== undefined, { timeout, message: `a push to ${to} (refused: ${this.refused.join("; ")})` })
      .toBe(true);
    return found!.payload;
  }

  close() {
    this.server.closeAllConnections();
    this.server.close();
  }
}

/** Chrome: hand a payload to the page's service worker, as a push service
 * would, and wait for its notification. Returns the notification's actions. */
export async function deliver(context: BrowserContext, page: Page, payload: object): Promise<string[]> {
  await expect.poll(() => page.evaluate(async () => !!(await navigator.serviceWorker.getRegistration()))).toBe(true);
  await page.evaluate(() => navigator.serviceWorker.ready);
  const cdp = await context.newCDPSession(page);
  const registrationId = new Promise<string>((resolve) => {
    cdp.on("ServiceWorker.workerRegistrationUpdated", (e) => {
      const r = e.registrations.find((x) => !x.isDeleted);
      if (r) resolve(r.registrationId);
    });
  });
  await cdp.send("ServiceWorker.enable");
  await cdp.send("ServiceWorker.deliverPushMessage", {
    origin: new URL(page.url()).origin,
    registrationId: await registrationId,
    data: JSON.stringify(payload),
  });
  const tag = (payload as { tag?: string }).tag ?? "illogical";
  const worker = context.serviceWorkers()[0] ?? (await context.waitForEvent("serviceworker"));
  let actions: string[] = [];
  await expect
    .poll(async () => {
      actions = await worker.evaluate(async (tag) => {
        const ns = await (self as unknown as { registration: ServiceWorkerRegistration }).registration.getNotifications({ tag });
        return ns.length ? (ns[0] as unknown as { actions: { action: string }[] }).actions.map((a) => a.action) : ["(none)"];
      }, tag);
      return actions[0] !== "(none)";
    })
    .toBe(true);
  await cdp.detach();
  return actions;
}

/** Chrome: tap `action` on the notification tagged `tag`, in the worker. */
export async function tap(context: BrowserContext, tag: string, action: string) {
  const worker = context.serviceWorkers()[0] ?? (await context.waitForEvent("serviceworker"));
  await worker.evaluate(
    async ([tag, action]) => {
      const g = self as unknown as { registration: ServiceWorkerRegistration; dispatchEvent(e: Event): boolean };
      const [n] = await g.registration.getNotifications({ tag });
      if (!n) throw new Error(`no notification tagged ${tag}`);
      const Ev = (self as unknown as { NotificationEvent: typeof Event }).NotificationEvent;
      g.dispatchEvent(new (Ev as unknown as new (t: string, i: object) => Event)("notificationclick", { notification: n, action }));
    },
    [tag, action] as const,
  );
}

/** A throwaway daemon on a port of its own; stop it with `kill`. */
export async function daemon(state: string, args: string[] = [], env: Record<string, string> = {}): Promise<{ proc: ChildProcess; port: number; url: string }> {
  const proc = spawn(
    "../target/debug/illogicald",
    ["--listen", ANY, "--state-dir", labs(state), "--shell", "bash --norc --noprofile", "--no-manager-env", ...args],
    { stdio: "ignore", env: { ...process.env, ...env } },
  );
  const port = await daemonPort(state, proc);
  const url = `http://127.0.0.1:${port}`;
  for (let i = 0; i < 100; i++) {
    try {
      if ((await fetch(`${url}/api/host`)).ok) return { proc, port, url };
    } catch {
      // not up yet
    }
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error("daemon did not start");
}
