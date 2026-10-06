// Many daemons' channels over one WebSocket to control's relay (M25).
//
// Browsers space out WebSocket connections to one address past about eight
// (S16), and through the relay every daemon is the same address. So a page
// that reaches many daemons opens `/api/relay/m` once, and each daemon's
// Noise channel is a numbered channel inside it. Each binary message is
// `kind (1) ‖ channel (4, BE) ‖ payload` (see control's relay.rs). A
// channel looks enough like a WebSocket for E2ESocket to use it as one.

const OPEN = 1;
const DATA = 2;
const CLOSE = 3;
const OPENED = 4;

/** The close code (try again later) control's relay closes a browser's
 * socket with when it's at its ceiling (#344), saying why. */
export const FULL = 1013;

/** Why control's relay is full, if this closed a socket for that. */
export function fullReason(e?: { code: number; reason: string }): string | undefined {
  return e?.code === FULL ? e.reason || "control is full: try again shortly" : undefined;
}

/** What E2ESocket needs of a socket. */
export interface SocketLike {
  binaryType: string;
  readonly url: string;
  readonly readyState: number;
  onmessage: ((e: { data: ArrayBuffer }) => void) | null;
  onclose: ((e?: { code: number; reason: string }) => void) | null;
  send(data: Uint8Array): void;
  close(): void;
}

function frame(kind: number, chan: number, payload: Uint8Array = new Uint8Array()): Uint8Array<ArrayBuffer> {
  const f = new Uint8Array(5 + payload.length);
  f[0] = kind;
  new DataView(f.buffer).setUint32(1, chan);
  f.set(payload, 5);
  return f;
}

class MuxChannel implements SocketLike {
  binaryType = "arraybuffer";
  readyState: number = WebSocket.CONNECTING;
  onmessage: ((e: { data: ArrayBuffer }) => void) | null = null;
  onclose: (() => void) | null = null;
  /** Why control closed it, if it did. */
  reason = "";
  /** Set when control's relay was full (#344). */
  full = false;
  private mux: RelayMux;
  readonly chan: number;
  readonly url: string;
  constructor(mux: RelayMux, chan: number, url: string) {
    this.mux = mux;
    this.chan = chan;
    this.url = url;
  }
  send(data: Uint8Array) {
    if (this.readyState === WebSocket.OPEN) this.mux.write(frame(DATA, this.chan, data));
  }
  close() {
    if (this.readyState === WebSocket.CLOSED) return;
    if (this.readyState === WebSocket.OPEN) this.mux.write(frame(CLOSE, this.chan));
    this.mux.forget(this.chan);
    this.ended();
  }
  ended() {
    if (this.readyState === WebSocket.CLOSED) return;
    this.readyState = WebSocket.CLOSED;
    this.onclose?.();
  }
}

export class RelayMux {
  private static all = new Map<string, RelayMux>();
  /** The one for this relay URL (`wss://control…/api/relay/m`). */
  static for(url: string): RelayMux {
    let m = RelayMux.all.get(url);
    if (!m) RelayMux.all.set(url, (m = new RelayMux(url)));
    return m;
  }

  /** Drop every shared socket (a simulated sleep, for tests). */
  static closeAll() {
    for (const m of RelayMux.all.values()) m.ws?.close();
  }

  /** WebSockets this has opened (for tests: one, however many daemons). */
  sockets = 0;
  private ws: WebSocket | null = null;
  private ready: Promise<WebSocket> | null = null;
  private next = 1;
  private chans = new Map<number, { ch: MuxChannel; opened: (ok: boolean) => void }>();

  readonly url: string;
  private constructor(url: string) {
    this.url = url;
  }

  get channels(): number {
    return this.chans.size;
  }

  private socket(timeoutMs: number): Promise<WebSocket> {
    if (this.ws?.readyState === WebSocket.OPEN) return Promise.resolve(this.ws);
    if (this.ready) return this.ready;
    this.ready = new Promise<WebSocket>((res, rej) => {
      const ws = new WebSocket(this.url);
      this.sockets++;
      ws.binaryType = "arraybuffer";
      const t = setTimeout(() => {
        ws.close();
        rej(new Error(`timed out: ${this.url}`));
      }, timeoutMs);
      ws.onopen = () => {
        clearTimeout(t);
        this.ws = ws;
        res(ws);
      };
      ws.onerror = () => {
        clearTimeout(t);
        rej(new Error(`couldn't connect: ${this.url}`));
      };
      ws.onmessage = (e) => this.take(new Uint8Array(e.data as ArrayBuffer));
      ws.onclose = (e) => {
        if (this.ws === ws) this.ws = null;
        const full = fullReason(e);
        // Every channel inside it is gone too.
        for (const { ch, opened } of [...this.chans.values()]) {
          if (full) Object.assign(ch, { reason: full, full: true });
          opened(false);
          ch.ended();
        }
        this.chans.clear();
      };
    }).finally(() => {
      this.ready = null;
    });
    return this.ready;
  }

  private take(b: Uint8Array) {
    if (b.length < 5) return;
    const chan = new DataView(b.buffer, b.byteOffset).getUint32(1);
    const c = this.chans.get(chan);
    if (!c) return;
    const payload = b.subarray(5);
    if (b[0] === OPENED) {
      c.ch.readyState = WebSocket.OPEN;
      c.opened(true);
    } else if (b[0] === DATA) {
      c.ch.onmessage?.({ data: payload.slice().buffer });
    } else if (b[0] === CLOSE) {
      c.ch.reason = new TextDecoder().decode(payload);
      this.chans.delete(chan);
      c.opened(false);
      c.ch.ended();
    }
  }

  write(f: Uint8Array<ArrayBuffer>) {
    if (this.ws?.readyState === WebSocket.OPEN) this.ws.send(f);
  }

  forget(chan: number) {
    this.chans.delete(chan);
  }

  /** A channel to daemon `id`, once control has its stream up. */
  async open(id: string, timeoutMs: number): Promise<SocketLike> {
    const ws = await this.socket(timeoutMs);
    const chan = this.next++;
    const ch = new MuxChannel(this, chan, `${this.url}#${id}`);
    return new Promise((res, rej) => {
      const t = setTimeout(() => {
        ch.close();
        rej(new Error(`timed out: ${id} through ${this.url}`));
      }, timeoutMs);
      this.chans.set(chan, {
        ch,
        opened: (ok) => {
          clearTimeout(t);
          if (ok) res(ch);
          // Control answered, but no channel: the shared socket works, so
          // a socket of its own wouldn't do better.
          else rej(Object.assign(new Error(ch.reason || `couldn't reach ${id}`), { refused: true, full: ch.full }));
        },
      });
      ws.send(frame(OPEN, chan, new TextEncoder().encode(id)));
    });
  }
}
