// The spike's stack for one worker: control, a daemon dialed in to its
// relay, and a plain test server standing in for a block's port. Tests get
// control's page (`parent`) with its device key trusted by the daemon.

import { test as base, expect, type Frame, type Page } from "@playwright/test";
import { spawn, type ChildProcess } from "node:child_process";
import { createHash } from "node:crypto";
import http from "node:http";
import net from "node:net";
import { createInterface } from "node:readline";

export const MARKER = "S27-PLAINTEXT-MARKER";

export interface Stack {
  controlPort: number;
  origin: string;
  daemon: { id: string; noise: string };
  admin: string;
  directPort: number;
  /** The test server's port. */
  site: number;
  restartControl(): Promise<void>;
}

function startS27(args: string[]): Promise<{ proc: ChildProcess; info: Record<string, string> }> {
  const proc = spawn("target/release/s27", args, { stdio: ["ignore", "pipe", "inherit"] });
  return new Promise((res, rej) => {
    const rl = createInterface({ input: proc.stdout! });
    rl.once("line", (l) => res({ proc, info: JSON.parse(l) }));
    proc.once("exit", (c) => rej(new Error(`s27 ${args[0]} exited ${c}`)));
  });
}

export function freePort(): Promise<number> {
  return new Promise((res) => {
    const s = net.createServer();
    s.listen(0, "127.0.0.1", () => {
      const p = (s.address() as net.AddressInfo).port;
      s.close(() => res(p));
    });
  });
}

/** The block's port, for the plain checks: pages, sizes, cookies, echo,
 * and a WebSocket echo at /ws. Every page carries the marker. */
function testSite(): Promise<{ port: number; close(): void }> {
  const page = (title: string) =>
    `<!doctype html><html><head><title>${title}</title></head><body><h1 id=h>${title}</h1><p>${MARKER.repeat(20)}</p></body></html>`;
  const server = http.createServer((req, res) => {
    const u = new URL(req.url ?? "/", "http://x");
    if (u.pathname === "/small") return res.writeHead(200, { "content-type": "text/plain" }).end("ok");
    if (u.pathname === "/big") {
      const n = Number(u.searchParams.get("n") ?? 1 << 20);
      return res.writeHead(200, { "content-type": "application/octet-stream" }).end(Buffer.alloc(n, 120));
    }
    if (u.pathname === "/echo") {
      const chunks: Buffer[] = [];
      req.on("data", (c) => chunks.push(c));
      req.on("end", () => res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify({ method: req.method, body: Buffer.concat(chunks).toString(), host: req.headers.host, origin: req.headers.origin ?? null })));
      return;
    }
    if (u.pathname === "/cookie/set") return res.writeHead(200, { "set-cookie": "s27=yes; Path=/; HttpOnly", "content-type": "text/plain" }).end("set");
    if (u.pathname === "/cookie/get") return res.writeHead(200, { "content-type": "text/plain" }).end(req.headers.cookie ?? "");
    if (u.pathname === "/redirect") return res.writeHead(302, { location: `http://localhost:${(server.address() as net.AddressInfo).port}/landed` }).end();
    res.writeHead(200, { "content-type": "text/html" }).end(page(u.pathname === "/" ? "block home" : u.pathname.slice(1)));
  });
  // A WebSocket echo, by hand (no dependency).
  server.on("upgrade", (req, sock) => {
    const key = createHash("sha1").update(`${req.headers["sec-websocket-key"]}258EAFA5-E914-47DA-95CA-C5AB0DC85B11`).digest("base64");
    sock.write(`HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: ${key}\r\n\r\n`);
    let buf = Buffer.alloc(0);
    sock.on("data", (d: Buffer) => {
      buf = Buffer.concat([buf, d]);
      while (buf.length >= 2) {
        const op = buf[0] & 15;
        let len = buf[1] & 127;
        let o = 2;
        if (len === 126) {
          len = buf.readUInt16BE(2);
          o = 4;
        } else if (len === 127) {
          len = Number(buf.readBigUInt64BE(2));
          o = 10;
        }
        if (buf.length < o + 4 + len) return;
        const mask = buf.subarray(o, o + 4);
        const data = Buffer.from(buf.subarray(o + 4, o + 4 + len).map((b, i) => b ^ mask[i % 4]));
        buf = buf.subarray(o + 4 + len);
        if (op === 8) return void sock.end(Buffer.from([0x88, 0]));
        const head = data.length < 126 ? Buffer.from([0x80 | op, data.length]) : Buffer.from([0x80 | op, 126, data.length >> 8, data.length & 255]);
        sock.write(Buffer.concat([head, data]));
      }
    });
    sock.on("error", () => {});
  });
  return new Promise((res) => server.listen(0, "127.0.0.1", () => res({ port: (server.address() as net.AddressInfo).port, close: () => server.close() })));
}

export async function admin(stack: Stack, path: string, body?: unknown): Promise<any> {
  const r = await fetch(`http://${stack.admin}${path}`, {
    method: body === undefined ? "GET" : "POST",
    headers: { "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (r.status === 204) return null;
  return r.json();
}

/** Control's counters, read the way a test would (not through a browser). */
export async function controlStats(stack: Stack): Promise<any> {
  const tls = await import("node:tls");
  return new Promise((res, rej) => {
    const s = tls.connect({ host: "127.0.0.1", port: stack.controlPort, servername: "control.test", rejectUnauthorized: false }, () => {
      s.write(`GET /.s27/stats HTTP/1.1\r\nHost: control.test:${stack.controlPort}\r\nConnection: close\r\n\r\n`);
    });
    let out = "";
    s.on("data", (d) => (out += d));
    s.on("end", () => res(JSON.parse(out.slice(out.indexOf("\r\n\r\n") + 4))));
    s.on("error", rej);
  });
}

let nextBlock = 1;

export interface Opened {
  origin: string;
  frame: Frame;
  block: string;
}

/** Open a block on `port` in control's page; resolves once the page is
 * there through the worker (its <h1>, or `ready`). */
export async function openBlock(
  page: Page,
  stack: Stack,
  o: { port?: number; ttlMs?: number; route?: string; path?: string; ready?: string } = {},
): Promise<Opened> {
  const block = `blk${nextBlock++}`;
  await admin(stack, "/block", { id: block, port: o.port ?? stack.site });
  const origin = await page.evaluate(
    (a) => (window as any).s27.openBlock(a).origin as string,
    { daemon: stack.daemon, block, ttlMs: o.ttlMs, route: o.route, path: o.path },
  );
  const frame = await frameAt(page, origin);
  await frame.locator(o.ready ?? "h1").first().waitFor({ timeout: 60_000 });
  return { origin, frame, block };
}

export async function frameAt(page: Page, origin: string): Promise<Frame> {
  await expect.poll(() => page.frames().some((f) => f.url().startsWith(origin)), { timeout: 30_000 }).toBe(true);
  return page.frames().find((f) => f.url().startsWith(origin))!;
}

/** The worker's own counters, asked from a page it controls. */
export function workerStats(frame: Frame): Promise<any> {
  return frame.evaluate(
    () =>
      new Promise((res) => {
        const ch = new MessageChannel();
        ch.port1.onmessage = (e) => res(e.data);
        navigator.serviceWorker.controller!.postMessage({ s27: "stats" }, [ch.port2]);
      }),
  );
}

export const test = base.extend<{ parent: Page }, { stack: Stack }>({
  stack: [
    async ({}, use) => {
      const dir = ".run/cert";
      const ctlArgs = (dial: string) => ["control", "--listen", "127.0.0.1:0", "--dial-listen", dial, "--dir", dir, "--web", "dist", "--marker", MARKER];
      let control = await startS27(ctlArgs("127.0.0.1:0"));
      const dial = control.info.dial;
      const controlPort = Number(control.info.listen.split(":")[1]);
      const daemon = await startS27(["daemon", "--relay", `ws://${dial}/relay/dial`, "--admin", "127.0.0.1:0", "--direct", "127.0.0.1:0", "--dir", dir]);
      const site = await testSite();
      const stack: Stack = {
        controlPort,
        origin: `https://control.test:${controlPort}`,
        daemon: { id: daemon.info.id, noise: daemon.info.noise },
        admin: daemon.info.admin,
        directPort: Number(daemon.info.direct.split(":")[1]),
        site: site.port,
        // Control goes away and comes back on the same ports (a deploy).
        async restartControl() {
          control.proc.kill();
          await new Promise((r) => control.proc.once("exit", r));
          control = await startS27(["control", "--listen", `127.0.0.1:${controlPort}`, "--dial-listen", dial, "--dir", dir, "--web", "dist", "--marker", MARKER]);
        },
      };
      await expect.poll(async () => (await controlStats(stack)).daemons, { timeout: 10_000 }).toContain(stack.daemon.id);
      await use(stack);
      control.proc.kill();
      daemon.proc.kill();
      site.close();
    },
    { scope: "worker" },
  ],
  parent: async ({ page, stack }, use) => {
    await page.goto(stack.origin + "/");
    await page.waitForFunction(() => (window as any).s27?.ready);
    const sign = await page.evaluate(() => (window as any).s27.signPub as string);
    await admin(stack, "/trust", { sign });
    await use(page);
  },
});

export { expect };

/** Keep a measurement (with the machine's load, which skews timings) in
 * .run/results.jsonl, and print it. */
export async function record(what: string, browser: string, data: Record<string, unknown>) {
  const { appendFileSync, mkdirSync } = await import("node:fs");
  const os = await import("node:os");
  const line = { what, browser, at: new Date().toISOString(), load: os.loadavg()[0].toFixed(1), cores: os.cpus().length, ...data };
  mkdirSync(".run", { recursive: true });
  appendFileSync(".run/results.jsonl", JSON.stringify(line) + "\n");
  console.log(JSON.stringify(line));
}

/** Open today's path for comparison: the daemon's own block site, framed
 * by control's page, no worker. */
export async function openDirect(page: Page, stack: Stack, o: { port?: number; ready?: string; path?: string } = {}): Promise<Opened> {
  const block = `blk${nextBlock++}`;
  await admin(stack, "/block", { id: block, port: o.port ?? stack.site });
  await page.evaluate((a) => (window as any).s27.openDirect(a.block, a.port, a.path), { block, port: stack.directPort, path: o.path });
  const origin = `https://b-${block}.direct.test:${stack.directPort}`;
  const frame = await frameAt(page, origin);
  await frame.locator(o.ready ?? "h1").first().waitFor({ timeout: 60_000 });
  return { origin, frame, block };
}
