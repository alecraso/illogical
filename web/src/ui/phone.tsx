// Phone layout: one pane at a time, full screen. A sheet lists sessions,
// tabs and panes to switch between, and a key bar supplies the keys a phone
// keyboard lacks.

import { useState } from "preact/hooks";
import { paneIds, tabLabel, type Client } from "../client";
import { useSubscribe } from "./hooks";
import { AttentionBadge } from "./attention";
import { HostCrumb, HostSection } from "./hosts";
import { openPort } from "../blocks";
import { startAgent } from "./agent-dialog";
import { openSandboxes } from "./sandboxes";
import { openPicker } from "./picker";

export function PhoneHeader({ client }: { client: Client }) {
  const [open, setOpen] = useState(false);
  const tab = client.tabView();
  const session = client.state?.sessions.find((s) => s.id === client.session);
  const panes = tab ? paneIds(tab) : [];
  const active = client.active();
  return (
    <>
      <header class="bar phone-bar">
        <button class="sheet-button" aria-expanded={open} onClick={() => setOpen(!open)}>
          {client.state?.panes.some((p) => p.attention === "needs_input") ? <span class="att needs_input">●</span> : "☰"} <HostCrumb />
          <span class="crumb">{session?.name}</span> ›{" "}
          {tab && client.tabMachine(tab.id) && <span class="host-tag">VM</span>}
          <span class="crumb">{tab ? tabLabel(client, tab) : ""}</span>
        </button>
        {panes.length > 1 && (
          <span class="pane-count">
            {panes.indexOf(active ?? -1) + 1}/{panes.length}
          </span>
        )}
      </header>
      {open && <Sheet client={client} close={() => setOpen(false)} />}
    </>
  );
}

function Sheet({ client, close }: { client: Client; close: () => void }) {
  const state = client.state!;
  const active = client.active();
  const wanting = state.panes.filter((p) => p.attention === "needs_input" || p.attention === "done");
  const act = (fn: () => void) => () => {
    fn();
    close();
  };
  return (
    <div class="sheet-backdrop" onClick={close}>
      <nav class="sheet" onClick={(e) => e.stopPropagation()}>
        <HostSection close={close} />
        {wanting.length > 0 && (
          <section class="needs-you">
            <h2>Needs you</h2>
            {wanting.map((p) => (
              <button key={p.id} class="sheet-item" onClick={act(() => client.setActive(p.id))}>
                <AttentionBadge state={p.attention} />{" "}
                {client.title(p.id) || p.current?.text || p.last?.text || p.cwd || `pane %${p.id}`}
              </button>
            ))}
          </section>
        )}
        {state.sessions.map((s) => (
          <section key={s.id}>
            <h2>{s.name}</h2>
            {s.tabs.map((tid) => {
              const t = client.tabView(tid);
              if (!t) return null;
              const panes = paneIds(t);
              return (
                <div key={tid} class="sheet-tab">
                  <button class={tid === client.tab ? "sheet-item current" : "sheet-item"} onClick={act(() => client.selectTab(tid))}>
                    {(client.tabMachine(tid) || panes.some((p) => client.machine(p))) && <span class="host-tag">VM</span>}
                    {tabLabel(client, t)}
                  </button>
                  {panes.length > 1 &&
                    panes.map((p, i) => (
                      <button
                        key={p}
                        class={p === active ? "sheet-item sheet-pane current" : "sheet-item sheet-pane"}
                        onClick={act(() => client.setActive(p))}
                      >
                        {/* Shells often title every pane alike; the number tells them apart. */}
                        <span class="pane-number">{i + 1}</span>
                        {client.title(p) || client.cwd(p) || `pane %${p}`}
                      </button>
                    ))}
                </div>
              );
            })}
          </section>
        ))}
        <div class="sheet-actions">
          <button onClick={act(() => client.session !== null && client.intent({ op: "new_tab", session: client.session, from_pane: active ?? null }))}>
            New tab
          </button>
          <button onClick={act(() => client.session !== null && void client.newVm({ session: client.session, tab: true }))}>New VM tab</button>
          <button onClick={act(() => client.session !== null && startAgent(client, { session: client.session, from: active }))}>New agent</button>
          {active !== undefined && <button onClick={act(() => openPicker(client, active, true))}>Go to directory</button>}
          {active !== undefined && (
            <button onClick={act(() => client.intent({ op: "split", pane: active, edge: "right" }))}>Split pane</button>
          )}
          {active !== undefined && client.tab !== null && client.tabMachine(client.tab) && (
            <button onClick={act(() => client.intent({ op: "split", pane: active, edge: "right", local: true }))}>Split (local)</button>
          )}
          {active !== undefined && (
            // Where the active pane runs: its machine, or this host.
            <button onClick={act(() => void openPort(client, { split: active, host: client.machine(active)?.id, local: !client.machine(active) }))}>
              Open port
            </button>
          )}
          <button onClick={act(() => client.intent({ op: "new_session", name: null, from_pane: active ?? null }))}>New session</button>
          <button onClick={act(() => openSandboxes())}>Sandboxes</button>
          {active !== undefined && (
            <button class="danger" onClick={act(() => client.intent({ op: "close_pane", pane: active }))}>
              Close pane
            </button>
          )}
          {client.tab !== null && (client.tabMachine(client.tab) || paneIds(client.tabView(client.tab)!).length > 1) && (
            <button class="danger" onClick={act(() => client.tab !== null && client.intent({ op: "close_tab", tab: client.tab }))}>
              {client.tabMachine(client.tab) ? "Close tab and machine" : "Close tab"}
            </button>
          )}
        </div>
      </nav>
    </div>
  );
}

const KEYS: { label: string; bytes?: string; app?: string; mod?: "ctrl" | "alt" }[] = [
  { label: "Esc", bytes: "\x1b" },
  { label: "Tab", bytes: "\t" },
  { label: "Ctrl", mod: "ctrl" },
  { label: "Alt", mod: "alt" },
  { label: "←", bytes: "\x1b[D", app: "\x1bOD" },
  { label: "↑", bytes: "\x1b[A", app: "\x1bOA" },
  { label: "↓", bytes: "\x1b[B", app: "\x1bOB" },
  { label: "→", bytes: "\x1b[C", app: "\x1bOC" },
  { label: "|", bytes: "|" },
  { label: "~", bytes: "~" },
  { label: "/", bytes: "/" },
  { label: "-", bytes: "-" },
];

export function KeyBar({ client }: { client: Client }) {
  useSubscribe((fn) => client.subscribe(fn));
  const enc = new TextEncoder();
  return (
    <div class="keybar" role="toolbar" aria-label="Extra keys">
      {KEYS.map((k) => {
        const on = k.mod ? client.modifiers[k.mod] : false;
        return (
          <button
            key={k.label}
            class={on ? "key on" : "key"}
            aria-pressed={k.mod ? on : undefined}
            // Keep focus (and the on-screen keyboard) on the terminal.
            onPointerDown={(e) => e.preventDefault()}
            onClick={() => {
              const pane = client.active();
              if (pane === undefined) return;
              if (k.mod) {
                client.modifiers = { ...client.modifiers, [k.mod]: !client.modifiers[k.mod] };
                client.emit();
                return;
              }
              const view = client.panes.get(pane)?.view;
              const seq = view?.appCursor && k.app ? k.app : k.bytes!;
              client.input(pane, enc.encode(seq));
            }}
          >
            {k.label}
          </button>
        );
      })}
    </div>
  );
}
