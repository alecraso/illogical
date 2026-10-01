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
  transport: "tailnet" | "provider";
  provider?: ProviderRef;
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
    return this.list?.this ?? null;
  }

  /** Every host, the home daemon first. */
  get names(): string[] {
    return this.list ? [this.list.this, ...this.list.hosts.map((h) => h.name)] : [];
  }

  find(name: string): Host | undefined {
    return this.list?.hosts.find((h) => h.name === name);
  }

  /** What the client prefixes its URLs with: "" for this page's daemon. */
  base(name: string | null = this.shown): string {
    if (name === null || name === this.home) return "";
    const h = this.find(name);
    if (h?.transport === "provider" && !this.upgraded.has(name)) {
      return `${location.origin}/tunnel/${encodeURIComponent(name)}`;
    }
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
    return this.shown ?? this.home ?? "";
  }

  select(name: string) {
    const shown = name === this.home ? null : name;
    if (shown === this.shown) return;
    this.shown = shown;
    save(SHOWN_KEY, shown);
    this.emit();
    if (shown !== null) void this.upgrade(shown);
  }

  /** Fetch the list from the home daemon; on failure keep the cached one. */
  async refresh() {
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
    this.emit();
  }
}

export const directory = new HostDirectory();
