// Claude Code conversations (M33): every one on the daemon's machine, from
// a terminal or the desktop app's Code tab, grouped by folder, newest
// first. Picking one shows it as an agent block (stopped, its transcript
// as it grows), or goes to the block that has it already. Opened from a
// pane's menu, the agent dialog and the phone's sheet. On a phone it is a
// full-height sheet.

import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "preact/hooks";
import type { Client } from "../client";
import type { PaneId } from "../proto";

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

type Row = { kind: "folder"; cwd: string } | { kind: "conv"; c: Conversation };

function Picker({ client, where, phone, close }: { client: Client; where: ConversationsWhere; phone: boolean; close: () => void }) {
  const [list, setList] = useState<Conversation[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [all, setAll] = useState(false);
  const [liveOnly, setLiveOnly] = useState(false);
  const [sel, setSel] = useState(0);
  const [busy, setBusy] = useState(false);
  const input = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLUListElement>(null);

  const load = async () => {
    setError(null);
    try {
      const q = new URLSearchParams({ limit: "1000" });
      if (all) q.set("all", "1");
      if (liveOnly) q.set("live", "1");
      const res = await client.request("GET", `/api/conversations?${q}`);
      if (!res.ok) throw new Error(`couldn't list them (${res.status})`);
      setList((await res.json<{ conversations: Conversation[] }>()).conversations);
    } catch (e) {
      setError((e as Error).message);
    }
  };
  useEffect(() => {
    void load();
  }, [all, liveOnly]);
  useLayoutEffect(() => {
    if (!phone) input.current?.focus();
  }, []);

  const home = useMemo(() => (list ? homeOf(list) : null), [list]);
  // Folders by their newest conversation; the one we came from first.
  const rows: Row[] = useMemo(() => {
    if (!list) return [];
    const words = query.toLowerCase().split(/\s+/).filter(Boolean);
    const hits = list.filter((c) => {
      const hay = `${c.title} ${c.first_prompt ?? ""} ${c.last_prompt ?? ""} ${c.cwd}`.toLowerCase();
      return words.every((w) => hay.includes(w));
    });
    const groups = new Map<string, Conversation[]>();
    for (const c of hits) {
      const g = groups.get(c.cwd) ?? [];
      g.push(c);
      groups.set(c.cwd, g);
    }
    const here = where.cwd;
    const order = [...groups.keys()].sort((a, b) => {
      const ah = here && (here === a || here.startsWith(a + "/") || a.startsWith(here + "/")) ? 1 : 0;
      const bh = here && (here === b || here.startsWith(b + "/") || b.startsWith(here + "/")) ? 1 : 0;
      if (ah !== bh) return bh - ah;
      return groups.get(b)![0].updated_ms - groups.get(a)![0].updated_ms;
    });
    const out: Row[] = [];
    for (const cwd of order) {
      out.push({ kind: "folder", cwd });
      for (const c of groups.get(cwd)!) out.push({ kind: "conv", c });
    }
    return out;
  }, [list, query, where.cwd]);
  const convs = rows.filter((r): r is { kind: "conv"; c: Conversation } => r.kind === "conv");

  useEffect(() => {
    listRef.current?.querySelector(".picker-row.selected")?.scrollIntoView({ block: "nearest" });
  }, [sel, rows]);

  const pick = async (c: Conversation, then?: "continue" | "fork") => {
    if (busy) return;
    if (c.block !== null && !then) {
      client.focusPane(c.block);
      close();
      return;
    }
    if (c.live?.pane !== undefined && client.info(c.live.pane) && !then) {
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
      const c = convs[sel]?.c;
      if (c) void pick(c);
    }
  };

  const chosen = convs[sel]?.c;
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
          {!list && !error && <li class="picker-empty">Reading…</li>}
          {list && convs.length === 0 && <li class="picker-empty">{query ? "Nothing matches" : "No conversations here"}</li>}
          {rows.map((r) => {
            if (r.kind === "folder") {
              return (
                <li key={`f:${r.cwd}`} class="conv-folder" title={r.cwd}>
                  {tilde(r.cwd, home) || "(no folder)"}
                </li>
              );
            }
            const c = r.c;
            i += 1;
            const n = i;
            return (
              <li
                key={c.id}
                role="option"
                aria-selected={n === sel}
                class={`picker-row conv-row${n === sel ? " selected" : ""}`}
                data-conversation={c.id}
                onClick={() => {
                  setSel(n);
                  void pick(c);
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
          <button class="primary" disabled={!chosen || busy} onClick={() => chosen && void pick(chosen)}>
            {chosen?.block != null ? "Go to it" : chosen?.live?.pane !== undefined && client.info(chosen.live.pane) ? "Go to pane" : "Open"}
          </button>
          <button
            disabled={!chosen || busy || !!chosen.live || chosen.block !== null}
            title={chosen?.live ? `It's ${chosen.live.place}: fork it instead` : undefined}
            onClick={() => chosen && void pick(chosen, "continue")}
          >
            Continue
          </button>
          <button disabled={!chosen || busy} title="A new session with its history; the original is left alone" onClick={() => chosen && void pick(chosen, "fork")}>
            Fork
          </button>
          <button onClick={close}>Cancel</button>
        </div>
      </div>
    </div>
  );
}
