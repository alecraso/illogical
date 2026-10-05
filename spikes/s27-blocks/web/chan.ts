// A block's Noise channel to its daemon (the browser's end of
// src/daemon.rs), and the streams inside it. Used by the worker (HTTP) and
// by the WebSocket shim in the block's pages.
//
// The handshake is the product's (Noise IK, web/src/e2e/noise.ts, prologue
// `illogical/1\n<daemon id>\n`), with the block's one-off key as the static
// key. The first transport message is the grant; the daemon answers
// {"ok"} or {"refused"} before anything else happens.

import { cat, Initiator, type Cipher } from "../../../web/src/e2e/noise.ts";

const CHUNK = 16 * 1024;
const enc = new TextEncoder();
const dec = new TextDecoder();

export interface Grant {
  daemon: string;
  block: string;
  key: string;
  expires: number;
  by: string;
  sig: string;
}

/** What the parent page hands a block. */
export interface Creds {
  daemon: { id: string; noise: string };
  /** The block key: PKCS#8 private half, and the public half (hex). */
  pkcs8: ArrayBuffer;
  pub: string;
  grant: Grant;
  /** "relay" (through control) or a direct `wss://…/e2e` URL. */
  route: string;
  /** Control's page origin, where renewals are asked for. */
  control: string;
}

export interface ResHead {
  status: number;
  headers: [string, string][];
}

export interface WsHandlers {
  onOpen(protocol: string): void;
  onMessage(text: boolean, data: Uint8Array): void;
  onClose(code: number, reason: string): void;
}

/** The daemon refused the grant (expired, revoked, unknown block). */
export class Refused extends Error {}

export const NativeWebSocket: typeof WebSocket = globalThis.WebSocket;

export function hex(b: Uint8Array): string {
  return Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");
}
export function unhex(s: string): Uint8Array<ArrayBuffer> {
  const out = new Uint8Array(s.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = parseInt(s.slice(2 * i, 2 * i + 2), 16);
  return out;
}

function frame(kind: string, sid: number, payload: Uint8Array = new Uint8Array()): Uint8Array {
  const f = new Uint8Array(5 + payload.length);
  f[0] = kind.charCodeAt(0);
  new DataView(f.buffer).setUint32(1, sid);
  f.set(payload, 5);
  return f;
}

export function relayUrl(creds: Creds, host: string): string {
  return creds.route === "relay" ? `wss://${host}/.s27/relay?d=${creds.daemon.id}` : creds.route;
}

interface Handler {
  frame(kind: string, payload: Uint8Array): void;
  fail(why: string): void;
}

export class BlockChannel {
  onclose: () => void = () => {};
  closed = false;
  /** The daemon's clock when the channel opened (for skew). */
  daemonNow = 0;
  private sendQ: Promise<void> = Promise.resolve();
  private recvQ: Promise<void> = Promise.resolve();
  private partial: Uint8Array[] = [];
  private handlers = new Map<number, Handler>();
  private next = 1;
  private ws!: WebSocket;
  private tx!: Cipher;
  private rx!: Cipher;
  private first: ((m: string) => void) | null = null;

  static async connect(url: string, creds: Creds, timeoutMs = 10_000): Promise<BlockChannel> {
    const c = new BlockChannel();
    const priv = await crypto.subtle.importKey("pkcs8", creds.pkcs8, { name: "X25519" }, false, ["deriveBits"]);
    const pub = await crypto.subtle.importKey("raw", unhex(creds.pub), { name: "X25519" }, true, []);
    const ik = new Initiator({ privateKey: priv, publicKey: pub }, unhex(creds.daemon.noise));
    const ws = new NativeWebSocket(url);
    ws.binaryType = "arraybuffer";
    c.ws = ws;
    const answer = await new Promise<string>((res, rej) => {
      const t = setTimeout(() => {
        ws.close();
        rej(new Error(`timed out: ${url}`));
      }, timeoutMs);
      let step = 0;
      ws.onopen = async () => {
        ws.send(await ik.write(new Uint8Array(), enc.encode(`illogical/1\n${creds.daemon.id}\n`)));
      };
      ws.onerror = () => {
        clearTimeout(t);
        rej(new Error(`couldn't connect: ${url}`));
      };
      // The daemon's answer (a refusal) can still be on its way through
      // the receive queue when the socket closes: read it first.
      ws.onclose = () =>
        void c.recvQ.then(() => {
          clearTimeout(t);
          rej(new Error("closed during the handshake"));
        });
      ws.onmessage = (e) => {
        const wire = new Uint8Array(e.data as ArrayBuffer);
        if (step++ === 0) {
          c.recvQ = c.recvQ.then(async () => {
            const { channel } = await ik.read(wire);
            c.tx = channel.send;
            c.rx = channel.recv;
            c.first = (m) => {
              clearTimeout(t);
              res(m);
            };
            c.put("T", enc.encode(JSON.stringify(creds.grant)));
          });
          c.recvQ.catch((err) => rej(err));
        } else c.take(wire);
      };
    });
    const a = JSON.parse(answer);
    if (a.refused) {
      ws.close();
      throw new Refused(a.refused);
    }
    c.daemonNow = a.now ?? 0;
    ws.onclose = () => c.close();
    ws.onerror = () => c.close();
    return c;
  }

  private take(wire: Uint8Array) {
    this.recvQ = this.recvQ
      .then(async () => {
        const plain = await this.rx.open(wire);
        this.partial.push(plain.subarray(1));
        if (plain[0] !== 0) return;
        const whole = this.partial.length === 1 ? this.partial[0] : cat(...this.partial);
        this.partial = [];
        const kind = String.fromCharCode(whole[0]);
        const rest = whole.subarray(1);
        if (kind === "T") {
          const f = this.first;
          this.first = null;
          f?.(dec.decode(rest));
        } else if (kind === "B") {
          const k = String.fromCharCode(rest[0]);
          const sid = new DataView(rest.buffer, rest.byteOffset).getUint32(1);
          const h = this.handlers.get(sid);
          h?.frame(k, rest.subarray(5));
        }
      })
      .catch(() => this.close());
  }

  private put(kind: "T" | "B", body: Uint8Array) {
    const plain = cat(enc.encode(kind), body);
    this.sendQ = this.sendQ
      .then(async () => {
        for (let o = 0; ; o += CHUNK) {
          const last = o + CHUNK >= plain.length;
          const wire = await this.tx.seal(cat(Uint8Array.of(last ? 0 : 1), plain.subarray(o, o + CHUNK)));
          if (this.ws.readyState === NativeWebSocket.OPEN) this.ws.send(wire);
          if (last) break;
        }
      })
      .catch(() => this.close());
  }

  private send(f: Uint8Array) {
    this.put("B", f);
  }

  close() {
    if (this.closed) return;
    this.closed = true;
    try {
      this.ws.close();
    } catch {}
    for (const h of this.handlers.values()) h.fail("the channel closed");
    this.handlers.clear();
    this.onclose();
  }

  /** An HTTP request; resolves at the response's head, its body streams. */
  fetch(method: string, path: string, headers: [string, string][], body: Uint8Array): Promise<{ head: ResHead; body: ReadableStream<Uint8Array> }> {
    const sid = this.next++;
    return new Promise((res, rej) => {
      if (this.closed) return rej(new Error("the channel closed"));
      let ctl: ReadableStreamDefaultController<Uint8Array>;
      const stream = new ReadableStream<Uint8Array>({
        start: (c) => {
          ctl = c;
        },
        cancel: () => {
          this.handlers.delete(sid);
          this.send(frame("K", sid));
        },
      });
      let headed = false;
      this.handlers.set(sid, {
        frame: (k, p) => {
          if (k === "R") {
            headed = true;
            res({ head: JSON.parse(dec.decode(p)), body: stream });
          } else if (k === "D") ctl.enqueue(p.slice());
          else if (k === "E") {
            this.handlers.delete(sid);
            ctl.close();
          } else if (k === "X") {
            this.handlers.delete(sid);
            const why = JSON.parse(dec.decode(p)).message as string;
            if (headed) ctl.error(new Error(why));
            else rej(new Error(why));
          }
        },
        fail: (why) => {
          if (headed) ctl.error(new Error(why));
          else rej(new Error(why));
        },
      });
      this.send(frame("H", sid, enc.encode(JSON.stringify({ method, path, headers }))));
      for (let o = 0; o < body.length; o += 60 * 1024) this.send(frame("D", sid, body.subarray(o, o + 60 * 1024)));
      this.send(frame("E", sid));
    });
  }

  /** A WebSocket to the block's port. */
  websocket(path: string, protocols: string[], h: WsHandlers): { send(text: boolean, data: Uint8Array): void; close(code: number, reason: string): void } {
    const sid = this.next++;
    let done = false;
    const finish = (code: number, reason: string) => {
      if (done) return;
      done = true;
      this.handlers.delete(sid);
      h.onClose(code, reason);
    };
    this.handlers.set(sid, {
      frame: (k, p) => {
        if (k === "A") h.onOpen(JSON.parse(dec.decode(p)).protocol ?? "");
        else if (k === "M") h.onMessage(p[0] === 0, p.subarray(1).slice());
        else if (k === "C") {
          const c = JSON.parse(dec.decode(p));
          finish(c.code || 1005, c.reason || "");
        } else if (k === "X") finish(1006, JSON.parse(dec.decode(p)).message);
      },
      fail: (why) => finish(1006, why),
    });
    this.send(frame("O", sid, enc.encode(JSON.stringify({ path, protocols }))));
    return {
      send: (text, data) => this.send(frame("M", sid, cat(Uint8Array.of(text ? 0 : 1), data))),
      close: (code, reason) => {
        this.send(frame("C", sid, enc.encode(JSON.stringify({ code, reason }))));
        finish(code, reason);
      },
    };
  }
}

// ---- where the worker and the pages keep the block's credentials -------

let opened: Promise<IDBDatabase> | null = null;

/** One connection, let go when the database is being deleted (storage
 * cleared), so the deletion isn't held up by us. */
function db(): Promise<IDBDatabase> {
  opened ??= new Promise((res, rej) => {
    const r = indexedDB.open("s27", 1);
    r.onupgradeneeded = () => r.result.createObjectStore("kv");
    r.onsuccess = () => {
      r.result.onversionchange = () => {
        r.result.close();
        opened = null;
      };
      res(r.result);
    };
    r.onerror = () => {
      opened = null;
      rej(r.error);
    };
  });
  return opened;
}

export async function load<T>(key: string): Promise<T | undefined> {
  const d = await db();
  return new Promise((res, rej) => {
    const r = d.transaction("kv").objectStore("kv").get(key);
    r.onsuccess = () => res(r.result as T | undefined);
    r.onerror = () => rej(r.error);
  });
}

export async function save(key: string, v: unknown): Promise<void> {
  const d = await db();
  return new Promise((res, rej) => {
    const t = d.transaction("kv", "readwrite");
    t.objectStore("kv").put(v, key);
    t.oncomplete = () => res();
    t.onerror = () => rej(t.error);
  });
}
