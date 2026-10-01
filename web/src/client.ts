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
  type ClientMsg,
  type Intent,
  type PaneId,
  type ServerMsg,
  type SessionId,
  type State,
  type TabId,
  type TabView,
} from "./proto";
import { TerminalView } from "./terminal-view";

export interface PaneEntry {
  view: TerminalView;
  epoch: number;
  offset: number | null;
  title: string;
}

export interface Modifiers {
  ctrl: boolean;
  alt: boolean;
}

export class Client {
  state: State | null = null;
  clientId: number | null = null;
  connected = false;
  error: string | null = null;
  session: SessionId | null = null;
  tab: TabId | null = null;
  readonly activePane = new Map<TabId, PaneId>();
  readonly panes = new Map<PaneId, PaneEntry>();
  /** Sticky modifiers from the phone key bar, applied to the next key. */
  modifiers: Modifiers = { ctrl: false, alt: false };
  /** Set by the UI: make this client's size the tab's size. */
  claim: (tab: TabId) => void = () => {};

  private ws: WebSocket | undefined;
  private retry = 0;
  private nextId = 1;
  private listeners = new Set<() => void>();
  private errorTimer: number | undefined;
  /** When this client last asked for a change; panes that appear soon
   * after are the ones it created, and become active. */
  private lastIntentAt = 0;

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

  cwd(pane: PaneId): string | null {
    return this.state?.panes.find((p) => p.id === pane)?.cwd ?? null;
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
    const url = `${location.protocol === "https:" ? "wss" : "ws"}://${location.host}/ws`;
    const sock = new WebSocket(url);
    sock.binaryType = "arraybuffer";
    this.ws = sock;
    sock.onmessage = (e) => {
      if (typeof e.data === "string") this.onMessage(JSON.parse(e.data) as ServerMsg);
      else this.onFrame(e.data as ArrayBuffer);
    };
    sock.onclose = () => {
      if (this.ws !== sock) return;
      this.ws = undefined;
      this.connected = false;
      this.clientId = null;
      const delay = Math.min(250 * 2 ** this.retry, 5000);
      this.retry++;
      this.emit();
      setTimeout(() => this.connect(), delay);
    };
  }

  /** Reconnect now if the socket is down (a phone coming back). */
  wake() {
    if (!this.ws) {
      this.retry = 0;
      this.connect();
    }
  }

  send(msg: ClientMsg) {
    if (this.ws?.readyState === WebSocket.OPEN) this.ws.send(JSON.stringify(msg));
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
    if (this.ws?.readyState === WebSocket.OPEN) this.ws.send(encodeFrame(FrameKind.Input, pane, data));
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
    for (const info of state.panes) {
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
  const title = client.panes.get(pane)?.title;
  if (title) return title;
  const cwd = client.cwd(pane);
  return cwd ? cwd.split("/").filter(Boolean).pop() ?? "/" : `@${tab.id}`;
}
