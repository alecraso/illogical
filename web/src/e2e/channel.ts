// An end-to-end channel to a daemon over a WebSocket (direct, or through
// control's relay): the browser's copy of `illogical_e2e::channel`.
//
// Inside it travel the WebSocket protocol's text and binary messages and
// HTTP requests, so a page reached through the relay works the same as
// one on the tailnet. WebCrypto is async, so sealing and opening each run
// in a strict queue: nonces must go out (and be read) in order.

import { cat, Initiator, type Cipher } from "./noise.ts";
import type { DeviceKeys } from "./keys.ts";
import { unhex } from "./cert.ts";
import { RelayMux, type SocketLike } from "./relaymux.ts";

const CHUNK = 16 * 1024;
const MAX_MSG = 64 << 20;
const enc = new TextEncoder();
const dec = new TextDecoder();

export interface RequestHead {
  method: string;
  path: string;
  content_type?: string;
  /** The answer may come in parts (channel.rs); the page never asks. */
  stream?: boolean;
}
export interface ResponseHead {
  status: number;
  content_type?: string;
  /** More parts follow: only for a request with `stream`. */
  more?: boolean;
}

export type Msg =
  | { kind: "text"; text: string }
  | { kind: "binary"; data: Uint8Array }
  | { kind: "request"; id: number; head: RequestHead; body: Uint8Array }
  | { kind: "response"; id: number; head: ResponseHead; body: Uint8Array };

export function encodeMsg(m: Msg): Uint8Array {
  const headed = (k: string, id: number, head: unknown, body: Uint8Array) => {
    const h = enc.encode(JSON.stringify(head));
    const pre = new Uint8Array(9);
    pre[0] = k.charCodeAt(0);
    const dv = new DataView(pre.buffer);
    dv.setUint32(1, id);
    dv.setUint32(5, h.length);
    return cat(pre, h, body);
  };
  switch (m.kind) {
    case "text":
      return cat(enc.encode("T"), enc.encode(m.text));
    case "binary":
      return cat(enc.encode("B"), m.data);
    case "request":
      return headed("Q", m.id, m.head, m.body);
    case "response":
      return headed("R", m.id, m.head, m.body);
  }
}

export function decodeMsg(b: Uint8Array): Msg {
  const kind = String.fromCharCode(b[0]);
  const rest = b.subarray(1);
  if (kind === "T") return { kind: "text", text: dec.decode(rest) };
  if (kind === "B") return { kind: "binary", data: rest };
  if (kind === "Q" || kind === "R") {
    const dv = new DataView(rest.buffer, rest.byteOffset, rest.byteLength);
    const id = dv.getUint32(0);
    const n = dv.getUint32(4);
    const head = JSON.parse(dec.decode(rest.subarray(8, 8 + n)));
    const body = rest.subarray(8 + n);
    return kind === "Q" ? { kind: "request", id, head, body } : { kind: "response", id, head, body };
  }
  throw new Error(`unknown message kind ${kind}`);
}

export interface DaemonRef {
  /** Its device id (in the prologue). */
  id: string;
  /** Its Noise public key, hex, from a certificate this browser checked. */
  noise: string;
}

export interface Response {
  status: number;
  ok: boolean;
  contentType?: string;
  body: Uint8Array;
  json<T = unknown>(): T;
  text(): string;
}

/** A way to a daemon: a WebSocket URL, or (`mux`) a channel inside the
 * page's one socket to control's relay (M25). */
export interface Route {
  url: string;
  timeoutMs: number;
  /** `wss://control…/api/relay/m`: reach the daemon through it, as a
   * channel, instead of a socket of its own at `url`. */
  mux?: string;
  /** Only if a `mux` route before it couldn't reach control at all (an
   * older control without the shared socket). */
  ifNoMux?: boolean;
}

function openRoute(r: Route, id: string): Promise<SocketLike> {
  if (r.mux) return RelayMux.for(r.mux).open(id, r.timeoutMs);
  return openSocket(r.url, r.timeoutMs) as Promise<unknown> as Promise<SocketLike>;
}

function openSocket(url: string, timeoutMs: number): Promise<WebSocket> {
  return new Promise((res, rej) => {
    const ws = new WebSocket(url);
    ws.binaryType = "arraybuffer";
    const t = setTimeout(() => {
      ws.close();
      rej(new Error(`timed out: ${url}`));
    }, timeoutMs);
    ws.onopen = () => {
      clearTimeout(t);
      res(ws);
    };
    ws.onerror = () => {
      clearTimeout(t);
      rej(new Error(`couldn't connect: ${url}`));
    };
  });
}

export class E2ESocket {
  onText: (t: string) => void = () => {};
  onBinary: (b: Uint8Array) => void = () => {};
  onClose: () => void = () => {};
  /** Which way it went: the first URL tried, or a later one (the relay). */
  readonly url: string;

  private sendQ: Promise<void> = Promise.resolve();
  private recvQ: Promise<void> = Promise.resolve();
  private partial: Uint8Array[] = [];
  private partialLen = 0;
  private nextId = 1;
  private pending = new Map<number, { res: (r: Response) => void; rej: (e: Error) => void }>();
  private closed = false;

  private ws: SocketLike;
  private send_: Cipher;
  private recv: Cipher;

  private constructor(ws: SocketLike, send: Cipher, recv: Cipher, early: Uint8Array[]) {
    this.ws = ws;
    this.send_ = send;
    this.recv = recv;
    this.url = ws.url;
    const take = (wire: Uint8Array) => {
      this.recvQ = this.recvQ.then(() => this.take(wire)).catch(() => this.close());
    };
    // What the daemon sent right after its handshake message (its hello)
    // arrived while we were still finishing ours.
    for (const w of early) take(w);
    ws.onmessage = (e) => take(new Uint8Array(e.data as ArrayBuffer));
    ws.onclose = () => this.close();
  }

  /** Try each URL in order (direct ones first, the relay last). */
  static async connect(urls: Route[], daemon: DaemonRef, keys: DeviceKeys): Promise<E2ESocket> {
    let last: unknown = new Error("no way to reach it");
    let refused = false;
    for (const route of urls) {
      if (route.ifNoMux && refused) continue;
      try {
        const ws = await openRoute(route, daemon.id);
        try {
          return await E2ESocket.handshake(ws, daemon, keys, route.timeoutMs);
        } catch (e) {
          ws.close();
          throw e;
        }
      } catch (e) {
        last = e;
        if (route.mux && (e as { refused?: boolean }).refused) refused = true;
      }
    }
    throw last;
  }

  private static handshake(ws: SocketLike, daemon: DaemonRef, keys: DeviceKeys, timeoutMs: number): Promise<E2ESocket> {
    return new Promise((res, rej) => {
      const ik = new Initiator(keys.noise, unhex(daemon.noise));
      const t = setTimeout(() => rej(new Error("handshake timed out")), Math.max(timeoutMs, 5000));
      // Every message from the first on is kept: the daemon's first
      // transport message can arrive before we've finished reading its
      // handshake reply, and dropping it would skip a nonce.
      const early: Uint8Array[] = [];
      let first = true;
      ws.onmessage = async (e) => {
        const wire = new Uint8Array(e.data as ArrayBuffer);
        if (!first) return void early.push(wire);
        first = false;
        try {
          const { channel } = await ik.read(wire);
          clearTimeout(t);
          res(new E2ESocket(ws, channel.send, channel.recv, early));
        } catch (err) {
          clearTimeout(t);
          rej(err);
        }
      };
      ws.onclose = () => {
        clearTimeout(t);
        rej(new Error("closed during the handshake (not an approved device?)"));
      };
      void ik
        .write(new Uint8Array(), enc.encode(`illogical/1\n${daemon.id}\n`))
        .then((m) => ws.send(m))
        .catch(rej);
    });
  }

  get open(): boolean {
    return !this.closed && this.ws.readyState === WebSocket.OPEN;
  }

  private async take(wire: Uint8Array) {
    const plain = await this.recv.open(wire);
    const more = plain[0] !== 0;
    this.partial.push(plain.subarray(1));
    this.partialLen += plain.length - 1;
    if (this.partialLen > MAX_MSG) throw new Error("message too large");
    if (more) return;
    const whole = this.partial.length === 1 ? this.partial[0] : cat(...this.partial);
    this.partial = [];
    this.partialLen = 0;
    const m = decodeMsg(whole);
    if (m.kind === "text" || m.kind === "binary") {
      if (this.held) this.held.push(m);
      else this.deliver(m);
    } else if (m.kind === "response") {
      const done = this.pending.get(m.id);
      this.pending.delete(m.id);
      const body = m.body;
      done?.res({
        status: m.head.status,
        ok: m.head.status >= 200 && m.head.status < 300,
        contentType: m.head.content_type,
        body,
        json: () => JSON.parse(dec.decode(body)),
        text: () => dec.decode(body),
      });
    }
  }

  /** Protocol messages that came before `start`. */
  private held: Msg[] | null = [];

  private deliver(m: Msg) {
    if (m.kind === "text") this.onText(m.text);
    else if (m.kind === "binary") this.onBinary(m.data);
  }

  /** Set `onText` and `onBinary`, then call this: messages that arrived
   * before (the daemon's hello) are delivered now, in order. Requests work
   * without it. */
  start() {
    const held = this.held ?? [];
    this.held = null;
    for (const m of held) this.deliver(m);
  }

  private put(m: Msg) {
    if (!this.open) return;
    const plain = encodeMsg(m);
    this.sendQ = this.sendQ
      .then(async () => {
        for (let o = 0; o < plain.length || o === 0; o += CHUNK) {
          const last = o + CHUNK >= plain.length;
          const chunk = cat(Uint8Array.of(last ? 0 : 1), plain.subarray(o, o + CHUNK));
          const wire = await this.send_.seal(chunk);
          if (this.ws.readyState === WebSocket.OPEN) this.ws.send(wire);
          if (last) break;
        }
      })
      .catch(() => this.close());
  }

  sendText(text: string) {
    this.put({ kind: "text", text });
  }

  sendBinary(data: Uint8Array) {
    this.put({ kind: "binary", data });
  }

  /** An HTTP request to the daemon's API, through the channel. */
  request(method: string, path: string, body?: unknown, timeoutMs = 30_000): Promise<Response> {
    const id = this.nextId++;
    // Raw bytes (an upload's chunk, M70) go as they are.
    const raw = body instanceof Uint8Array ? body : undefined;
    const json = body === undefined || raw ? undefined : JSON.stringify(body);
    return new Promise((res, rej) => {
      if (!this.open) return rej(new Error("not connected"));
      const t = setTimeout(() => {
        this.pending.delete(id);
        rej(new Error("request timed out"));
      }, timeoutMs);
      this.pending.set(id, {
        res: (r) => {
          clearTimeout(t);
          res(r);
        },
        rej: (e) => {
          clearTimeout(t);
          rej(e);
        },
      });
      this.put({
        kind: "request",
        id,
        head: {
          method,
          path,
          ...(raw ? { content_type: "application/octet-stream" } : json === undefined ? {} : { content_type: "application/json" }),
        },
        body: raw ?? (json === undefined ? new Uint8Array() : enc.encode(json)),
      });
    });
  }

  close() {
    if (this.closed) return;
    this.closed = true;
    this.ws.close();
    for (const p of this.pending.values()) p.rej(new Error("connection closed"));
    this.pending.clear();
    this.onClose();
  }
}
