// The spike's processes, without Playwright (tests/stack.ts and
// safari/safari.ts both use them): control, a daemon dialed in to its
// relay, and a plain test server standing in for a block's port.

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

export function startS27(args: string[]): Promise<{ proc: ChildProcess; info: Record<string, string> }> {
  const proc = spawn(process.env.S27_BIN ?? "target/release/s27", args, { stdio: ["ignore", "pipe", "inherit"] });
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
export function testSite(): Promise<{ port: number; close(): void }> {
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
  if (!r.ok) throw new Error(`${path}: ${r.status}`);
  const text = await r.text();
  return text ? JSON.parse(text) : null;
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

/** Start control (on `controlListen`, default any port), the daemon and
 * the test server. */
export async function startStack(controlListen = "127.0.0.1:0"): Promise<Stack & { stop(): void }> {
  // A stack running elsewhere (netem.sh: control and the daemon in their
  // own containers), as JSON.
  if (process.env.S27_STACK) {
    const s = JSON.parse(process.env.S27_STACK) as Stack;
    return { ...s, async restartControl() { throw new Error("not with S27_STACK"); }, stop() {} };
  }
  const dir = ".run/cert";
  const ctl = (listen: string, dial: string) => ["control", "--listen", listen, "--dial-listen", dial, "--dir", dir, "--web", "dist", "--marker", MARKER];
  let control = await startS27(ctl(controlListen, "127.0.0.1:0"));
  const dial = control.info.dial;
  const controlPort = Number(control.info.listen.split(":")[1]);
  const daemon = await startS27(["daemon", "--relay", `ws://${dial}/relay/dial`, "--admin", "127.0.0.1:0", "--direct", "127.0.0.1:0", "--dir", dir]);
  const site = await testSite();
  const stack = {
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
      control = await startS27(ctl(`127.0.0.1:${controlPort}`, dial));
    },
    stop() {
      control.proc.kill();
      daemon.proc.kill();
      site.close();
    },
  };
  for (let i = 0; !(await controlStats(stack)).daemons.includes(stack.daemon.id); i++) {
    if (i > 100) throw new Error("the daemon didn't dial in");
    await new Promise((r) => setTimeout(r, 100));
  }
  return stack;
}
