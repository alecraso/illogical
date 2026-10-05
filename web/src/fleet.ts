// Every host at once (M25): a summaries-only connection (M23) to each
// machine in the directory, merged into one list of panes, where a pane is
// (host, pane id). The tab view still connects for real to the one host it
// shows; this only knows what every pane is up to, which is what the swarm
// draws. Control never sees terminal content, so the merge happens here.
//
// - Each host keeps its own transport (tailnet, dial-out, provider tunnel,
//   or an end-to-end channel through control). Relayed ones share one
//   socket to control (see e2e/relaymux.ts).
// - A host is `connected`, `stale` (it dropped; its panes stay, greyed,
//   from the last summary, with when it was last heard), `offline` (never
//   reached, or gone a while), `asleep` (a sandbox whose provider says it
//   sleeps: never woken just to be counted) or `capped` (over the
//   connection cap: shown from what was last known).
// - Connects go through a small limiter with jitter, so a laptop waking
//   with 20 hosts doesn't open 20 sockets at once, and a slow or dead host
//   only holds its own slot for a few seconds.
// - A heartbeat notices a link that died without closing.

import { Client } from "./client";
import type { Driver, PaneInfo, Presence, State, ThreadSummary } from "./proto";
import { RelayMux } from "./e2e/relaymux";

export type HostStatus = "connecting" | "connected" | "stale" | "offline" | "asleep" | "capped";

/** What the fleet needs to know about a host from the directory. */
export interface HostRef {
  name: string;
  /** Its daemon id in control's directory, if it has one. */
  id?: string;
  transport: string;
  /** A provider's word for a sandbox's state (`running`, `warm`, `cold`). */
  status?: string;
  /** Whose it is, when it isn't this person's (M19): their name, and the
   * team it belongs to. Absent: yours. */
  owner?: string;
  team?: string | null;
  /** M30: the owner's account id (`account:<id>` is their principal), and
   * the team's name. */
  ownerId?: string;
  teamName?: string;
}

/** M30: whose a pane is, for clustering by person: you, a teammate (by
 * account), or a team (a team's own machine). */
export interface Person {
  /** `me`, `account:<id>` or `team:<id>`. */
  id: string;
  name: string;
  kind: "me" | "person" | "team";
}

export interface FleetHost extends HostRef {
  state: HostStatus;
  /** When it last answered (ms), or null. */
  lastSeen: number | null;
  /** The last summary it sent: still shown while it's away. */
  summary: State | null;
  /** How it's reached now, when connected through control. */
  path: "direct" | "relayed" | null;
}

/** One pane of the merged list. */
export interface FleetPane {
  /** `host:pane`, unique across the fleet. */
  key: string;
  host: string;
  id: number;
  info: PaneInfo;
  session: { id: number; name: string } | null;
  /** Its host isn't live: draw it greyed. */
  stale: boolean;
  owner?: string;
  team?: string | null;
  /** M30: whose it is (its session's owner, which is its machine's). */
  person: Person;
  /** M30: who drives it (M13), if anyone does. */
  driver: Driver | null;
  /** M30: who has it open now (M13 presence), other than summaries. */
  watchers: Presence[];
  /** M61: messages in its thread this person hasn't read, and whether
   * one mentions them. */
  unread: number;
  mention: boolean;
}

/** Most summary connections one page holds (S16: about 14 MB a page for
 * 20 daemons; the cap is about connections, not memory). */
export const CAP = 24;
/** Connects started at once. */
const SLOTS = 4;
/** A connect holds its slot at most this long (a dead host). */
const SLOT_MS = 3000;
/** Spread reconnects after a wake over this. */
const SPREAD_MS = 1500;
/** Ask quiet hosts to answer this often... */
const HEARTBEAT_MS = 3000;
/** ...and give up on a link that said nothing for this long. */
const SILENT_MS = 6000;
/** A host away this long is offline, not just stale. */
const OFFLINE_MS = 60_000;
/** Hidden this long, the page lets go of sandboxes (as the tab view does). */
const HIDDEN_GRACE_MS = 10_000;
const CACHE_KEY = "illogical.fleet";

interface Entry {
  ref: HostRef;
  client: Client | null;
  state: HostStatus;
  lastSeen: number | null;
  summary: State | null;
  lostAt: number | null;
  /** When it was last opened or looked at, for the cap. */
  usedAt: number;
  off: (() => void) | null;
}

export class Fleet {
  private hosts = new Map<string, Entry>();
  private listeners = new Set<() => void>();
  /** Clients waiting to connect, in order, each once. */
  private queue: Client[] = [];
  /** Clients connecting now, and how to give back their slot. */
  private trying = new Map<Client, () => void>();
  private timer: number | undefined;
  private lastBeat = Date.now();
  private hidden: number | undefined;
  /** The page is hidden: sandboxes aren't held awake, or woken. */
  private away = false;
  private merged: FleetPane[] | null = null;
  /** Shown when the cap leaves hosts out. */
  notice: string | null = null;
  /** M30: what to call this person ("me" otherwise). */
  me = "me";
  /** Connects started, and how many failed, since the last wake (for the
   * tests and the S16 comparison). */
  stats = { started: 0, wakeAt: 0, allBackMs: null as number | null };
  private failedBefore = 0;

  /** Tries that ended without connecting, since the last wake. */
  get failures(): number {
    let n = 0;
    for (const e of this.hosts.values()) n += e.client?.failures ?? 0;
    return n - this.failedBefore;
  }

  /** `make` opens a summaries-only client to a host (null: can't). */
  private make: (h: HostRef) => Client | null;
  constructor(make: (h: HostRef) => Client | null) {
    this.make = make;
    try {
      const cached = JSON.parse(localStorage.getItem(CACHE_KEY) ?? "{}") as Record<string, State>;
      for (const [name, summary] of Object.entries(cached)) {
        this.hosts.set(name, {
          ref: { name, transport: "unknown" },
          client: null,
          state: "offline",
          lastSeen: null,
          summary,
          lostAt: null,
          usedAt: 0,
          off: null,
        });
      }
    } catch {
      // nothing saved
    }
  }

  subscribe(fn: () => void): () => void {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  }

  private emit() {
    this.merged = null;
    for (const fn of this.listeners) fn();
  }

  /** The directory changed: these are the hosts now. */
  setHosts(refs: HostRef[]) {
    const names = new Set(refs.map((r) => r.name));
    for (const [name, e] of this.hosts) {
      if (!names.has(name)) {
        this.stop(e);
        this.hosts.delete(name);
      }
    }
    for (const ref of refs) {
      const e = this.hosts.get(ref.name);
      if (e) {
        const moved = e.ref.id !== ref.id || e.ref.transport !== ref.transport;
        e.ref = ref;
        if (moved) this.stop(e);
      } else {
        this.hosts.set(ref.name, {
          ref,
          client: null,
          state: "offline",
          lastSeen: null,
          summary: null,
          lostAt: null,
          usedAt: 0,
          off: null,
        });
      }
    }
    this.plan();
    this.emit();
  }

  /** Begin: watch for wakes and quiet links. */
  start() {
    if (this.timer !== undefined) return;
    this.timer = window.setInterval(() => this.tick(), 1000);
    document.addEventListener("visibilitychange", () => {
      clearTimeout(this.hidden);
      if (document.visibilityState === "visible") {
        this.away = false;
        this.wake();
      } else this.hidden = window.setTimeout(() => this.letGo(), HIDDEN_GRACE_MS);
    });
    window.addEventListener("online", () => this.wake());
  }

  /** Who to connect to, within the cap; asleep sandboxes stay asleep. */
  private plan() {
    const live = [...this.hosts.values()].filter((e) => !this.asleep(e));
    live.sort((a, b) => b.usedAt - a.usedAt || a.ref.name.localeCompare(b.ref.name));
    const keep = new Set(live.slice(0, CAP).map((e) => e.ref.name));
    const capped = live.length - keep.size;
    this.notice = capped > 0 ? `Live summaries for ${CAP} of ${live.length} machines: the other ${capped} refresh when you open them.` : null;
    for (const e of this.hosts.values()) {
      if (this.asleep(e)) {
        this.stop(e);
        e.state = "asleep";
      } else if (!keep.has(e.ref.name)) {
        this.stop(e);
        e.state = "capped";
      } else if (!e.client) {
        this.begin(e);
      }
    }
  }

  /** A sandbox sleeps when nothing holds it awake, and a summary
   * connection does: let go of those while the page is hidden. */
  private letGo() {
    this.away = true;
    for (const e of this.hosts.values()) if (e.ref.transport === "provider") e.client?.sleep();
  }

  private asleep(e: Entry): boolean {
    return e.ref.transport === "provider" && !!e.ref.status && e.ref.status !== "running";
  }

  private begin(e: Entry) {
    const c = this.make(e.ref);
    if (!c) return;
    e.client = c;
    e.state = "connecting";
    c.schedule = (_connect, delay) => void setTimeout(() => this.enqueue(c), delay + Math.random() * 250);
    e.off = c.subscribe(() => this.update(e));
    this.enqueue(c);
  }

  private stop(e: Entry) {
    e.off?.();
    e.off = null;
    if (e.client) {
      this.queue = this.queue.filter((c) => c !== e.client);
      this.trying.get(e.client)?.();
      e.client.close();
    }
    e.client = null;
  }

  /** Connect `c` when a slot is free. */
  private enqueue(c: Client) {
    if (!this.queue.includes(c)) this.queue.push(c);
    this.pump();
  }

  private pump() {
    while (this.trying.size < SLOTS && this.queue.length) {
      const c = this.queue.shift()!;
      // Connected meanwhile, or already trying: nothing to do.
      if (c.linked || this.trying.has(c)) continue;
      // Hidden: a sandbox's reconnect would wake it.
      if (this.away && [...this.hosts.values()].some((e) => e.client === c && e.ref.transport === "provider")) continue;
      this.stats.started++;
      let freed = false;
      const free = () => {
        if (freed) return;
        freed = true;
        clearTimeout(timer);
        this.trying.delete(c);
        this.pump();
      };
      // Given back when it connects or fails (update), or after SLOT_MS:
      // a host that doesn't answer doesn't hold up the rest.
      const timer = window.setTimeout(free, SLOT_MS);
      this.trying.set(c, free);
      c.wake();
    }
  }

  private update(e: Entry) {
    const c = e.client;
    if (!c) return;
    const was = e.state;
    // Its try is over, either way: the next one may go.
    if (c.connected || !c.linked) this.trying.get(c)?.();
    if (c.revoked) {
      // M30: access was removed (a share revoked, a member removed, a team
      // locked): what it showed goes, it isn't kept greyed.
      e.summary = null;
      e.state = "offline";
      e.lostAt = Date.now();
      this.save();
    } else if (c.connected) {
      e.state = "connected";
      e.lastSeen = Date.now();
      e.lostAt = null;
      e.summary = c.state;
      if (this.stats.wakeAt && this.stats.allBackMs === null && this.everyoneBack()) {
        this.stats.allBackMs = Date.now() - this.stats.wakeAt;
      }
    } else if (was === "connected") {
      e.state = "stale";
      e.lostAt = Date.now();
      this.save();
    }
    this.emit();
  }

  private everyoneBack(): boolean {
    return [...this.hosts.values()].every((e) => !e.client || e.client.connected);
  }

  private tick() {
    const now = Date.now();
    // A long gap between ticks: the machine slept.
    if (now - this.lastBeat > 5000) this.wake();
    this.lastBeat = now;
    let changed = false;
    for (const e of this.hosts.values()) {
      const c = e.client;
      if (c?.connected) {
        if (now - c.lastHeard > SILENT_MS) {
          // Gone quiet without closing: treat it as dropped.
          c.drop();
          changed = true;
        } else if (now - c.lastHeard > HEARTBEAT_MS) {
          c.heartbeat();
        }
      }
      if ((e.state === "stale" || e.state === "connecting") && e.lostAt !== null && now - e.lostAt > OFFLINE_MS) {
        e.state = "offline";
        changed = true;
      }
    }
    if (changed) this.emit();
  }

  /** After a sleep or a network change: reconnect everyone that's down,
   * spread out. */
  wake() {
    this.stats = { started: 0, wakeAt: Date.now(), allBackMs: null };
    this.failedBefore = 0;
    this.failedBefore = this.failures;
    for (const e of this.hosts.values()) {
      const c = e.client;
      if (!c) continue;
      if (c.connected) {
        // It may have died while we slept: ask, and the heartbeat decides.
        c.heartbeat();
        continue;
      }
      window.setTimeout(() => this.enqueue(c), Math.random() * SPREAD_MS);
    }
  }

  /** Simulate a sleep for tests: drop every link at once. */
  sleepAll() {
    for (const e of this.hosts.values()) e.client?.drop();
    RelayMux.closeAll();
  }

  /** Shows a pane in the tab view: set by the page. */
  onOpen: (host: string, pane: number) => void = () => {};

  /** Open a pane of the merged list in its tab, connected for real (a
   * sleeping sandbox wakes for it). */
  open(host: string, pane: number) {
    this.touch(host);
    this.onOpen(host, pane);
  }

  /** Someone opened this host (or a pane on it): it counts as used. */
  touch(name: string) {
    const e = this.hosts.get(name);
    if (!e) return;
    e.usedAt = Date.now();
    if (e.state === "capped") this.plan();
  }

  private save() {
    try {
      const out: Record<string, State> = {};
      for (const [name, e] of this.hosts) if (e.summary) out[name] = e.summary;
      localStorage.setItem(CACHE_KEY, JSON.stringify(out));
    } catch {
      // too big, or storage off: kept in this page only
    }
  }

  /** Every host, with its state. */
  get list(): FleetHost[] {
    return [...this.hosts.values()].map((e) => ({
      ...e.ref,
      state: e.state,
      lastSeen: e.lastSeen,
      summary: e.summary,
      path: e.client?.path ?? null,
    }));
  }

  /** An API request to one host, over its summary connection (M26: the
   * swarm acts on panes without opening them). */
  async request(host: string, method: string, path: string, body?: unknown) {
    const c = this.hosts.get(host)?.client;
    if (!c) throw new Error(`${host} isn't connected`);
    return c.request(method, path, body);
  }

  /** A host's summary connection, for its threads (the chat view). */
  clientOf(host: string): Client | null {
    return this.hosts.get(host)?.client ?? null;
  }

  /** Follow an editor on a host (M28). */
  follow(host: string, pane: number, fn: (m: import("./proto").FollowMsg) => void): () => void {
    const c = this.hosts.get(host)?.client;
    return c ? c.followEditor(pane, fn) : () => {};
  }

  refollow(host: string, pane: number) {
    this.hosts.get(host)?.client?.refollowEditor(pane);
  }

  /** A pane operation on a host (asking its owner for trust, say). */
  paneOp(host: string, pane: number, op: import("./proto").PaneOp) {
    this.hosts.get(host)?.client?.paneOp(pane, op);
  }

  /** This person's role in a pane's session on its host (M12): `owner`
   * unless the host said otherwise. */
  role(p: FleetPane): "viewer" | "editor" | "owner" {
    const roles = this.hosts.get(p.host)?.summary?.roles;
    if (!roles) return "owner";
    return roles.find(([s]) => s === p.session?.id)?.[1] ?? "viewer";
  }

  /** Who else is on a host, and where they look (M13). */
  presence(host: string) {
    return this.hosts.get(host)?.summary?.presence ?? [];
  }

  /** This client's principal id on a host. */
  meOn(host: string): string {
    const e = this.hosts.get(host);
    return e?.summary?.presence?.find((p) => p.client === e.client?.clientId)?.who ?? "owner";
  }

  host(name: string): FleetHost | undefined {
    return this.list.find((h) => h.name === name);
  }

  /** How many summary connections are open (or opening). */
  get connections(): number {
    return [...this.hosts.values()].filter((e) => e.client).length;
  }

  private injected: FleetPane[] = [];
  /** Made-up panes drawn beside the real ones (M26's synthetic fleet, for
   * the frame-rate check and screenshots). */
  inject(panes: FleetPane[]) {
    this.injected = panes;
    this.emit();
  }

  /** M30: whose a host's panes are. */
  personOf(ref: HostRef): Person {
    if (ref.team) return { id: `team:${ref.team}`, name: ref.teamName ?? ref.team, kind: "team" };
    if (ref.ownerId) return { id: `account:${ref.ownerId}`, name: ref.owner ?? ref.ownerId, kind: "person" };
    return { id: "me", name: this.me, kind: "me" };
  }

  /** M30: the panes grouped by person (you first, then people, then
   * teams), for "cluster by person". */
  byPerson(): { person: Person; panes: FleetPane[] }[] {
    const groups = new Map<string, { person: Person; panes: FleetPane[] }>();
    for (const p of this.panes) (groups.get(p.person.id) ?? groups.set(p.person.id, { person: p.person, panes: [] }).get(p.person.id)!).panes.push(p);
    const rank = { me: 0, person: 1, team: 2 };
    return [...groups.values()].sort((a, b) => rank[a.person.kind] - rank[b.person.kind] || a.person.name.localeCompare(b.person.name));
  }

  /** Every pane on every host, as last known. Someone else's private pane
   * (M14) is left out: it shows only that it's there. */
  get panes(): FleetPane[] {
    if (this.merged) return this.merged;
    const out: FleetPane[] = [];
    for (const [name, e] of this.hosts) {
      const st = e.summary;
      if (!st) continue;
      const sessionOf = new Map<number, { id: number; name: string }>();
      for (const s of st.sessions) for (const t of s.tabs) sessionOf.set(t, { id: s.id, name: s.name });
      const tabOf = new Map<number, number>();
      for (const t of st.tabs) for (const [p] of t.layout.panes) tabOf.set(p, t.id);
      const person = this.personOf(e.ref);
      const watchers = new Map<number, Presence[]>();
      for (const p of st.presence ?? []) if (p.pane !== undefined) (watchers.get(p.pane) ?? watchers.set(p.pane, []).get(p.pane)!).push(p);
      const threads = new Map<number, ThreadSummary>();
      for (const t of st.threads ?? []) if ("pane" in t.target) threads.set(t.target.pane, t);
      for (const info of st.panes) {
        // Someone else's private pane (M14): not even a tile.
        if (info.private && st.roles) continue;
        // A pane from another host in this layout (#17): its own host lists it.
        if (info.type === "remote") continue;
        out.push({
          key: `${name}:${info.id}`,
          host: name,
          id: info.id,
          info,
          session: sessionOf.get(tabOf.get(info.id) ?? -1) ?? null,
          stale: e.state !== "connected",
          owner: e.ref.owner,
          team: e.ref.team,
          person,
          driver: info.driver ?? null,
          watchers: watchers.get(info.id) ?? [],
          unread: threads.get(info.id)?.unread ?? 0,
          mention: !!threads.get(info.id)?.mention,
        });
      }
    }
    out.push(...this.injected);
    this.merged = out;
    return out;
  }
}
