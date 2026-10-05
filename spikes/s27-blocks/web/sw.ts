// The block's service worker (control's code, on the block's origin). It
// answers every request of the block's pages from the daemon, over the
// block's Noise channel; control only ever sees ciphertext.
//
// - The channel is opened on demand and again after it drops (the browser
//   stops an idle worker, which closes its socket).
// - A refused grant (expired) asks the pages for a new one: they ask the
//   parent page, which signs it, and hand it back here.
// - HTML pages get the WebSocket shim first in their <head>.
// - Cookies: a response built here can't set them, and requests here
//   don't carry them, so the worker keeps the block's cookie jar itself.

import { BlockChannel, type Creds, load, Refused, relayUrl, save } from "./chan.ts";

declare const self: ServiceWorkerGlobalScope;

const SHIM = `<script src="/.s27/shim.js"></script>`;
const NULL_BODY = new Set([101, 103, 204, 205, 304]);

let creds: Creds | undefined;
let chan: Promise<BlockChannel> | null = null;
let jar: Record<string, string> | undefined;
let waiting: ((c: Creds) => void)[] = [];
const stats = { started: Date.now(), connects: 0, refused: 0, renewals: 0, requests: 0, failures: 0, lastConnectMs: 0 };

self.addEventListener("install", () => void self.skipWaiting());
self.addEventListener("activate", (e) => e.waitUntil(self.clients.claim()));

self.addEventListener("message", (e) => {
  const m = e.data;
  const port = e.ports[0];
  if (m?.s27 === "creds") {
    creds = m.creds;
    chan = null;
    for (const w of waiting) w(m.creds);
    waiting = [];
    e.waitUntil(save("creds", m.creds).then(() => port?.postMessage({ ok: true })));
  } else if (m?.s27 === "probe") {
    // Can this worker open a WebSocket to `url`? (For the tests.)
    const w = new WebSocket(m.url);
    const done = (r: string) => {
      port?.postMessage(r);
      try {
        w.close();
      } catch {}
    };
    w.onopen = () => done("open");
    w.onerror = () => done("error");
    setTimeout(() => done("timeout"), 5000);
  } else if (m?.s27 === "stats") {
    port?.postMessage({ ...stats, connected: chan !== null });
  }
});

async function getCreds(): Promise<Creds | undefined> {
  creds ??= await load<Creds>("creds");
  return creds;
}

/** New credentials, from the parent page by way of a page of ours. */
async function renew(): Promise<Creds> {
  stats.renewals++;
  const got = new Promise<Creds>((res, rej) => {
    waiting.push(res);
    setTimeout(() => rej(new Error("no new grant came")), 10_000);
  });
  for (const c of await self.clients.matchAll({ type: "window", includeUncontrolled: true })) c.postMessage({ s27: "renew" });
  return got;
}

async function connect(): Promise<BlockChannel> {
  let c = await getCreds();
  if (!c) throw new Error("this block has no key");
  const t0 = performance.now();
  for (let attempt = 0; ; attempt++) {
    try {
      const ch = await BlockChannel.connect(relayUrl(c, self.location.host), c);
      stats.connects++;
      stats.lastConnectMs = performance.now() - t0;
      return ch;
    } catch (e) {
      if (e instanceof Refused && /expired/.test(e.message) && attempt === 0) {
        stats.refused++;
        c = await renew();
        continue;
      }
      throw e;
    }
  }
}

function channel(): Promise<BlockChannel> {
  if (!chan) {
    const p = connect().then((ch) => {
      ch.onclose = () => {
        if (chan === p) chan = null;
      };
      return ch;
    });
    p.catch(() => {
      if (chan === p) chan = null;
    });
    chan = p;
  }
  return chan;
}

async function cookies(): Promise<Record<string, string>> {
  jar ??= (await load<Record<string, string>>("jar")) ?? {};
  return jar;
}

function keep(setCookie: string[]) {
  if (!setCookie.length || !jar) return;
  for (const sc of setCookie) {
    const [pair, ...attrs] = sc.split(";");
    const i = pair.indexOf("=");
    if (i < 0) continue;
    const name = pair.slice(0, i).trim();
    const gone = attrs.some((a) => /^\s*max-age\s*=\s*(0|-)/i.test(a)) || attrs.some((a) => /^\s*expires\s*=/i.test(a) && Date.parse(a.split("=")[1]) < Date.now());
    if (gone) delete jar[name];
    else jar[name] = pair.slice(i + 1).trim();
  }
  void save("jar", jar);
}

async function tunnel(req: Request, navigate: boolean): Promise<Response> {
  const url = new URL(req.url);
  const headers: [string, string][] = [];
  req.headers.forEach((v, k) => headers.push([k, v]));
  if (req.referrer && req.referrer !== "about:client") headers.push(["referer", req.referrer]);
  const jarNow = await cookies();
  const cookie = Object.entries(jarNow).map(([k, v]) => `${k}=${v}`).join("; ");
  if (cookie) headers.push(["cookie", cookie]);
  const body = req.method === "GET" || req.method === "HEAD" ? new Uint8Array() : new Uint8Array(await req.arrayBuffer());
  let res;
  for (let attempt = 0; ; attempt++) {
    const ch = await channel();
    try {
      res = await ch.fetch(req.method, url.pathname + url.search, headers, body);
      break;
    } catch (e) {
      // A channel that died under us (a stopped daemon, a dropped relay):
      // once more on a new one.
      if (attempt > 0 || !ch.closed) throw e;
    }
  }
  stats.requests++;
  const out = new Headers();
  const setCookie: string[] = [];
  let encoding = "";
  for (const [k, v] of res.head.headers) {
    if (k === "set-cookie") setCookie.push(v);
    else if (k === "content-encoding") encoding = v;
    else if (k !== "content-length") {
      try {
        out.append(k, v);
      } catch {}
    }
  }
  keep(setCookie);
  let stream: ReadableStream<Uint8Array> = res.body;
  if (encoding === "gzip" || encoding === "deflate") stream = stream.pipeThrough(new DecompressionStream(encoding) as unknown as TransformStream<Uint8Array, Uint8Array>);
  const status = res.head.status;
  if (NULL_BODY.has(status) || req.method === "HEAD") {
    void stream.cancel().catch(() => {});
    return new Response(null, { status: status === 101 ? 502 : status, headers: out });
  }
  const html = (out.get("content-type") ?? "").startsWith("text/html");
  if (navigate && html) {
    out.append("content-security-policy", `frame-ancestors 'self' ${creds?.control ?? ""}`);
    const page = await new Response(stream).text();
    const at = page.search(/<head[\s>]/i);
    const end = at < 0 ? 0 : page.indexOf(">", at) + 1;
    return new Response(page.slice(0, end) + SHIM + page.slice(end), { status, headers: out });
  }
  return new Response(stream, { status, headers: out });
}

// ---- an app's own worker ------------------------------------------------
//
// A page in the block may register a worker of its own (VS Code's webviews
// do). That script's fetch never reaches a worker: it goes to control,
// which doesn't have it. So control answers with this worker and the app's
// path in S27_APP_SW, and this worker fetches the app's script through the
// channel and runs it inside, handing it the events: its fetch handlers go
// first, and what they leave goes through the channel as usual.

const APP = (self as unknown as { S27_APP_SW?: string }).S27_APP_SW;
type Handler = (e: unknown) => void;
const appHandlers: Record<string, Handler[]> = {};
let app: Promise<void> | null = null;

function loadApp(): Promise<void> {
  app ??= (async () => {
    const res = await tunnel(new Request(new URL(APP!, self.location.href)), false);
    if (!res.ok) throw new Error(`the app's worker: ${res.status}`);
    const code = await res.text();
    const add = (type: string, fn: Handler) => (appHandlers[type] ??= []).push(fn);
    const scope = new Proxy(self, {
      get(t, k) {
        if (k === "addEventListener") return add;
        const v = Reflect.get(t, k);
        return typeof v === "function" ? v.bind(t) : v;
      },
    });
    new Function("self", "addEventListener", code)(scope, add);
  })();
  return app;
}

function dispatchApp(type: string, e: unknown) {
  for (const h of appHandlers[type] ?? []) h(e);
}

if (APP) {
  for (const type of ["install", "activate"] as const) {
    self.addEventListener(type, (e) => {
      const waits: Promise<unknown>[] = [];
      e.waitUntil(loadApp().then(() => dispatchApp(type, { waitUntil: (p: Promise<unknown>) => waits.push(p) })).then(() => Promise.all(waits)));
    });
  }
  self.addEventListener("message", (e) => {
    if (e.data?.s27) return;
    e.waitUntil(loadApp().then(() => dispatchApp("message", e)));
  });
}

async function viaApp(e: FetchEvent): Promise<Response | undefined> {
  await loadApp();
  let answer: Promise<Response> | undefined;
  dispatchApp("fetch", {
    request: e.request,
    clientId: e.clientId,
    resultingClientId: e.resultingClientId,
    respondWith: (r: Response | Promise<Response>) => (answer = Promise.resolve(r)),
    waitUntil: (p: Promise<unknown>) => e.waitUntil(p),
  });
  return answer;
}

self.addEventListener("fetch", (e) => {
  const url = new URL(e.request.url);
  if (APP && !url.pathname.startsWith("/.s27/")) {
    e.respondWith(
      (async () => {
        const r = await viaApp(e).catch(() => undefined);
        if (r) return r;
        if (url.origin !== self.location.origin) return fetch(e.request);
        try {
          return await tunnel(e.request, e.request.mode === "navigate");
        } catch (err) {
          return new Response(`s27: ${err}`, { status: 502 });
        }
      })(),
    );
    return;
  }
  if (url.origin !== self.location.origin || url.pathname.startsWith("/.s27/")) return;
  const navigate = e.request.mode === "navigate";
  e.respondWith(
    (async () => {
      if (!(await getCreds())) {
        // No key here (storage cleared): control's bootstrap page gets one.
        return navigate ? fetch(e.request) : new Response("s27: this block has no key", { status: 503 });
      }
      try {
        return await tunnel(e.request, navigate);
      } catch (err) {
        stats.failures++;
        return new Response(`s27: ${err}`, { status: 502, headers: { "content-type": "text/plain" } });
      }
    })(),
  );
});
