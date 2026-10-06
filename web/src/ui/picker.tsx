// The directory picker (M7): browse directories on the host a block runs
// on (this daemon's, another host's, or its VM's), starting where the
// block is (OSC 7) with directories used lately there first, and start a
// pane or a tab there, or `cd` its shell there. Opened from a pane's menu,
// the `+` button's menu, the phone's sheet, or Ctrl+Shift+G. On a phone it
// is a full-height sheet.

import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "preact/hooks";
import type { Client } from "../client";
import type { PaneId, RunRequest } from "../proto";
import { directory } from "../hosts";
import { fuzzy, isDir, listDirs, recentDirs, type FsList } from "../fs";
import { isWorkspace, openWorkspace } from "../blocks/workspace";

let open: { client: Client; pane: PaneId; phone: boolean } | null = null;
const listeners = new Set<() => void>();
const changed = () => listeners.forEach((fn) => fn());

/** Show the picker for the block `pane` (its host and directory). */
export function openPicker(client: Client, pane: PaneId | undefined, phone = false) {
  if (pane === undefined) return client.toast("no pane to start from");
  open = { client, pane, phone };
  changed();
}

export function PickerLayer() {
  const [, setTick] = useState(0);
  useEffect(() => {
    const fn = () => setTick((t) => t + 1);
    listeners.add(fn);
    return () => {
      listeners.delete(fn);
    };
  }, []);
  if (!open) return null;
  const { client, pane, phone } = open;
  return (
    <Picker
      key={pane}
      client={client}
      pane={pane}
      phone={phone}
      close={() => {
        open = null;
        changed();
      }}
    />
  );
}

/** Ctrl+Shift+G anywhere (before the terminal sees it) opens the picker on
 * the active pane. */
export function usePickerShortcut(client: Client, phone: boolean) {
  useEffect(() => {
    const key = (e: KeyboardEvent) => {
      if (!(e.ctrlKey && e.shiftKey && !e.altKey && e.key.toLowerCase() === "g")) return;
      e.preventDefault();
      e.stopPropagation();
      openPicker(client, client.active(), phone);
    };
    window.addEventListener("keydown", key, true);
    return () => window.removeEventListener("keydown", key, true);
  }, [client, phone]);
}

type Row = { path: string; label: string; recent?: boolean; up?: boolean };

const base = (p: string) => p.split("/").filter(Boolean).pop() ?? "/";

function Picker({ client, pane, phone, close }: { client: Client; pane: PaneId; phone: boolean; close: () => void }) {
  const info = client.info(pane);
  const machine = client.machine(pane);
  const tab = client.tabOfPane(pane);
  const tabMachine = tab ? client.tabMachine(tab.id) : undefined;
  const [list, setList] = useState<FsList | null>(null);
  const [recent, setRecent] = useState<string[]>([]);
  const [query, setQuery] = useState("");
  const [sel, setSel] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // M34: the directory shown is a chant workspace.
  const [workspace, setWorkspace] = useState(false);
  const input = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLUListElement>(null);
  const started = useRef(false);

  const go = async (path: string) => {
    setError(null);
    try {
      const l = await listDirs(client, pane, path);
      setList(l);
      setQuery("");
      setSel(0);
    } catch (e) {
      setError((e as Error).message);
    }
  };

  useEffect(() => {
    if (started.current) return;
    started.current = true;
    // Where the block is; its home if that's gone (or unknown).
    void (async () => {
      const cwd = info?.cwd ?? "~";
      try {
        setList(await listDirs(client, pane, cwd));
      } catch {
        await go("~");
      }
    })();
    recentDirs(client, pane)
      .then(setRecent)
      .catch(() => setRecent([]));
  }, []);

  useEffect(() => {
    setWorkspace(false);
    if (!list || client.state?.roles) return;
    let live = true;
    void isWorkspace(client, pane, list.path).then((v) => live && setWorkspace(v));
    return () => {
      live = false;
    };
  }, [list?.path]);

  // A phone's keyboard would cover half the list: only on request there.
  useLayoutEffect(() => {
    if (!phone) input.current?.focus();
  }, []);

  const rows: Row[] = useMemo(() => {
    if (!list) return [];
    const q = query.trim();
    const here = list.path;
    const dirs: Row[] = list.entries.filter(isDir).map((e) => ({ path: e.path, label: e.name + "/" }));
    const recents: Row[] = recent.filter((r) => r !== here).map((r) => ({ path: r, label: r, recent: true }));
    if (!q) {
      // Hidden directories last.
      const sorted = [...dirs.filter((d) => !d.label.startsWith(".")), ...dirs.filter((d) => d.label.startsWith("."))];
      const up: Row[] = list.parent ? [{ path: list.parent, label: "..", up: true }] : [];
      return [...recents, ...up, ...sorted];
    }
    if (q.startsWith("/") || q.startsWith("~")) return [{ path: q, label: `Go to ${q}` }];
    const scored = [
      ...recents.map((r) => ({ r, s: fuzzy(q, r.path) })),
      ...dirs.map((r) => ({ r, s: fuzzy(q, r.label) })),
    ].filter((x): x is { r: Row; s: number } => x.s !== null);
    scored.sort((a, b) => b.s - a.s);
    return scored.map((x) => x.r);
  }, [list, recent, query]);

  useEffect(() => {
    listRef.current?.querySelector(".picker-row.selected")?.scrollIntoView({ block: "nearest" });
  }, [sel, rows]);

  const where = list?.path ?? info?.cwd ?? "~";
  const host = directory.current || "this host";
  const machineLabel = machine ? (machine.borrowed ? machine.sprite : (machine.name ?? machine.sprite)) : null;
  // A machine's directories exist only where it runs: a tab of its own
  // is for a sandbox we borrow (borrowed again), not a VM tab's machine.
  const tabRefusal = machine && !machine.borrowed ? "a VM's directories are only in its own tab: use New pane here" : null;
  const isTerminal = info?.type === "terminal";
  const idle = isTerminal && info?.running && !info.current && info.attention !== "working" && info.attention !== "needs_input";
  const cdRefusal = !isTerminal
    ? "only a terminal's shell can cd"
    : !info?.integration
      ? "this pane has shell integration off"
      : !idle
        ? "it's running something: cd only goes to an idle shell"
        : null;
  const paneRefusal =
    machine && !machine.borrowed && machine.id !== tabMachine?.id ? "this VM is the pane's own: share it with the tab first" : null;

  const act = async (what: "pane" | "tab" | "cd") => {
    if (!list || busy) return;
    setBusy(true);
    setError(null);
    const path = list.path;
    let err: string | null;
    if (what === "cd") err = await client.make(`/api/panes/${pane}/cd`, { path });
    else if (what === "pane") err = await client.make("/api/run", { split: pane, join: true, cwd: path, from_pane: pane } satisfies RunRequest);
    else
      err = await client.make("/api/run", {
        cwd: path,
        from_pane: pane,
        sandbox: machine?.borrowed ? machine.sprite : null,
      } satisfies RunRequest);
    setBusy(false);
    if (err) setError(err);
    else close();
  };

  const onKey = (e: KeyboardEvent) => {
    if (e.key === "Escape") {
      e.preventDefault();
      close();
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      setSel((s) => Math.min(rows.length - 1, s + 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setSel((s) => Math.max(0, s - 1));
    } else if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
      e.preventDefault();
      void act("pane");
    } else if (e.key === "Enter") {
      e.preventDefault();
      const r = rows[sel];
      if (r) void go(r.path);
    } else if (e.key === "Backspace" && query === "" && list?.parent) {
      e.preventDefault();
      void go(list.parent);
    }
  };

  // The path as buttons, each going to that directory.
  const parts = where.split("/").filter(Boolean);
  const crumbs = [
    { label: "/", path: "/" },
    ...parts.map((p, i) => ({ label: p, path: "/" + parts.slice(0, i + 1).join("/") })),
  ];

  return (
    <div
      class="prompt-backdrop picker-backdrop"
      role="dialog"
      aria-label="Go to directory"
      onPointerDown={(e) => e.target === e.currentTarget && close()}
    >
      <div class={phone ? "picker phone" : "picker"} onKeyDown={onKey}>
        <div class="picker-where">
          <span class="picker-host">{host}</span>
          {machineLabel && <span class="host-tag">{machine?.borrowed ? `${machineLabel} · shell` : `VM ${machineLabel}`}</span>}
        </div>
        <div class="picker-path" data-path={list?.path ?? ""}>
          {crumbs.map((c, i) => (
            <button key={c.path} class="crumb-button" onClick={() => void go(c.path)}>
              {c.label}
              {i > 0 && i < crumbs.length - 1 ? "/" : ""}
            </button>
          ))}
        </div>
        <input
          ref={input}
          class="picker-filter"
          placeholder="Filter, or type a path"
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
          {list && rows.length === 0 && <li class="picker-empty">{query ? "Nothing matches" : "No directories here"}</li>}
          {rows.map((r, i) => (
            <li
              key={`${r.recent ? "r" : "d"}:${r.path}`}
              role="option"
              aria-selected={i === sel}
              class={`picker-row${i === sel ? " selected" : ""}${r.recent ? " recent" : ""}`}
              data-path={r.path}
              onClick={() => void go(r.path)}
            >
              {r.recent ? <span class="picker-icon">↺</span> : <span class="picker-icon">{r.up ? "↑" : "▸"}</span>}
              <span class="picker-label">{r.recent ? r.path : r.label}</span>
            </li>
          ))}
          {list?.truncated && <li class="picker-empty">More entries than one listing holds: type to filter</li>}
        </ul>
        {error && <p class="error picker-error">{error}</p>}
        <div class="picker-actions">
          <span class="picker-target" title={where}>
            {base(where)}
          </span>
          <button class="primary" disabled={!list || busy || !!paneRefusal} title={paneRefusal ?? "Ctrl+Enter"} onClick={() => void act("pane")}>
            New pane here
          </button>
          <button disabled={!list || busy || !!tabRefusal} title={tabRefusal ?? undefined} onClick={() => void act("tab")}>
            New tab here
          </button>
          <button disabled={!list || busy || !isTerminal} title={cdRefusal ?? undefined} onClick={() => void act("cd")}>
            cd there
          </button>
          {workspace && list && (
            <button
              data-open-workspace
              onClick={() => {
                openWorkspace(client, list.path, pane);
                close();
              }}
            >
              Open as workspace
            </button>
          )}
          <button onClick={close}>Cancel</button>
        </div>
      </div>
    </div>
  );
}
