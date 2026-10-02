// Connection to the daemon and everything the UI reads: the server's layout
// state, this client's own selection (which session and tab it shows, which
// pane is active), and one terminal per pane.
//
// Every pane's output carries its absolute stream offset. A pane remembers
// the offset just past the last byte it drew and, on reconnect, asks to
// resume from there; the daemon replays the gap or sends a snapshot.

import {
  decodeFrame,
  encodeFrame,
  FrameKind,
  type ClientId,
  type ClientMsg,
  type Driver,
  type Intent,
  type PaneId,
  type PaneOp,
  type Presence,
  type ServerMsg,
  type SessionId,
  type State,
  type TabId,
  type TabView,
} from "./proto";
import { TerminalView } from "./terminal-view";
import { makeBlockView, type BlockView } from "./blocks";
import { E2ESocket, type DaemonRef } from "./e2e/channel.ts";
import type { DeviceKeys } from "./e2e/keys.ts";

export interface PaneEntry {
  view: TerminalView;
  epoch: number;
  offset: number | null;
  title: string;
}

/** A block that isn't a terminal: its type's view and latest state. */
export interface BlockEntry {
  view: BlockView;
  state: unknown;
}

export interface Modifiers {
  ctrl: boolean;
  alt: boolean;
}

/** An API answer, from fetch or through an end-to-end channel. */
export interface ApiResponse {
  ok: boolean;
  status: number;
  json<T = unknown>(): Promise<T>;
}

/** A daemon reached through illogical control (M17/M18): an end-to-end
 * channel, directly when one of its URLs answers, else through the relay. */
export interface E2ETarget {
  daemon: DaemonRef;
  /** Direct URLs from the directory (`https://box.….ts.net`). */
  direct: string[];
  /** Control's relay for it (`wss://control…/api/relay/c/<id>`). */
  relay: string;
  keys: DeviceKeys;
}

/** The connection a Client talks over. */
interface Link {
  onText: (t: string) => void;
  onBinary: (b: ArrayBuffer) => void;
  onClose: () => void;
  readonly open: boolean;
  sendText(t: string): void;
  sendBinary(b: Uint8Array): void;
  close(): void;
}

class SocketLink implements Link {
  onText: (t: string) => void = () => {};
  onBinary: (b: ArrayBuffer) => void = () => {};
  onClose: () => void = () => {};
  private sock: WebSocket;
  constructor(url: string) {
    this.sock = new WebSocket(url);
    this.sock.binaryType = "arraybuffer";
    this.sock.onmessage = (e) => (typeof e.data === "string" ? this.onText(e.data) : this.onBinary(e.data as ArrayBuffer));
    this.sock.onclose = () => this.onClose();
  }
  get open() {
    return this.sock.readyState === WebSocket.OPEN;
  }
  sendText(t: string) {
    this.sock.send(t);
  }
  sendBinary(b: Uint8Array) {
    this.sock.send(b as Uint8Array<ArrayBuffer>);
  }
  close() {
    this.sock.close();
  }
}

class E2ELink implements Link {
  onText: (t: string) => void = () => {};
  onBinary: (b: ArrayBuffer) => void = () => {};
  onClose: () => void = () => {};
  sock: E2ESocket | undefined;
  private closed = false;
  /** Connects in the background; the Client sees it as a socket that opens
   * (or closes, and is retried). */
  constructor(target: E2ETarget, onPath: (how: "direct" | "relayed") => void) {
    const urls = [
      ...target.direct.map((u) => ({ url: `${u.replace(/^http/, "ws").replace(/\/$/, "")}/e2e`, timeoutMs: 1500 })),
      { url: target.relay, timeoutMs: 10_000 },
    ];
    E2ESocket.connect(urls, target.daemon, target.keys).then(
      (sock) => {
        if (this.closed) return sock.close();
        this.sock = sock;
        onPath(sock.url.startsWith(target.relay) ? "relayed" : "direct");
        sock.onText = (t) => this.onText(t);
        sock.onBinary = (b) => this.onBinary(b.slice().buffer as ArrayBuffer);
        sock.onClose = () => this.onClose();
        sock.start();
      },
      () => this.onClose(),
    );
  }
  get open() {
    return !!this.sock?.open;
  }
  sendText(t: string) {
    this.sock?.sendText(t);
  }
  sendBinary(b: Uint8Array) {
    this.sock?.sendBinary(b);
  }
  close() {
    this.closed = true;
    this.sock?.close();
  }
}

export class Client {
  /** The daemon's origin (`https://box.….ts.net`), or "" for the one this
   * page came from. Another daemon must list this page's origin as
   * allowed (`--allow-origin`). A path (`/h/box`) is a dial-out host,
   * reached through this page's own daemon. `e2e:<id>` with a target: a
   * daemon reached through illogical control. */
  constructor(
    readonly base = "",
    readonly e2e?: E2ETarget,
  ) {}

  /** How an end-to-end client is connected, for the host chip. */
  path: "direct" | "relayed" | null = null;

  /** A request to the daemon's API: fetch, or through the channel. */
  async request(method: string, path: string, body?: unknown): Promise<ApiResponse> {
    if (this.e2e) {
      const sock = (this.link as E2ELink | undefined)?.sock;
      if (!sock?.open) throw new Error("not connected");
      const r = await sock.request(method, path, body);
      return { ok: r.ok, status: r.status, json: async <T,>() => r.json<T>() };
    }
    const res = await fetch(this.base + path, {
      method,
      ...(body === undefined ? {} : { headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) }),
    });
    return { ok: res.ok, status: res.status, json: <T,>() => res.json() as Promise<T> };
  }

  state: State | null = null;
  clientId: number | null = null;
  connected = false;
  error: string | null = null;
  session: SessionId | null = null;
  tab: TabId | null = null;
  readonly activePane = new Map<TabId, PaneId>();
  /** Terminals, by pane id. */
  readonly panes = new Map<PaneId, PaneEntry>();
  /** Every other block type, in the same id space. */
  readonly blocks = new Map<PaneId, BlockEntry>();
  /** Sticky modifiers from the phone key bar, applied to the next key. */
  modifiers: Modifiers = { ctrl: false, alt: false };
  private focused: PaneId | null | undefined = undefined;

  /** Tell the daemon which pane this client is looking at (`null`: none,
   * the window is in the background). Attention skips panes being looked
   * at. */
  focusPane(pane: PaneId | null) {
    if (pane === this.focused || !this.connected) return;
    this.focused = pane;
    this.send({ type: "focus", pane });
  }

  /** Set by the UI: make this client's size the tab's size. */
  claim: (tab: TabId) => void = () => {};

  private link: Link | undefined;
  private retry = 0;
  private nextId = 1;
  private listeners = new Set<() => void>();
  private errorTimer: number | undefined;
  /** When this client last asked for a change; panes that appear soon
   * after are the ones it created, and become active. */
  private lastIntentAt = 0;
  private pendingBlocks = new Map<PaneId, unknown>();

  subscribe(fn: () => void): () => void {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  }

  emit() {
    for (const fn of this.listeners) fn();
  }

  // ---- reading state

  tabView(id: TabId | null = this.tab): TabView | undefined {
    return this.state?.tabs.find((t) => t.id === id);
  }

  tabOfPane(pane: PaneId): TabView | undefined {
    return this.state?.tabs.find((t) => paneIds(t).includes(pane));
  }

  sessionOfTab(tab: TabId): SessionId | undefined {
    return this.state?.sessions.find((s) => s.tabs.includes(tab))?.id;
  }

  active(tab: TabId | null = this.tab): PaneId | undefined {
    if (tab === null) return undefined;
    const t = this.tabView(tab);
    const a = this.activePane.get(tab);
    return t && a !== undefined && paneIds(t).includes(a) ? a : t ? paneIds(t)[0] : undefined;
  }

  /** The element a block of any type is drawn in, and how to show it. */
  viewOf(id: PaneId): { host: HTMLElement; setVisible(v: boolean): void; focus(): void } | undefined {
    return this.panes.get(id)?.view ?? this.blocks.get(id)?.view;
  }

  /** What a block calls itself: a terminal's title, or its view's. */
  title(id: PaneId): string {
    return this.panes.get(id)?.title || this.blocks.get(id)?.view.title() || "";
  }

  // ---- other people (M13)

  /** Someone asking to drive a pane this client's person drives. */
  requests: { pane: PaneId; who: string; name: string }[] = [];
  /** M14: guests asking the owner to trust them with a pane here. */
  trustRequests: { pane: PaneId; who: string; name: string }[] = [];

  answerTrust(pane: PaneId, who: string, minutes: number | null) {
    this.trustRequests = this.trustRequests.filter((r) => !(r.pane === pane && r.who === who));
    if (minutes) this.paneOp(pane, { op: "grant_trust", to: who, minutes });
    this.emit();
  }

  /** M14: whether this client's person may type in `pane` (a guest needs
   * a VM pane, or the owner's trust). */
  mayType(pane: PaneId): boolean {
    if (!this.state?.roles) return true;
    const info = this.info(pane);
    if (!info) return false;
    const session = this.sessionOfTab(this.tabOfPane(pane)?.id ?? -1);
    if (this.role(session ?? null) === "viewer") return false;
    if (info.host != null || info.type !== "terminal") return true;
    const me = this.me();
    return (info.trusted ?? []).some(([w, until]) => w === me && until > Date.now());
  }

  /** This client's principal id (`owner` for the daemon's owner). */
  me(): string {
    return this.state?.presence?.find((p) => p.client === this.clientId)?.who ?? "owner";
  }

  /** Everyone else connected, within what this client sees. */
  others(): Presence[] {
    const me = this.me();
    return (this.state?.presence ?? []).filter((p) => p.who !== me);
  }

  /** Who drives `pane`, if it's someone else. */
  drivenBy(pane: PaneId): Driver | undefined {
    const d = this.info(pane)?.driver;
    return d && d.who !== this.me() ? d : undefined;
  }

  /** Following someone's focus (a client id) until this client acts. */
  following: ClientId | null = null;

  follow(client: ClientId | null) {
    this.following = client;
    this.applyFollow();
    this.emit();
  }

  private applyFollow() {
    if (this.following === null) return;
    const p = this.state?.presence?.find((x) => x.client === this.following);
    if (!p) {
      this.following = null;
      return;
    }
    if (p.tab !== undefined && p.tab !== this.tab && this.tabView(p.tab)) {
      this.tab = p.tab;
      this.session = this.sessionOfTab(p.tab) ?? this.session;
    }
    if (p.pane !== undefined && p.tab !== undefined) this.activePane.set(p.tab, p.pane);
  }

  answerRequest(pane: PaneId, give: boolean) {
    const r = this.requests.find((x) => x.pane === pane);
    this.requests = this.requests.filter((x) => x.pane !== pane);
    if (r && give) this.paneOp(pane, { op: "give_control", to: r.who });
    this.emit();
  }

  /** This client's role in a session (M12): `owner` unless the daemon
   * said otherwise. */
  role(session: SessionId | null = this.session): "viewer" | "editor" | "owner" {
    const r = session === null ? undefined : this.state?.roles?.find(([s]) => s === session)?.[1];
    return this.state?.roles ? (r ?? "viewer") : "owner";
  }

  cwd(pane: PaneId): string | null {
    return this.info(pane)?.cwd ?? null;
  }

  info(pane: PaneId) {
    return this.state?.panes.find((p) => p.id === pane);
  }

  /** The machine a pane runs on, if not the daemon's host. */
  machine(pane: PaneId) {
    const host = this.info(pane)?.host;
    return host == null ? undefined : this.state?.machines?.find((m) => m.id === host);
  }

  /** The machine a tab owns, which its panes share. */
  tabMachine(tab: TabId) {
    return this.state?.machines?.find((m) => "tab" in m.owner && m.owner.tab === tab);
  }

  /** Panes running on a machine. */
  panesOn(machine: number): PaneId[] {
    return (this.state?.panes ?? []).filter((p) => p.host === machine).map((p) => p.id);
  }

  /** POST to the API; a failure shows as a toast. */
  async api(path: string, body: unknown = {}, failure = "that didn't work") {
    try {
      const res = await this.request("POST", path, body);
      if (!res.ok) this.toast((await res.json<{ error?: string }>().catch(() => null))?.error ?? `${failure} (${res.status})`);
      return res.ok;
    } catch {
      this.toast(failure);
      return false;
    }
  }

  /** A read-only link to a terminal pane (M4c), good for `ttlSecs`; copied
   * to the clipboard when the browser lets us. */
  async share(pane: PaneId, ttlSecs = 3600): Promise<string | null> {
    try {
      const res = await this.request("POST", "/api/shares", { pane, ttl_secs: ttlSecs });
      const body = await res.json<{ url?: string; path?: string; error?: string }>().catch(() => null);
      if (!res.ok || !body) {
        this.toast(body?.error ?? `couldn't share it (${res.status})`);
        return null;
      }
      const url = body.url ?? new URL(body.path ?? "", location.href).href;
      await navigator.clipboard?.writeText(url).catch(() => {});
      return url;
    } catch {
      this.toast("couldn't share it");
      return null;
    }
  }

  /**
   * A shell on a new throwaway VM: a tab in `session` whose panes share it
   * (`tab`), a pane-owned one in a tab of its own, or a split of `split`.
   */
  async newVm(where: { session?: number; split?: PaneId; tab?: boolean }) {
    // The session's own pane names it exactly (a session id given as text
    // could also be another session's name).
    const tab = where.session === undefined ? undefined : this.state?.sessions.find((s) => s.id === where.session)?.tabs[0];
    const fromPane = tab === undefined ? null : (this.active(tab) ?? null);
    // Show it when it appears, as for a tab made here.
    this.lastIntentAt = Date.now();
    await this.api(
      "/api/run",
      {
        vm: !where.tab,
        vm_tab: !!where.tab,
        from_pane: fromPane,
        session: fromPane === null ? (where.session?.toString() ?? null) : null,
        split: where.tab ? null : (where.split ?? null),
      },
      "couldn't start a VM",
    );
  }

  /** An agent block (M6b): beside `split`, or in a new tab of `session`. */
  async newAgent(o: { config: Record<string, unknown>; vm: boolean; split?: PaneId; session?: number; from?: PaneId }) {
    this.lastIntentAt = Date.now();
    await this.api(
      "/api/blocks",
      {
        type: "agent",
        config: o.config,
        vm: o.vm,
        split: o.split ?? null,
        from_pane: o.from ?? null,
        session: o.from === undefined ? (o.session?.toString() ?? null) : null,
      },
      "couldn't start the agent",
    );
  }

  /** POST to the API and show what it makes (a pane from `/api/run`);
   * the error if it failed, for whoever asked to show it. */
  async make(path: string, body: unknown): Promise<string | null> {
    this.lastIntentAt = Date.now();
    try {
      const res = await this.request("POST", path, body);
      if (res.ok) return null;
      return (await res.json<{ error?: string }>().catch(() => null))?.error ?? `that didn't work (${res.status})`;
    } catch {
      return "can't reach the daemon";
    }
  }

  paneOp(pane: PaneId, op: PaneOp) {
    this.send({ type: "pane", pane, op });
  }

  // ---- changing local selection

  selectTab(tab: TabId) {
    this.tab = tab;
    this.session = this.sessionOfTab(tab) ?? this.session;
    this.emit();
  }

  selectSession(session: SessionId) {
    this.session = session;
    this.tab = this.state?.sessions.find((s) => s.id === session)?.tabs[0] ?? null;
    this.emit();
  }

  setActive(pane: PaneId) {
    const tab = this.tabOfPane(pane);
    if (!tab) return;
    if (this.tab !== tab.id) this.selectTab(tab.id);
    if (this.activePane.get(tab.id) !== pane) {
      this.activePane.set(tab.id, pane);
      this.emit();
    }
  }

  // ---- talking to the daemon

  connect() {
    if (this.closed || this.asleep) return;
    let link: Link;
    if (this.e2e) {
      link = new E2ELink(this.e2e, (how) => {
        this.path = how;
        this.emit();
      });
    } else {
      const here = `${location.protocol === "https:" ? "wss" : "ws"}://${location.host}`;
      const url = /^https?:/.test(this.base) ? `${this.base.replace(/^http/, "ws")}/ws` : `${here}${this.base}/ws`;
      link = new SocketLink(url);
    }
    this.link = link;
    link.onText = (t) => this.onMessage(JSON.parse(t) as ServerMsg);
    link.onBinary = (b) => this.onFrame(b);
    link.onClose = () => {
      if (this.link !== link) return;
      this.link = undefined;
      this.connected = false;
      this.clientId = null;
      const delay = Math.min(250 * 2 ** this.retry, 5000);
      this.retry++;
      this.emit();
      setTimeout(() => this.connect(), delay);
    };
  }

  /** Done with this daemon (another host is shown): disconnect for good,
   * so a sandbox isn't kept awake, and let go of its terminals. */
  close() {
    this.closed = true;
    const link = this.link;
    this.link = undefined;
    this.connected = false;
    link?.close();
    for (const p of this.panes.values()) p.view.dispose();
    for (const b of this.blocks.values()) b.view.dispose();
    this.panes.clear();
    this.blocks.clear();
    this.listeners.clear();
  }

  private closed = false;
  private asleep = false;

  /** Let go of the connection while nobody is looking (a sandbox host:
   * an open connection keeps it awake); `wake` reconnects. */
  sleep() {
    if (this.closed || !this.link) return;
    this.asleep = true;
    const link = this.link;
    this.link = undefined;
    this.connected = false;
    link.close();
    this.emit();
  }

  /** Reconnect now if the socket is down (a phone coming back). */
  wake() {
    this.asleep = false;
    if (!this.link && !this.closed) {
      this.retry = 0;
      this.connect();
    }
  }

  send(msg: ClientMsg) {
    if (this.link?.open) this.link.sendText(JSON.stringify(msg));
  }

  intent(intent: Intent) {
    this.lastIntentAt = Date.now();
    this.send({ type: "intent", id: this.nextId++, intent });
  }

  view(tab: TabId, cols: number, rows: number, zoom: PaneId | null, claim: boolean) {
    this.send({ type: "view", tab, cols, rows, zoom, claim });
  }

  input(pane: PaneId, data: Uint8Array) {
    const tab = this.tabOfPane(pane);
    // Typing here makes this window the one whose size counts.
    if (tab && tab.owner !== this.clientId) this.claim(tab.id);
    data = this.applyModifiers(data);
    if (this.link?.open) this.link.sendBinary(encodeFrame(FrameKind.Input, pane, data));
  }

  private applyModifiers(data: Uint8Array): Uint8Array {
    const { ctrl, alt } = this.modifiers;
    if (!ctrl && !alt) return data;
    this.modifiers = { ctrl: false, alt: false };
    this.emit();
    let out = data;
    if (ctrl && data.length === 1) {
      const c = data[0];
      if (c >= 0x61 && c <= 0x7a) out = Uint8Array.of(c - 0x60); // a-z
      else if (c >= 0x40 && c <= 0x5f) out = Uint8Array.of(c - 0x40); // @A-Z[\]^_
      else if (c === 0x20) out = Uint8Array.of(0); // space
    }
    if (alt) out = Uint8Array.of(0x1b, ...out);
    return out;
  }

  /** Show a short message in the status pill. */
  toast(message: string) {
    this.showError(message);
  }

  private showError(message: string) {
    this.error = message;
    clearTimeout(this.errorTimer);
    this.errorTimer = window.setTimeout(() => {
      this.error = null;
      this.emit();
    }, 4000);
    this.emit();
  }

  private onMessage(msg: ServerMsg) {
    switch (msg.type) {
      case "hello":
        this.clientId = msg.client;
        this.connected = true;
        this.focused = undefined;
        this.retry = 0;
        this.applyState(msg.state, true);
        break;
      case "state":
        this.applyState(msg.state, false);
        break;
      case "size":
        this.panes.get(msg.pane)?.view.resize(msg.cols, msg.rows);
        break;
      case "resync": {
        const p = this.panes.get(msg.pane);
        if (p) {
          p.offset = null;
          this.send({ type: "attach", panes: [{ pane: msg.pane, offset: null }] });
        }
        break;
      }
      case "error":
        this.showError(msg.message);
        break;
      case "notice":
        this.showError(msg.message);
        break;
      case "control_request":
        this.requests = [...this.requests.filter((r) => r.pane !== msg.pane), { pane: msg.pane, who: msg.who, name: msg.name }];
        this.emit();
        break;
      case "trust_request":
        this.trustRequests = [
          ...this.trustRequests.filter((r) => !(r.pane === msg.pane && r.who === msg.who)),
          { pane: msg.pane, who: msg.who, name: msg.name },
        ];
        this.emit();
        break;
      case "block": {
        const b = this.blocks.get(msg.block);
        if (b) {
          b.state = msg.state;
          b.view.update(msg.state);
          this.emit();
        } else {
          // Its state can arrive before the layout that has it.
          this.pendingBlocks.set(msg.block, msg.state);
        }
        break;
      }
    }
  }

  /** Bring panes and the local selection in line with the server. On a
   * new connection every known pane resumes from its offset. */
  private applyState(state: State, reconnect: boolean) {
    this.state = state;
    const attach: { pane: PaneId; offset: number | null }[] = [];
    const created: PaneId[] = [];
    const live = new Set(state.panes.map((p) => p.id));
    for (const [id, entry] of this.panes) {
      if (!live.has(id)) {
        entry.view.dispose();
        this.panes.delete(id);
      }
    }
    for (const [id, entry] of this.blocks) {
      if (!live.has(id)) {
        entry.view.dispose();
        this.blocks.delete(id);
      }
    }
    for (const info of state.panes) {
      if (info.type !== "terminal") {
        if (!this.blocks.has(info.id)) {
          const view = makeBlockView(info.type, this, info.id);
          const pending = this.pendingBlocks.get(info.id);
          this.pendingBlocks.delete(info.id);
          if (pending !== undefined) view.update(pending);
          this.blocks.set(info.id, { view, state: pending ?? null });
          if (!reconnect) created.push(info.id);
        }
        continue;
      }
      let entry = this.panes.get(info.id);
      if (entry && entry.epoch !== info.epoch) {
        // A different stream (daemon restarted): our offset means nothing.
        entry.epoch = info.epoch;
        entry.offset = null;
        attach.push({ pane: info.id, offset: null });
      } else if (!entry) {
        entry = this.newPane(info.id, info.epoch);
        attach.push({ pane: info.id, offset: null });
        if (!reconnect) created.push(info.id);
      } else if (reconnect) {
        attach.push({ pane: info.id, offset: entry.offset });
      }
    }
    // Layout is the truth for sizes of visible panes.
    for (const t of state.tabs) {
      for (const [id, r] of t.layout.panes) this.panes.get(id)?.view.resize(r.cols, r.rows);
    }
    this.fixSelection();
    this.applyFollow();
    // Show what we just made: a split's new pane, a new tab or session.
    if (created.length && Date.now() - this.lastIntentAt < 3000) {
      const tab = this.tabOfPane(created[0]);
      if (tab) {
        this.session = this.sessionOfTab(tab.id) ?? this.session;
        this.tab = tab.id;
        this.activePane.set(tab.id, created[0]);
      }
    }
    if (attach.length) this.send({ type: "attach", panes: attach });
    this.emit();
  }

  private newPane(id: PaneId, epoch: number): PaneEntry {
    const view = new TerminalView();
    const entry: PaneEntry = { view, epoch, offset: null, title: "" };
    view.onInput((data) => this.input(id, data));
    view.onTitle((t) => {
      entry.title = t;
      this.emit();
    });
    view.onFocus(() => this.setActive(id));
    this.panes.set(id, entry);
    return entry;
  }

  private fixSelection() {
    const s = this.state;
    if (!s) return;
    if (!s.sessions.some((x) => x.id === this.session)) this.session = s.sessions[0]?.id ?? null;
    const tabs = s.sessions.find((x) => x.id === this.session)?.tabs ?? [];
    if (this.tab === null || !tabs.includes(this.tab)) this.tab = tabs[0] ?? null;
  }

  private onFrame(buf: ArrayBuffer) {
    const f = decodeFrame(buf);
    const p = this.panes.get(f.pane);
    if (!p) return;
    if (f.kind === FrameKind.Snapshot) {
      p.view.reset();
      p.view.write(f.data);
      p.offset = f.offset;
      return;
    }
    if (f.kind !== FrameKind.Output || p.offset === null) return;
    let data = f.data;
    if (f.offset > p.offset) {
      // A gap we can't fill: start over from a snapshot.
      p.offset = null;
      this.send({ type: "attach", panes: [{ pane: f.pane, offset: null }] });
      return;
    }
    if (f.offset < p.offset) {
      const skip = p.offset - f.offset;
      if (skip >= data.length) return;
      data = data.subarray(skip);
    }
    p.view.write(data);
    p.offset += data.length;
  }
}

export function paneIds(tab: TabView): PaneId[] {
  const out: PaneId[] = [];
  const walk = (n: TabView["root"]) => {
    if (n.type === "pane") out.push(n.pane);
    else n.children.forEach((c) => walk(c.node));
  };
  walk(tab.root);
  return out;
}

/** What to call a tab: its name, else its active pane's title or folder. */
export function tabLabel(client: Client, tab: TabView): string {
  if (tab.name) return tab.name;
  const pane = client.active(tab.id);
  if (pane === undefined) return `@${tab.id}`;
  const title = client.title(pane);
  if (title) return title;
  const cwd = client.cwd(pane);
  return cwd ? cwd.split("/").filter(Boolean).pop() ?? "/" : `@${tab.id}`;
}
