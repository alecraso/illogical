// Remote panes (#17, M4's option (a)): a pane that lives on another host,
// with its place in this layout. The home daemon keeps only `{host, pane}`
// (a block of type `remote`); this page connects to that host directly for
// the pane's bytes, so nothing is relayed, and draws its terminal in the
// block's slot. Its size, restart policy and history are its host's.
//
// One connection per host serves every remote pane on it, drawing only
// those panes. While the host can't be reached, the slot says so and the
// connection keeps retrying, so the pane comes back by itself.
//
// Making one: the host runs it (in a session named after the home daemon),
// then the home daemon records its place. Closing it here closes it there
// too, when the host can be reached; a pane its host closed (it exited, or
// was closed on the host's own page) loses its place here.

import { Client, paneIds } from "../client";
import { directory } from "../hosts";
import type { PaneId, RemoteRef, TabId } from "../proto";
import { registerBlock, type BlockView } from "./view";
import type { TerminalView } from "../terminal-view";

/** A pane never seen on its host is taken as gone only after this long:
 * the host's layout can arrive after the home daemon's. */
const GONE_GRACE_MS = 5000;
/** A host that dropped off the network closes nothing: its connection
 * just goes quiet. Ask a quiet host to answer this often... */
const HEARTBEAT_MS = 3000;
/** ...and give up on a link that said nothing for this long, or that
 * hasn't connected in this long, and connect again as after any drop. */
const SILENT_MS = 6000;

interface HostLink {
  client: Client;
  /** The views showing each of its panes. */
  views: Map<PaneId, Set<RemoteView>>;
  /** When the connect under way started (ms), if one is. */
  trying: number | null;
}

/** Every host this page shows remote panes of, and its connection. */
class RemoteHosts {
  private hosts = new Map<string, HostLink>();
  private timer: number | undefined;

  /** Heartbeats, and dropping links that went quiet (as the fleet does). */
  private tick = () => {
    const now = Date.now();
    for (const l of this.hosts.values()) {
      const c = l.client;
      if (c.connected) {
        l.trying = null;
        if (now - c.lastHeard > SILENT_MS) c.drop();
        else if (now - c.lastHeard > HEARTBEAT_MS) c.heartbeat();
      } else if (c.linked) {
        l.trying ??= now;
        if (now - l.trying > SILENT_MS) {
          l.trying = null;
          c.drop();
        }
      } else {
        l.trying = null;
      }
    }
  };

  /** Whether `host` can be reached from this page: a host in the home
   * daemon's list (not control's directory, which has no home daemon). */
  known(host: string): boolean {
    return !directory.control && !!directory.find(host);
  }

  take(host: string, pane: PaneId, view: RemoteView): Client | null {
    if (!this.known(host)) return null;
    let link = this.hosts.get(host);
    if (!link) {
      const client = new Client(directory.base(host));
      client.only = new Set();
      const l: HostLink = { client, views: new Map(), trying: null };
      // Typing in a pane there makes this window the one whose size counts,
      // here and there.
      client.claim = (tab: TabId) => {
        const t = client.tabView(tab);
        for (const [p, vs] of l.views) if (t && paneIds(t).includes(p)) vs.forEach((v) => v.claimed());
      };
      client.connect();
      this.hosts.set(host, l);
      this.timer ??= window.setInterval(this.tick, 1000);
      link = l;
    }
    (link.views.get(pane) ?? link.views.set(pane, new Set()).get(pane)!).add(view);
    link.client.want(pane);
    return link.client;
  }

  release(host: string, pane: PaneId, view: RemoteView) {
    const link = this.hosts.get(host);
    const vs = link?.views.get(pane);
    if (!link || !vs?.delete(view) || vs.size) return;
    link.views.delete(pane);
    link.client.unwant(pane);
    if (link.views.size === 0) {
      link.client.close();
      this.hosts.delete(host);
      if (this.hosts.size === 0 && this.timer !== undefined) {
        window.clearInterval(this.timer);
        this.timer = undefined;
      }
    }
  }

  /** For tests: the connection to `host`, if any. */
  client(host: string): Client | undefined {
    return this.hosts.get(host)?.client;
  }
}

export const remotes = new RemoteHosts();

/** A shell on `host` with its place in `home`'s layout: beside `split`,
 * or in a new tab of `session`. */
export async function newRemote(home: Client, host: string, where: { split?: PaneId; session?: number }) {
  const there = new Client(directory.base(host));
  let pane: PaneId;
  try {
    const res = await there.request("POST", "/api/run", { session: directory.home });
    if (!res.ok) {
      home.toast((await res.json<{ error?: string }>().catch(() => null))?.error ?? `${host} said no (${res.status})`);
      return;
    }
    pane = (await res.json<{ pane: PaneId }>()).pane;
  } catch {
    home.toast(`can't reach ${host}`);
    return;
  }
  const failed = await home.make("/api/blocks", {
    type: "remote",
    config: { host, pane },
    split: where.split ?? null,
    session: where.split === undefined && where.session !== undefined ? String(where.session) : null,
  });
  if (failed) {
    home.toast(failed);
    // Nothing here shows it: don't leave it running there.
    void there.request("POST", `/api/panes/${pane}/close`).catch(() => {});
  }
}

/** The hosts a pane could be opened on from here: the home daemon's list,
 * when this page shows the home daemon. */
export function remoteHosts(): string[] {
  if (directory.control || directory.shown !== null) return [];
  return directory.list?.hosts.map((h) => h.name) ?? [];
}

class RemoteView implements BlockView {
  readonly host = document.createElement("div");
  private badge = document.createElement("div");
  private note = document.createElement("div");
  private at: RemoteRef | null = null;
  private client: Client | null = null;
  private off: (() => void) | null = null;
  private term: TerminalView | null = null;
  private visible = true;
  /** Seen on its host: once it isn't, it's gone. */
  private seen = false;
  private missingSince = 0;
  private gone = false;
  private goneTimer: number | undefined;
  /** Its place here, and whether this window sizes it. */
  private size: { cols: number; rows: number; owned: boolean } | null = null;
  /** What its host was last told, and on which connection. */
  private pushed = "";

  constructor(
    private home: Client,
    private id: PaneId,
  ) {
    this.host.className = "block block-remote";
    this.badge.className = "remote-badge";
    this.note.className = "remote-note";
    this.host.append(this.badge, this.note);
  }

  update(state: unknown) {
    const at = state as RemoteRef;
    if (this.at && this.at.host === at.host && this.at.pane === at.pane) return;
    this.release();
    this.at = at;
    this.badge.textContent = at.host;
    this.host.dataset.remoteHost = at.host;
    this.host.dataset.remotePane = String(at.pane);
    this.client = remotes.take(at.host, at.pane, this);
    if (this.client) {
      const c = this.client;
      this.off = c.subscribe(() => this.refresh());
    }
    this.refresh();
  }

  private release() {
    this.off?.();
    this.off = null;
    if (this.at && this.client) remotes.release(this.at.host, this.at.pane, this);
    this.client = null;
    this.setTerm(null);
    clearTimeout(this.goneTimer);
  }

  private setTerm(t: TerminalView | null) {
    if (t === this.term) return;
    if (this.term?.host.parentElement === this.host) this.term.host.remove();
    this.term = t;
    this.pushed = "";
    if (t) {
      this.host.insertBefore(t.host, this.badge);
      t.setVisible(this.visible);
    }
  }

  /** How it is: connected and shown, or why not. */
  private refresh() {
    const at = this.at;
    if (!at) return;
    const c = this.client;
    let note = "";
    let state = "live";
    if (!c) {
      note = `${at.host} isn't in this daemon's host list.`;
      state = "unknown";
    } else if (!c.connected) {
      note = `${at.host} is unreachable · reconnecting…`;
      state = "unreachable";
    } else if (c.state && !c.info(at.pane)) {
      if (c.state.roles) {
        note = `%${at.pane} on ${at.host} isn't shared with you.`;
        state = "unknown";
      } else {
        state = "gone";
        note = `%${at.pane} has closed on ${at.host}.`;
        this.missingSince ||= Date.now();
        // Its host closed it: so does its place here. A pane never seen
        // there may just not have arrived yet. (A moment's jitter, so every
        // window doesn't ask at once.)
        const wait = this.seen ? Math.random() * 300 : GONE_GRACE_MS - (Date.now() - this.missingSince);
        clearTimeout(this.goneTimer);
        this.goneTimer = window.setTimeout(() => this.lose(), Math.max(0, wait));
      }
    } else if (c.state) {
      this.seen = true;
      this.missingSince = 0;
      clearTimeout(this.goneTimer);
    }
    this.setTerm(c?.panes.get(at.pane)?.view ?? null);
    this.host.dataset.state = state;
    this.note.textContent = note;
    this.note.style.display = note ? "" : "none";
    this.push();
    this.home.emit();
  }

  /** Its host closed it: close its place here (only that). */
  private lose() {
    const c = this.client;
    if (this.gone || !this.at || !c?.connected || !c.state || c.info(this.at.pane) || c.state.roles) return;
    // Another window got there first.
    if (!this.home.info(this.id)) return;
    this.gone = true;
    this.home.intent({ op: "close_pane", pane: this.id });
  }

  /** Tell its host the size of its place here. */
  private push(claim = false) {
    const c = this.client;
    const at = this.at;
    if (!c?.connected || !at || !this.size) return;
    const t = c.tabOfPane(at.pane);
    if (!t) return;
    const zoom = paneIds(t).length > 1 ? at.pane : null;
    const owned = claim || this.size.owned;
    const key = `${c.clientId}:${t.id}:${this.size.cols}x${this.size.rows}:${zoom}:${owned}`;
    if (key === this.pushed && !claim) return;
    this.pushed = key;
    c.view(t.id, this.size.cols, this.size.rows, zoom, owned);
  }

  layout(cols: number, rows: number, owned: boolean) {
    this.size = { cols, rows, owned };
    this.push();
  }

  /** Typed into on its host's connection: this window sizes it now. */
  claimed() {
    const tab = this.home.tabOfPane(this.id);
    if (tab && tab.owner !== this.home.clientId) this.home.claim(tab.id);
    this.push(true);
  }

  closing() {
    const c = this.client;
    const at = this.at;
    if (!at || this.gone) return;
    this.gone = true;
    if (c?.connected && c.info(at.pane)) c.intent({ op: "close_pane", pane: at.pane });
    else this.home.toast(`${at.host} can't be reached: %${at.pane} stays open there`);
  }

  setVisible(v: boolean) {
    this.visible = v;
    this.host.style.display = v ? "" : "none";
    this.term?.setVisible(v);
  }

  title(): string {
    const at = this.at;
    if (!at) return "remote";
    const c = this.client;
    const t = c?.title(at.pane) || c?.cwd(at.pane)?.split("/").filter(Boolean).pop() || "";
    return t ? `${at.host}: ${t}` : at.host;
  }

  text(): string {
    return this.term?.text() ?? this.note.textContent ?? "";
  }

  focus() {
    this.term?.focus();
  }

  dispose() {
    this.release();
    this.host.remove();
  }
}

registerBlock("remote", (client, id) => new RemoteView(client, id));
