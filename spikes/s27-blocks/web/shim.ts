// The WebSocket shim, first in every page of a block (the worker puts it
// there). A worker can't carry WebSockets, so a socket to the block's own
// host goes over a Noise channel this page opens itself; any other socket
// is a real one. The page's channel, not the worker's: the browser stops an
// idle worker after about 30 s, which would cut a hot-reload socket that
// sits quiet for minutes.
//
// It also relays renewals: when the worker's grant has expired it asks its
// pages, and a page asks the parent.

import { BlockChannel, type Creds, load, NativeWebSocket, Refused, relayUrl, save } from "./chan.ts";
import { askParent, giveWorker } from "./page.ts";

let chan: Promise<BlockChannel> | null = null;
const shimStats = { sockets: 0, connects: 0, renewals: 0 };
(window as unknown as { s27shim: unknown }).s27shim = shimStats;

async function renew(control: string): Promise<Creds> {
  shimStats.renewals++;
  const c = await askParent(control, "renew");
  await save("creds", c);
  await giveWorker(c);
  return c;
}

navigator.serviceWorker?.addEventListener("message", async (e) => {
  if (e.data?.s27 !== "renew") return;
  const c = await load<Creds>("creds");
  if (c) await renew(c.control).catch(() => {});
});

async function connect(): Promise<BlockChannel> {
  let c = await load<Creds>("creds");
  if (!c) throw new Error("this block has no key");
  try {
    return await BlockChannel.connect(relayUrl(c, location.host), c);
  } catch (e) {
    if (!(e instanceof Refused && /expired/.test(e.message))) throw e;
    c = await renew(c.control);
    return BlockChannel.connect(relayUrl(c, location.host), c);
  }
}

function channel(): Promise<BlockChannel> {
  if (!chan) {
    const p = connect().then((ch) => {
      shimStats.connects++;
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

const enc = new TextEncoder();
const dec = new TextDecoder();

class ShimSocket extends EventTarget {
  static readonly CONNECTING = 0;
  static readonly OPEN = 1;
  static readonly CLOSING = 2;
  static readonly CLOSED = 3;
  readonly CONNECTING = 0;
  readonly OPEN = 1;
  readonly CLOSING = 2;
  readonly CLOSED = 3;
  readyState = 0;
  protocol = "";
  extensions = "";
  bufferedAmount = 0;
  binaryType: BinaryType = "blob";
  readonly url: string;
  onopen: ((e: Event) => void) | null = null;
  onmessage: ((e: MessageEvent) => void) | null = null;
  onclose: ((e: CloseEvent) => void) | null = null;
  onerror: ((e: Event) => void) | null = null;
  private h: ReturnType<BlockChannel["websocket"]> | null = null;
  private sendQ: Promise<void> = Promise.resolve();

  constructor(url: URL, protocols: string[]) {
    super();
    this.url = url.href;
    shimStats.sockets++;
    channel()
      .then((ch) => {
        if (this.readyState === 3) return;
        this.h = ch.websocket(url.pathname + url.search, protocols, {
          onOpen: (p) => {
            this.protocol = p;
            this.readyState = 1;
            this.fire(new Event("open"));
          },
          onMessage: (text, data) => {
            const d = text ? dec.decode(data) : this.binaryType === "arraybuffer" ? data.buffer : new Blob([data as Uint8Array<ArrayBuffer>]);
            this.fire(new MessageEvent("message", { data: d, origin: location.origin }));
          },
          onClose: (code, reason) => this.closed(code, reason),
        });
      })
      .catch(() => this.closed(1006, ""));
  }

  private fire(e: Event) {
    this.dispatchEvent(e);
    const f = (this as unknown as Record<string, ((e: Event) => void) | null>)[`on${e.type}`];
    f?.call(this, e);
  }

  private closed(code: number, reason: string) {
    if (this.readyState === 3) return;
    const was = this.readyState;
    this.readyState = 3;
    if (code === 1006 || was === 0) this.fire(new Event("error"));
    this.fire(new CloseEvent("close", { code, reason, wasClean: code !== 1006 }));
  }

  send(data: string | ArrayBufferLike | Blob | ArrayBufferView) {
    if (this.readyState === 0) throw new DOMException("Still in CONNECTING state.", "InvalidStateError");
    if (this.readyState !== 1 || !this.h) return;
    const h = this.h;
    this.sendQ = this.sendQ.then(async () => {
      if (typeof data === "string") h.send(true, enc.encode(data));
      else if (data instanceof Blob) h.send(false, new Uint8Array(await data.arrayBuffer()));
      else if (ArrayBuffer.isView(data)) h.send(false, new Uint8Array(data.buffer, data.byteOffset, data.byteLength));
      else h.send(false, new Uint8Array(data as ArrayBuffer));
    });
  }

  close(code = 1000, reason = "") {
    if (this.readyState >= 2) return;
    this.readyState = 2;
    if (this.h) this.h.close(code, reason);
    else this.closed(code, reason);
  }
}

const Shimmed = new Proxy(NativeWebSocket, {
  construct(target, args: [string | URL, (string | string[])?]) {
    const u = new URL(args[0], location.href);
    if ((u.protocol === "ws:" || u.protocol === "wss:") && u.host === location.host) {
      const p = args[1];
      return new ShimSocket(u, p === undefined ? [] : typeof p === "string" ? [p] : p) as unknown as WebSocket;
    }
    return new target(...(args as [string, string[]]));
  },
});
window.WebSocket = Shimmed;
