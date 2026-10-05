// The hosts this page can switch between (M4a): the daemon it was loaded
// from (the home daemon) and the others on that daemon's list. Each host
// owns its own layout, and the client connects to whichever is shown
// directly; the home daemon never relays. The last list (and which host was
// shown) is kept in localStorage, so known hosts stay reachable while the
// home daemon is down.
//
// A resident daemon in a sandbox (M4b, a "provider" host) is reached
// through the home daemon's tunnel (`/tunnel/<name>`), which wakes it. If
// it also has a tailnet URL, that's tried once it's awake, and used when it
// answers within about 5s (S4: wake through the provider first, because
// tailnet packets don't wake a sleeping sandbox).

export interface ProviderRef {
  provider: string;
  sandbox: string;
  port: number;
}

export interface Host {
  name: string;
  urls: string[];
  /** A daemon in illogical control's directory: its device id. */
  id?: string;
  /** How it's reached: `tailnet`, straight to its URLs; `dial_out` (M4c),
   * it dials the home daemon and is reached through it at `/h/<name>/…`;
   * `provider` (M4b), a resident daemon in a sandbox, through the home
   * daemon's provider tunnel at `/tunnel/<name>/…` (both on this page's
   * own origin); `ssh` (M51), reached by a terminal's own ssh, never from
   * a page. */
  transport: "tailnet" | "dial_out" | "provider" | "control" | "ssh";
  provider?: ProviderRef;
  /** An ssh host's destination (`user@box`). */
  ssh?: string;
  added_ms: number;
  last_seen_ms: number | null;
  /** A provider host's sandbox state, from the provider (never by
   * connecting): running, warm, cold, gone. */
  status?: string;
}

export interface HostList {
  this: string;
  hosts: Host[];
}

const LIST_KEY = "illogical.hosts";
const SHOWN_KEY = "illogical.host";
const CONTROL_SHOWN_KEY = "illogical.control.host";

function load<T>(key: string): T | null {
  try {
    const v = localStorage.getItem(key);
    return v ? (JSON.parse(v) as T) : null;
  } catch {
    return null;
  }
}

function save(key: string, v: unknown) {
  try {
    localStorage.setItem(key, JSON.stringify(v));
  } catch {
    // Private mode or storage off: just not remembered.
  }
}

export class HostDirectory {
  /** The last list we got (or the cached one until then). */
  list: HostList | null = load<HostList>(LIST_KEY);
  /** The list is the cached one: the home daemon didn't answer. */
  stale = true;
  /** The host shown; `null` is the home daemon (this page's own). */
  shown: string | null = load<string>(SHOWN_KEY);
  /** Provider hosts whose tailnet URL answered: used instead of the tunnel. */
  private upgraded = new Set<string>();
  /** The page is illogical control's (M17): the list comes from control,
   * and there's no home daemon. */
  control = false;
  /** The control this page's daemon joined (`/api/host`'s `control`), if
   * any: the host menu links to its page for the account's other machines
   * (M49), rather than listing them here. */
  joined: string | null = null;
  /** Control mode: how the shown host is reached, for the host chip. */
  path: "direct" | "relayed" | null = null;

  setPath(p: "direct" | "relayed" | null) {
    if (p === this.path) return;
    this.path = p;
    this.emit();
  }
  private listeners = new Set<() => void>();

  constructor() {
    if (this.shown !== null && !this.find(this.shown)) this.shown = null;
    if (this.shown !== null) void this.upgrade(this.shown);
  }

  subscribe(fn: () => void): () => void {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  }

  private emit() {
    for (const fn of this.listeners) fn();
  }

  /** The home daemon's name, once known. */
  get home(): string | null {
    return this.control ? null : (this.list?.this ?? null);
  }

  /** Every host, the home daemon first. */
  /** The hosts this page can switch to. ssh hosts aren't among them: only
   * a terminal reaches those (`sshOnly`). */
  get names(): string[] {
    const reachable = (this.list?.hosts ?? []).filter((h) => h.transport !== "ssh").map((h) => h.name);
    if (this.control) return reachable;
    return this.list ? [this.list.this, ...reachable] : [];
  }

  /** ssh hosts (M51): listed so they aren't a surprise, reached from a
   * terminal with `illogical --host NAME …`. */
  get sshOnly(): Host[] {
    return (this.list?.hosts ?? []).filter((h) => h.transport === "ssh");
  }

  /** Control mode: the daemons this browser checked, from control. */
  setControl(hosts: Host[], stale: boolean) {
    const first = !this.control;
    this.control = true;
    this.list = { this: "", hosts };
    this.stale = stale;
    if (first) this.shown = load<string>(CONTROL_SHOWN_KEY);
    if (this.shown !== null && !this.find(this.shown)) this.shown = null;
    this.emit();
  }

  find(name: string): Host | undefined {
    return this.list?.hosts.find((h) => h.name === name);
  }

  /** What the client prefixes its URLs with: "" for this page's daemon. */
  base(name: string | null = this.shown): string {
    if (this.control) {
      const h = this.find(name ?? this.names[0] ?? "");
      return h?.id ? `e2e:${h.id}` : "";
    }
    if (name === null || name === this.home) return "";
    const h = this.find(name);
    // The one place a host's URL is chosen.
    if (h?.transport === "dial_out") return `/h/${encodeURIComponent(h.name)}`;
    if (h?.transport === "provider" && !this.upgraded.has(name)) return `/tunnel/${encodeURIComponent(h.name)}`;
    return h?.urls[0] ?? "";
  }

  /** Whether the shown host lives in a sandbox that sleeps. */
  get sleeps(): boolean {
    return this.shown !== null && this.find(this.shown)?.transport === "provider";
  }

  /** A provider host with a tailnet URL: once the tunnel has woken it,
   * switch to the tailnet if it answers within about 5s. */
  private async upgrade(name: string) {
    const url = this.find(name)?.transport === "provider" ? this.find(name)?.urls[0] : undefined;
    if (!url || this.upgraded.has(name)) return;
    const until = Date.now() + 5000;
    while (Date.now() < until && this.shown === name) {
      try {
        const res = await fetch(`${url}/api/host`, { signal: AbortSignal.timeout(1500) });
        if (res.ok) {
          this.upgraded.add(name);
          this.emit();
          return;
        }
      } catch {
        // not up yet
      }
      await new Promise((r) => setTimeout(r, 500));
    }
  }

  /** The shown host's name ("" until the home daemon's is known). */
  get current(): string {
    return this.shown ?? this.home ?? this.names[0] ?? "";
  }

  select(name: string) {
    const shown = name === this.home ? null : name;
    if (shown === this.shown) return;
    this.shown = shown;
    save(this.control ? CONTROL_SHOWN_KEY : SHOWN_KEY, shown);
    this.emit();
    if (shown !== null) void this.upgrade(shown);
  }

  /** Fetch the list from the home daemon; on failure keep the cached one. */
  async refresh() {
    if (this.control) return;
    try {
      const res = await fetch("/api/hosts");
      if (!res.ok) throw new Error(String(res.status));
      this.list = (await res.json()) as HostList;
      this.stale = false;
      save(LIST_KEY, this.list);
      // A host that left the list can't stay shown.
      if (this.shown !== null && !this.find(this.shown)) {
        this.shown = null;
        save(SHOWN_KEY, null);
      }
    } catch {
      this.stale = true;
    }
    try {
      const res = await fetch("/api/host");
      if (res.ok) this.joined = ((await res.json()) as { control?: string }).control ?? null;
    } catch {
      // Keep what we knew.
    }
    this.emit();
  }
}

export const directory = new HostDirectory();
