// M52 from phones (testnet/test.sh's m52 claim): a Pixel 7 in Chrome and an
// iPhone in WebKit sign in to the test stack's control as the approving
// device's account (device.ts, kept in --state), are approved by it as new
// devices, open the joined box's pane and type a marker into it, with ssh
// out of the picture. Then `docker restart` of the box (--restart): the
// same pages, still open, reach it again through the relay and a second
// marker is typed. Each marker is read back from the box's pane by the
// device over the relay, not by the phone that typed it.
//
//   node --experimental-strip-types m52-phones.ts --state FILE --box NAME \
//     --restart CONTAINER --marker PREFIX
//
// Control's public URL is a container's address, and a page there wouldn't
// be a secure context (no WebCrypto), so the phones load it as
// http://127.0.0.1:PORT through a small proxy here: it forwards to where
// the device reaches control (its `via`), rewrites the Origin control
// checks and the URLs control hands out to its own, and stands the fake
// GitHub's sign-in under /__github. Exits 0 when both markers came back on
// both phones, 1 (saying why) otherwise.

import { execFileSync } from "node:child_process";
import { createServer, request, type IncomingHttpHeaders, type Server } from "node:http";
import { connect } from "node:net";
import { chromium, devices, webkit, type Browser, type BrowserContextOptions, type Page } from "@playwright/test";
import { Device } from "./device.ts";

const args = process.argv.slice(2);
const opt = (name: string): string => {
  const i = args.indexOf(name);
  if (i < 0) throw new Error(`${name} is required`);
  return args[i + 1];
};
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
const log = (s: string) => console.error(`[m52-phones] ${s}`);

async function until<T>(what: string, f: () => Promise<T | null | undefined | false>, timeoutMs: number): Promise<T> {
  const end = Date.now() + timeoutMs;
  let last: unknown;
  for (;;) {
    try {
      const v = await f();
      if (v) return v;
    } catch (e) {
      last = e;
    }
    if (Date.now() > end) throw new Error(`timed out (${timeoutMs / 1000}s) waiting for ${what}${last ? `: ${last}` : ""}`);
    await sleep(250);
  }
}

/** Control as http://127.0.0.1:PORT, for a page. */
async function proxy(d: Device): Promise<{ server: Server; local: string }> {
  const pub = new URL(d.control).origin;
  const real = new URL(d.reach(d.control));
  const ghPub = Object.keys(d.via).find((k) => k !== d.control && k !== pub) ?? "http://fakes:9001";
  const gh = new URL(d.reach(ghPub));
  let local = "";
  const fix = (v: string) => v.replaceAll(pub, local).replaceAll(ghPub, `${local}/__github`);
  const headers = (h: IncomingHttpHeaders): IncomingHttpHeaders => {
    const out = { ...h };
    if (out.origin === local) out.origin = pub;
    if (typeof out.referer === "string") out.referer = out.referer.replace(local, pub);
    out.host = new URL(pub).host;
    return out;
  };
  const server = createServer((req, res) => {
    let path = req.url!;
    let to = real;
    if (path.startsWith("/__github/")) {
      // The fake GitHub signs in whoever the authorize URL names.
      const u = new URL(path.slice("/__github".length), ghPub);
      if (u.pathname === "/login/oauth/authorize") {
        u.searchParams.set("login", d.login);
        u.searchParams.set("redirect_uri", u.searchParams.get("redirect_uri")!.replace(local, pub));
      }
      path = u.pathname + u.search;
      to = gh;
    }
    const up = request({ host: to.hostname, port: to.port, method: req.method, path, headers: headers(req.headers) }, (r) => {
      const h = { ...r.headers };
      if (typeof h.location === "string") h.location = fix(h.location);
      if (path.split("?")[0] === "/control.json") {
        const chunks: Buffer[] = [];
        r.on("data", (c: Buffer) => chunks.push(c));
        r.on("end", () => {
          const body = fix(Buffer.concat(chunks).toString());
          delete h["content-length"];
          res.writeHead(r.statusCode!, h).end(body);
        });
        return;
      }
      res.writeHead(r.statusCode!, h);
      r.pipe(res);
    });
    up.on("error", (e) => res.writeHead(502).end(String(e)));
    req.pipe(up);
  });
  // The relay's websockets: the same, raw after the request's head.
  server.on("upgrade", (req, sock, head) => {
    const up = connect(Number(real.port), real.hostname, () => {
      const h = headers(req.headers);
      const lines = [`${req.method} ${req.url} HTTP/1.1`];
      for (const [k, v] of Object.entries(h)) for (const one of Array.isArray(v) ? v : [v]) if (one !== undefined) lines.push(`${k}: ${one}`);
      up.write(`${lines.join("\r\n")}\r\n\r\n`);
      up.write(head);
      up.pipe(sock).pipe(up);
    });
    up.on("error", () => sock.destroy());
    sock.on("error", () => up.destroy());
  });
  await new Promise<void>((r) => server.listen(0, "127.0.0.1", r));
  const addr = server.address();
  local = `http://127.0.0.1:${typeof addr === "object" && addr ? addr.port : 0}`;
  return { server, local };
}

const phoneOf = (name: string): BrowserContextOptions => {
  const { viewport, userAgent, deviceScaleFactor, isMobile, hasTouch } = devices[name];
  return { viewport, userAgent, deviceScaleFactor, isMobile, hasTouch };
};

interface Phone {
  name: string;
  page: Page;
}

/** Sign a phone in, have the device approve it, and wait until it's in. */
async function signIn(d: Device, browser: Browser, name: string, options: BrowserContextOptions, local: string): Promise<Phone> {
  const ctx = await browser.newContext({ ...options, baseURL: local });
  const page = await ctx.newPage();
  page.on("pageerror", (e) => log(`${name}: page error: ${e.message}`));
  await page.goto("/");
  await page.locator("[data-signin=github]").tap();
  await until(`${name} to ask to be approved`, () => page.evaluate(() => window.__arugula?.control?.phase === "waiting"), 30_000);
  const asked = await page.evaluate(() => (window.__arugula.control as unknown as { request: unknown }).request);
  await d.approveRequest(asked as Parameters<Device["approveRequest"]>[0]);
  await until(`${name} to be approved`, () => page.evaluate(() => window.__arugula.control?.phase === "ready"), 30_000);
  log(`${name}: signed in and approved`);
  return { name, page };
}

/** The box shown on a phone, connected; its active pane once drawn. */
async function open(p: Phone, box: string, timeoutMs: number): Promise<number> {
  const { page } = p;
  await until(
    `${p.name} to list ${box}`,
    async () => (await page.evaluate(() => window.__arugula.control?.refresh()), page.evaluate((b) => window.__arugula.hosts.names.includes(b), box)),
    timeoutMs,
  );
  await page.evaluate((b) => window.__arugula.hosts.select(b), box);
  await until(
    `${p.name} connected to ${box}`,
    () => page.evaluate((b) => window.__arugula.hosts.current === b && window.__arugula.client.connected && !!window.__arugula.client.state, box),
    timeoutMs,
  );
  const pane = await until(`a pane on ${p.name}`, () => page.evaluate(() => window.__arugula.client.active()), timeoutMs);
  await until(`${p.name}'s pane ${pane} to draw`, () => page.evaluate((x) => window.__arugula.offset(x) !== null, pane), timeoutMs);
  return pane;
}

/** Tap the pane and type a command into it on the phone's keyboard; the
 * answer on the phone, then in the box's pane as the device reads it. */
async function typeMarker(d: Device, boxId: string, p: Phone, pane: number, marker: string) {
  const { page } = p;
  await page.locator(`[data-pane="${pane}"]`).tap({ position: { x: 40, y: 40 } });
  await page.keyboard.type(`echo ${marker}-$((6*7))\n`, { delay: 5 });
  const want = `${marker}-42`;
  await until(`${want} on ${p.name}`, () => page.evaluate(([x, w]) => window.__arugula.text(x).includes(w), [pane, want] as const), 20_000);
  await until(`${want} in ${boxId}'s pane ${pane}, read by the device`, async () => (await d.capture(boxId, pane)).includes(want), 20_000);
  log(`${p.name}: ${want} typed, and in the box's pane ${pane}`);
}

async function main() {
  const d = await Device.load(opt("--state"));
  const boxName = opt("--box");
  const container = opt("--restart");
  const marker = opt("--marker");
  const box = await d.waitOnline(boxName, 30_000);
  const { server, local } = await proxy(d);
  const browsers: Browser[] = [];
  try {
    browsers.push(await chromium.launch({ channel: process.env.E2E_CHROMIUM ? undefined : "chrome" }));
    browsers.push(await webkit.launch());
    const phones = [
      await signIn(d, browsers[0], "Pixel 7", phoneOf("Pixel 7"), local),
      await signIn(d, browsers[1], "iPhone", phoneOf("iPhone 15"), local),
    ];
    for (const [i, p] of phones.entries()) await typeMarker(d, box.id, p, await open(p, boxName, 60_000), `${marker}-PHONE${i}-A`);

    const started = execFileSync("docker", ["inspect", "-f", "{{.State.StartedAt}}", container], { encoding: "utf8" });
    execFileSync("docker", ["restart", container], { stdio: "ignore" });
    if (execFileSync("docker", ["inspect", "-f", "{{.State.StartedAt}}", container], { encoding: "utf8" }) === started) throw new Error(`${container} didn't restart`);
    log(`${container} restarted`);
    // The same pages: they find the box again through the relay by
    // themselves, once its daemon is back.
    await d.waitOnline(boxName, 90_000);
    for (const [i, p] of phones.entries()) await typeMarker(d, box.id, p, await open(p, boxName, 90_000), `${marker}-PHONE${i}-B`);
    console.log(JSON.stringify({ box: box.id, phones: phones.map((p) => p.name) }));
  } finally {
    for (const b of browsers) await b.close().catch(() => {});
    server.closeAllConnections();
    server.close();
  }
}

main().then(
  () => process.exit(0),
  (e: Error) => {
    console.error(`m52-phones: ${e.message}`);
    process.exit(1);
  },
);
