// Studio apps (M35): the person's apps in their studio (from the daemon's
// studio token), and picking one opens its box as an app block, or goes to
// the block that has it already. Opened from a pane's menu, the `+`
// button's menu and the phone's sheet. On a phone it is a full-height
// sheet.

import { useEffect, useLayoutEffect, useRef, useState } from "preact/hooks";
import type { Client } from "../client";
import type { OpenRequest, OpenResponse, PaneId } from "../proto";

export interface StudioApp {
  name: string;
  title: string | null;
  /** The box's origin. */
  url: string;
  status: string | null;
  /** App blocks that show it now. */
  blocks: PaneId[];
}

export interface AppsWhere {
  /** Split this block, else a new tab in `session`. */
  split?: PaneId;
  session?: number;
}

let open: { client: Client; where: AppsWhere; phone: boolean } | null = null;
const listeners = new Set<() => void>();
const changed = () => listeners.forEach((fn) => fn());

/** Show the studio apps picker. */
export function pickApp(client: Client, where: AppsWhere, phone = false) {
  open = { client, where, phone };
  changed();
}

export function AppsLayer() {
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

/** Open `app`'s box as a block; the new block, or null. */
export async function openApp(client: Client, app: string, where: AppsWhere): Promise<PaneId | null> {
  const res = await client.request("POST", "/api/blocks", {
    type: "app",
    config: { app },
    split: where.split ?? null,
    session: where.session === undefined ? null : String(where.session),
    from_pane: where.split ?? null,
  } satisfies OpenRequest);
  if (!res.ok) {
    const e = await res.json<{ error?: string }>().catch(() => ({ error: undefined }));
    client.toast(`couldn't open ${app}: ${e.error ?? res.status}`);
    return null;
  }
  return (await res.json<OpenResponse>()).block;
}

function Picker({ client, where, phone, close }: { client: Client; where: AppsWhere; phone: boolean; close: () => void }) {
  const [list, setList] = useState<StudioApp[] | null>(null);
  const [studio, setStudio] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [sel, setSel] = useState(0);
  const [busy, setBusy] = useState(false);
  const input = useRef<HTMLInputElement>(null);

  useEffect(() => {
    void (async () => {
      try {
        const res = await client.request("GET", "/api/studio/apps");
        const v = await res.json<{ apps?: StudioApp[]; studio?: string; error?: string }>();
        if (!res.ok) throw new Error(v.error ?? `couldn't list them (${res.status})`);
        setList(v.apps ?? []);
        setStudio(v.studio ?? null);
      } catch (e) {
        setError((e as Error).message);
      }
    })();
  }, []);
  useLayoutEffect(() => {
    if (!phone) input.current?.focus();
  }, []);

  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  const rows = (list ?? []).filter((a) => words.every((w) => `${a.name} ${a.title ?? ""}`.toLowerCase().includes(w)));

  const pick = async (a: StudioApp) => {
    if (busy) return;
    if (a.blocks.length > 0) {
      client.focusPane(a.blocks[0]);
      close();
      return;
    }
    setBusy(true);
    const block = await openApp(client, a.name, where);
    setBusy(false);
    if (block !== null) close();
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
    } else if (e.key === "Enter") {
      e.preventDefault();
      const a = rows[sel];
      if (a) void pick(a);
    }
  };

  const chosen = rows[sel];
  return (
    <div class="prompt-backdrop picker-backdrop" role="dialog" aria-label="Studio apps" onPointerDown={(e) => e.target === e.currentTarget && close()}>
      <div class={phone ? "picker apps phone" : "picker apps"} onKeyDown={onKey}>
        <div class="picker-where">
          <span class="picker-host">Studio apps</span>
          {studio && <span class="dim">{hostOf(studio)}</span>}
        </div>
        <input
          ref={input}
          class="picker-filter"
          placeholder="Search apps"
          value={query}
          autocomplete="off"
          autocapitalize="off"
          spellcheck={false}
          onInput={(e) => {
            setQuery(e.currentTarget.value);
            setSel(0);
          }}
        />
        <ul class="picker-list" role="listbox">
          {!list && !error && <li class="picker-empty">Asking the studio…</li>}
          {list && rows.length === 0 && <li class="picker-empty">{query ? "Nothing matches" : "No apps in the studio"}</li>}
          {rows.map((a, n) => (
            <li
              key={a.name}
              role="option"
              aria-selected={n === sel}
              class={`picker-row app-row${n === sel ? " selected" : ""}`}
              data-app={a.name}
              onClick={() => {
                setSel(n);
                void pick(a);
              }}
              title={a.url}
            >
              <span class="picker-label">{a.title ?? a.name}</span>
              {a.title && <span class="dim">{a.name}</span>}
              {a.blocks.length > 0 && <span class="host-tag">%{a.blocks[0]}</span>}
              {a.status && a.status !== "running" && <span class="dim">{a.status}</span>}
            </li>
          ))}
        </ul>
        {error && (
          <p class="error picker-error">
            {error}
            {/not logged in/.test(error) && (
              <>
                {" "}
                In a terminal: <code>illogical studio login &lt;studio url&gt;</code>
              </>
            )}
          </p>
        )}
        <div class="picker-actions">
          <span class="picker-target">{chosen ? hostOf(chosen.url) : ""}</span>
          <button class="primary" disabled={!chosen || busy} onClick={() => chosen && void pick(chosen)}>
            {chosen && chosen.blocks.length > 0 ? "Go to it" : "Open"}
          </button>
          <button onClick={close}>Cancel</button>
        </div>
      </div>
    </div>
  );
}

function hostOf(url: string): string {
  try {
    return new URL(url).host;
  } catch {
    return url;
  }
}
