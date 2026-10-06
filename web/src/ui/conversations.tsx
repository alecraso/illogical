// Claude Code conversations (M33): every one on the daemon's machine, from
// a terminal or the desktop app's Code tab, grouped by folder, newest
// first. Picking one shows it as an agent block (stopped, its transcript
// as it grows), or goes to the block that has it already. Opened from a
// pane's menu, the agent dialog and the phone's sheet. On a phone it is a
// full-height sheet.
//
// With more than one host (#78), every host's are listed, grouped by host
// and then folder: each host is asked over the fleet's connection (M25) at
// once and shows up as it answers. One that's asleep isn't woken to be
// asked, and one that doesn't answer in a few seconds says so. Opening
// one of another host's opens it on that host and shows it there.

import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "preact/hooks";
import type { ApiResponse, Client } from "../client";
import { directory } from "../hosts";
import type { OpenConversationRequest, OpenConversationResponse, PaneId } from "../proto";
import { getFleet } from "./hosts";

export interface Conversation {
  id: string;
  cwd: string;
  cwd_exists: boolean;
  branch?: string;
  source: "terminal" | "desktop" | "other";
  title: string;
  first_prompt?: string;
  last_prompt?: string;
  model?: string;
  updated_ms: number;
  forked_from?: string;
  archived: boolean;
  live?: { pid: number; pane?: PaneId; block?: PaneId; place: string; status: string };
  /** The agent block that has it open. */
  block: PaneId | null;
}

export interface ConversationsWhere {
  /** Split this block (desktop), else a new tab in `session`. */
  split?: PaneId;
  session?: number;
  /** Folders under this one first. */
  cwd?: string;
}

let open: { client: Client; where: ConversationsWhere; phone: boolean } | null = null;
const listeners = new Set<() => void>();
const changed = () => listeners.forEach((fn) => fn());

/** Show the conversations picker. */
export function pickConversation(client: Client, where: ConversationsWhere, phone = false) {
  open = { client, where, phone };
  changed();
}

export function ConversationsLayer() {
  const [, setTick] = useState(0);
  useEffect(() => {
    const fn = () => setTick((t) => t + 1);
    listeners.add(fn);
    return () => {
      listeners.delete(fn);
    };
  }, []);
  if (!open) return null;
  const { client, where, phone } = open;
  return (
    <Picker
      client={client}
      where={where}
      phone={phone}
      close={() => {
        open = null;
        changed();
      }}
    />
  );
}

function ago(ms: number): string {
  const s = Math.max(0, (Date.now() - ms) / 1000);
  if (s < 60) return "now";
  if (s < 3600) return `${Math.floor(s / 60)}m`;
  if (s < 86400) return `${Math.floor(s / 3600)}h`;
  if (s < 86400 * 30) return `${Math.floor(s / 86400)}d`;
  return new Date(ms).toLocaleDateString();
}

/** `~/dev/x` for `/home/me/dev/x` (the daemon's home, guessed from paths). */
function tilde(path: string, home: string | null): string {
  return home && (path === home || path.startsWith(home + "/")) ? "~" + path.slice(home.length) : path;
}

function homeOf(list: Conversation[]): string | null {
  for (const c of list) {
    const m = /^(\/home\/[^/]+|\/Users\/[^/]+|\/root)(\/|$)/.exec(c.cwd);
    if (m) return m[1];
  }
  return null;
}

const SOURCE = { terminal: "Terminal", desktop: "Desktop", other: "Other" } as const;

/** One host's conversations, or why there are none. */
interface HostConvs {
  host: string;
  /** Asked, no answer yet; answered; asleep (not asked); or away. */
  state: "reading" | "ok" | "asleep" | "away";
  list: Conversation[];
  note?: string;
}

/** How long a host gets to answer before it's shown as not answering. */
const HOST_MS = 5000;

function within<T>(p: Promise<T>, ms: number): Promise<T> {
  return new Promise((resolve, reject) => {
    const t = setTimeout(() => reject(new Error("not answering")), ms);
    p.then(
      (v) => (clearTimeout(t), resolve(v)),
      (e) => (clearTimeout(t), reject(e)),
    );
  });
}

type Row =
  | { kind: "host"; h: HostConvs }
  | { kind: "folder"; cwd: string; host: string; home: string | null }
  | { kind: "conv"; c: Conversation; host: string };

function Picker({ client, where, phone, close }: { client: Client; where: ConversationsWhere; phone: boolean; close: () => void }) {
  const [hosts, setHosts] = useState<HostConvs[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [all, setAll] = useState(false);
  const [liveOnly, setLiveOnly] = useState(false);
  const [sel, setSel] = useState(0);
  const [busy, setBusy] = useState(false);
  const input = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLUListElement>(null);
  const loads = useRef(0);

  // This page's host, and with a fleet the others (#78).
  const fleet = getFleet();
  const here = directory.current;
  const multi = !!fleet && fleet.list.length > 1;

  const load = () => {
    const gen = ++loads.current;
    setError(null);
    const q = new URLSearchParams({ limit: "1000" });
    if (all) q.set("all", "1");
    if (liveOnly) q.set("live", "1");
    const path = `/api/conversations?${q}`;
    const others = multi ? fleet!.list.filter((h) => h.name !== here) : [];
    const start: HostConvs[] = [
      { host: here, state: "reading", list: [] },
      ...others.map((h): HostConvs => {
        if (h.state === "asleep") return { host: h.name, state: "asleep", list: [], note: "asleep" };
        if (h.state === "capped") return { host: h.name, state: "away", list: [], note: "not live (too many machines)" };
        return { host: h.name, state: "reading", list: [] };
      }),
    ];
    setHosts(start);
    const set = (h: HostConvs) => gen === loads.current && setHosts((cur) => cur && cur.map((x) => (x.host === h.host ? h : x)));
    for (const h of start) {
      if (h.state !== "reading") continue;
      const ask: Promise<ApiResponse> = h.host === here ? client.request("GET", path) : fleet!.request(h.host, "GET", path);
      within(ask, HOST_MS)
        .then(async (res) => {
          if (!res.ok) throw new Error(`couldn't list them (${res.status})`);
          set({ host: h.host, state: "ok", list: (await res.json<{ conversations: Conversation[] }>()).conversations });
        })
        .catch((e: Error) => {
          if (!multi && gen === loads.current) setError(e.message);
          const st = fleet?.host(h.host)?.state;
          const why = st === "stale" || st === "offline" ? `${st} · ${e.message}` : e.message;
          set({ host: h.host, state: "away", list: [], note: why });
        });
    }
  };
  useEffect(() => {
    load();
  }, [all, liveOnly]);
  useLayoutEffect(() => {
    if (!phone) input.current?.focus();
  }, []);

  // Each host's folders by their newest conversation; this host first,
  // and in it the folder we came from.
  const rows: Row[] = useMemo(() => {
    if (!hosts) return [];
    const words = query.toLowerCase().split(/\s+/).filter(Boolean);
    const out: Row[] = [];
    for (const h of hosts) {
      if (multi) out.push({ kind: "host", h });
      const home = homeOf(h.list);
      const hits = h.list.filter((c) => {
        const hay = `${c.title} ${c.first_prompt ?? ""} ${c.last_prompt ?? ""} ${c.cwd}`.toLowerCase();
        return words.every((w) => hay.includes(w));
      });
      const groups = new Map<string, Conversation[]>();
      for (const c of hits) {
        const g = groups.get(c.cwd) ?? [];
        g.push(c);
        groups.set(c.cwd, g);
      }
      const cwd = h.host === here ? where.cwd : undefined;
      const near = (a: string) => (cwd && (cwd === a || cwd.startsWith(a + "/") || a.startsWith(cwd + "/")) ? 1 : 0);
      const order = [...groups.keys()].sort((a, b) => {
        if (near(a) !== near(b)) return near(b) - near(a);
        return groups.get(b)![0].updated_ms - groups.get(a)![0].updated_ms;
      });
      for (const f of order) {
        out.push({ kind: "folder", cwd: f, host: h.host, home });
        for (const c of groups.get(f)!) out.push({ kind: "conv", c, host: h.host });
      }
    }
    return out;
  }, [hosts, query, where.cwd, here, multi]);
  const convs = rows.filter((r): r is Extract<Row, { kind: "conv" }> => r.kind === "conv");
  const reading = !!hosts && hosts.some((h) => h.state === "reading");

  useEffect(() => {
    listRef.current?.querySelector(".picker-row.selected")?.scrollIntoView({ block: "nearest" });
  }, [sel, rows]);

  /** The pane it runs in, if that's one of its host's panes. */
  const inPane = (c: Conversation, host: string): boolean => {
    const p = c.live?.pane;
    if (p === undefined) return false;
    if (host === here) return !!client.info(p);
    return !!fleet?.panes.some((x) => x.host === host && x.id === p && !x.stale);
  };

  const pick = async (c: Conversation, host: string, then?: "continue" | "fork") => {
    if (busy) return;
    if (host !== here && fleet) return pickThere(c, host, then);
    if (c.block !== null && !then) {
      client.focusPane(c.block);
      close();
      return;
    }
    if (c.live?.pane !== undefined && inPane(c, host) && !then) {
      // It's running in one of our panes: that's where it goes on.
      client.focusPane(c.live.pane);
      close();
      return;
    }
    setBusy(true);
    const block = await client.openConversation({ id: c.id, then, split: where.split, session: where.session });
    setBusy(false);
    if (block !== null) close();
  };

  // Another host's: opened there (its first session, a tab of its own),
  // then shown there, as the swarm opens a pane on its host.
  const pickThere = async (c: Conversation, host: string, then?: "continue" | "fork") => {
    const f = fleet!;
    if (!then && (c.block !== null || inPane(c, host))) {
      f.open(host, c.block ?? c.live!.pane!);
      close();
      return;
    }
    setBusy(true);
    try {
      const res = await within(f.request(host, "POST", "/api/conversations/open", { id: c.id, then: then ?? null } satisfies OpenConversationRequest), HOST_MS);
      const v = await res.json<Partial<OpenConversationResponse>>().catch(() => null);
      if (!res.ok || typeof v?.block !== "number") {
        client.toast(v?.error ?? `couldn't open it on ${host} (${res.status})`);
        return;
      }
      if (v.error) client.toast(v.error);
      f.open(host, v.block);
      close();
    } catch (e) {
      client.toast(`couldn't open it on ${host}: ${(e as Error).message}`);
    } finally {
      setBusy(false);
    }
  };

  const onKey = (e: KeyboardEvent) => {
    if (e.key === "Escape") {
      e.preventDefault();
      close();
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      setSel((s) => Math.min(convs.length - 1, s + 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setSel((s) => Math.max(0, s - 1));
    } else if (e.key === "Enter") {
      e.preventDefault();
      const r = convs[sel];
      if (r) void pick(r.c, r.host);
    }
  };

  const chosenRow = convs[sel];
  const chosen = chosenRow?.c;
  const chosenHost = chosenRow?.host ?? here;
  let i = -1;
  return (
    <div
      class="prompt-backdrop picker-backdrop"
      role="dialog"
      aria-label="Claude Code conversations"
      onPointerDown={(e) => e.target === e.currentTarget && close()}
    >
      <div class={phone ? "picker conversations phone" : "picker conversations"} onKeyDown={onKey}>
        <div class="picker-where">
          <span class="picker-host">Claude Code conversations</span>
          <label class="check">
            <input type="checkbox" checked={liveOnly} onChange={(e) => setLiveOnly(e.currentTarget.checked)} />
            Open now
          </label>
          <label class="check" title="claude -p and SDK runs, archived ones, ones whose folder is gone">
            <input type="checkbox" checked={all} onChange={(e) => setAll(e.currentTarget.checked)} />
            All
          </label>
        </div>
        <input
          ref={input}
          class="picker-filter"
          placeholder="Search titles, prompts and folders"
          value={query}
          autocomplete="off"
          autocapitalize="off"
          spellcheck={false}
          onInput={(e) => {
            setQuery(e.currentTarget.value);
            setSel(0);
          }}
        />
        <ul class="picker-list" role="listbox" ref={listRef}>
          {!multi && reading && !error && <li class="picker-empty">Reading…</li>}
          {!multi && hosts && !reading && convs.length === 0 && <li class="picker-empty">{query ? "Nothing matches" : "No conversations here"}</li>}
          {rows.map((r) => {
            if (r.kind === "host") {
              const h = r.h;
              const n = h.list.length;
              const what = h.state === "reading" ? "reading…" : h.state === "ok" ? `${n} conversation${n === 1 ? "" : "s"}` : (h.note ?? "");
              return (
                <li key={`h:${h.host}`} class={`conv-host ${h.state}`} data-host={h.host}>
                  <span class="conv-host-name">{h.host}</span>
                  <span class="conv-host-state">{what}</span>
                </li>
              );
            }
            if (r.kind === "folder") {
              return (
                <li key={`f:${r.host}:${r.cwd}`} class="conv-folder" title={r.cwd}>
                  {tilde(r.cwd, r.home) || "(no folder)"}
                </li>
              );
            }
            const { c, host } = r;
            i += 1;
            const n = i;
            return (
              <li
                key={`${host}:${c.id}`}
                role="option"
                aria-selected={n === sel}
                class={`picker-row conv-row${n === sel ? " selected" : ""}`}
                data-conversation={c.id}
                data-host={host}
                onClick={() => {
                  setSel(n);
                  void pick(c, host);
                }}
                title={c.first_prompt ?? c.title}
              >
                <span class={`conv-source ${c.source}`}>{SOURCE[c.source]}</span>
                <span class="picker-label conv-title">{c.title}</span>
                {c.live && <span class="conv-live" title={c.live.place}>● {c.live.pane !== undefined ? `%${c.live.pane}` : "open"}</span>}
                {c.block !== null && <span class="host-tag">%{c.block}</span>}
                <span class="conv-when">{ago(c.updated_ms)}</span>
              </li>
            );
          })}
        </ul>
        {error && <p class="error picker-error">{error}</p>}
        <div class="picker-actions">
          <span class="picker-target" title={chosen?.live?.place ?? chosen?.cwd}>
            {chosen ? (chosen.live ? chosen.live.place : (chosen.last_prompt ?? chosen.first_prompt ?? "")) : ""}
          </span>
          <button class="primary" disabled={!chosen || busy} onClick={() => chosen && void pick(chosen, chosenHost)}>
            {chosen?.block != null ? "Go to it" : chosen && inPane(chosen, chosenHost) ? "Go to pane" : "Open"}
          </button>
          <button
            disabled={!chosen || busy || !!chosen.live || chosen.block !== null}
            title={chosen?.live ? `It's ${chosen.live.place}: fork it instead` : undefined}
            onClick={() => chosen && void pick(chosen, chosenHost, "continue")}
          >
            Continue
          </button>
          <button disabled={!chosen || busy} title="A new session with its history; the original is left alone" onClick={() => chosen && void pick(chosen, chosenHost, "fork")}>
            Fork
          </button>
          <button onClick={close}>Cancel</button>
        </div>
      </div>
    </div>
  );
}
