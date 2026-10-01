import { Fragment } from "preact";
import { useEffect, useLayoutEffect, useRef, useState } from "preact/hooks";
import { paneIds, tabLabel, type Client } from "../client";
import type { Edge, Intent, PaneId, Policy, Rect, SplitRect, TabId, TabView } from "../proto";
import type { Cell } from "./cells";
import { drag, startDrag, type Dragged, type Target } from "./drag";
import { useSubscribe, usePhone } from "./hooks";
import { askText, closeMenu, MenuLayer, openMenu, PromptLayer, type MenuItem } from "./menu";
import { disablePush, enablePush, pushState, type PushState } from "../push";
import { KeyBar, PhoneHeader } from "./phone";
import { AttentionBadge, tabAttention } from "./attention";

/** Where hidden panes' terminals live: off the page but still alive. */
const parking = document.createElement("div");
parking.id = "parking";
document.body.appendChild(parking);

type Renaming = { kind: "tab" | "session"; id: number } | null;

export function App({ client, cell }: { client: Client; cell: Cell }) {
  useSubscribe((fn) => client.subscribe(fn));
  const phone = usePhone();
  const [renaming, setRenaming] = useState<Renaming>(null);

  useEffect(() => {
    const focus = () => client.tab !== null && client.claim(client.tab);
    const visible = () => document.visibilityState === "visible" && client.wake();
    window.addEventListener("focus", focus);
    document.addEventListener("visibilitychange", visible);
    return () => {
      window.removeEventListener("focus", focus);
      document.removeEventListener("visibilitychange", visible);
    };
  }, [client]);

  useHoverToSwitchTabs(client);
  useReportFocus(client, phone);

  const state = client.state;
  const tab = client.tabView();
  return (
    <div class={phone ? "app phone" : "app"}>
      {state && state.sessions.length > 0 && (phone ? (
        <PhoneHeader client={client} />
      ) : (
        <TopBar client={client} renaming={renaming} setRenaming={setRenaming} />
      ))}
      <main class="main">
        {!state ? null : state.sessions.length === 0 ? (
          <div class="empty">
            <p>No sessions.</p>
            <button class="primary" onClick={() => client.intent({ op: "new_session", name: null, from_pane: null })}>
              New session
            </button>
          </div>
        ) : tab ? (
          <TabArea client={client} tab={tab} cell={cell} phone={phone} />
        ) : null}
      </main>
      {phone && state && state.sessions.length > 0 && <KeyBar client={client} />}
      <MenuLayer />
      <PromptLayer />
      <DragGhost />
      <StatusPill client={client} />
    </div>
  );
}

// ---------------------------------------------------------------- top bar

function TopBar({
  client,
  renaming,
  setRenaming,
}: {
  client: Client;
  renaming: Renaming;
  setRenaming: (r: Renaming) => void;
}) {
  useSubscribe(drag.subscribe);
  const state = client.state!;
  const session = state.sessions.find((s) => s.id === client.session) ?? state.sessions[0];
  const target = drag.current?.target;
  const marker = target?.kind === "tabbar" ? Math.min(target.index, session.tabs.length) : null;

  const sessionMenu = (e: MouseEvent) => {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    const items: MenuItem[] = [
      ...state.sessions.map((s) => ({
        label: `${s.id === session.id ? "✓ " : "    "}${s.name}`,
        run: () => client.selectSession(s.id),
      })),
      "separator",
      { label: "New session", run: () => client.intent({ op: "new_session", name: null, from_pane: client.active() ?? null }) },
      { label: "Rename session", run: () => setRenaming({ kind: "session", id: session.id }) },
      "separator",
      ...notificationItems(client),
      "separator",
      { label: "Close session", danger: true, run: () => client.intent({ op: "close_session", session: session.id }) },
    ];
    openMenu({ clientX: r.left, clientY: r.bottom + 4, preventDefault: () => e.preventDefault() }, items);
  };

  return (
    <header class="bar">
      {renaming?.kind === "session" && renaming.id === session.id ? (
        <RenameInput
          value={session.name}
          onDone={(name) => {
            setRenaming(null);
            if (name && name !== session.name) client.intent({ op: "rename_session", session: session.id, name });
          }}
        />
      ) : (
        <button class="session-button" title="Sessions" onClick={sessionMenu} onContextMenu={sessionMenu}>
          {session.name} <span class="caret">▾</span>
        </button>
      )}
      <div class="tabbar" role="tablist">
        {session.tabs.map((id, i) => {
          const t = client.tabView(id);
          if (!t) return null;
          return (
            <Fragment key={id}>
              {marker === i && <div class="drop-marker" />}
              <TabItem
                client={client}
                tab={t}
                index={i}
                selected={id === client.tab}
                renaming={renaming?.kind === "tab" && renaming.id === id}
                setRenaming={setRenaming}
              />
            </Fragment>
          );
        })}
        {marker === session.tabs.length && <div class="drop-marker" />}
        <button
          class="new-tab"
          title="New tab"
          onClick={() => client.intent({ op: "new_tab", session: session.id, from_pane: client.active() ?? null })}
        >
          +
        </button>
      </div>
      <div class="bar-fill" />
    </header>
  );
}

function TabItem({
  client,
  tab,
  index,
  selected,
  renaming,
  setRenaming,
}: {
  client: Client;
  tab: TabView;
  index: number;
  selected: boolean;
  renaming: boolean;
  setRenaming: (r: Renaming) => void;
}) {
  const label = tabLabel(client, tab);
  const close = () => client.intent({ op: "close_tab", tab: tab.id });
  if (renaming) {
    return (
      <div class="tab selected">
        <RenameInput
          value={tab.name ?? label}
          onDone={(name) => {
            setRenaming(null);
            if (name !== null) client.intent({ op: "rename_tab", tab: tab.id, name: name || null });
          }}
        />
      </div>
    );
  }
  return (
    <div
      class={selected ? "tab selected" : "tab"}
      role="tab"
      aria-selected={selected}
      data-tab-index={index}
      data-tab-id={tab.id}
      title={label}
      onPointerDown={(e) =>
        startDrag(e, { kind: "tab", tab: tab.id }, label, {
          onClick: () => client.selectTab(tab.id),
          onDrop: (what, target) => drop(client, what, target),
        })
      }
      onDblClick={() => setRenaming({ kind: "tab", id: tab.id })}
      onAuxClick={(e) => e.button === 1 && close()}
      onContextMenu={(e) =>
        openMenu(e, [
          { label: "Rename tab", run: () => setRenaming({ kind: "tab", id: tab.id }) },
          { label: "New tab", run: () => client.intent({ op: "new_tab", session: client.session!, from_pane: client.active(tab.id) ?? null }) },
          "separator",
          { label: "Close tab", danger: true, run: close },
        ])
      }
    >
      <span class="tab-label">{label}</span>
      <AttentionBadge state={tabAttention(client, tab)} />
      <button
        class="tab-close"
        title="Close tab"
        onPointerDown={(e) => e.stopPropagation()}
        onClick={(e) => {
          e.stopPropagation();
          close();
        }}
      >
        ×
      </button>
    </div>
  );
}

function RenameInput({ value, onDone }: { value: string; onDone: (v: string | null) => void }) {
  const ref = useRef<HTMLInputElement>(null);
  const done = useRef(false);
  useLayoutEffect(() => {
    ref.current?.focus();
    ref.current?.select();
  }, []);
  const finish = (v: string | null) => {
    if (done.current) return;
    done.current = true;
    onDone(v === null ? null : v.trim());
  };
  return (
    <input
      ref={ref}
      class="rename"
      value={value}
      onKeyDown={(e) => {
        if (e.key === "Enter") finish((e.target as HTMLInputElement).value);
        if (e.key === "Escape") finish(null);
      }}
      onBlur={(e) => finish((e.target as HTMLInputElement).value)}
    />
  );
}

// ---------------------------------------------------------------- tab area

/** One tab's panes, drawn at the cell rectangles the daemon computed. The
 * whole grid is scaled down when another window's size owns the tab. */
export function TabArea({ client, tab, cell, phone }: { client: Client; tab: TabView; cell: Cell; phone: boolean }) {
  const ref = useRef<HTMLDivElement>(null);
  const [area, setArea] = useState<{ w: number; h: number } | null>(null);

  useLayoutEffect(() => {
    const el = ref.current!;
    const measure = () => setArea({ w: el.clientWidth, h: el.clientHeight });
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    measure();
    return () => ro.disconnect();
  }, []);

  const cols = area ? Math.max(2, Math.floor(area.w / cell.width)) : 0;
  const rows = area ? Math.max(1, Math.floor(area.h / cell.height)) : 0;
  const active = client.active(tab.id) ?? null;
  const zoom = phone ? active : null;

  // Tell the daemon what we show. Showing a tab (or another pane, on a
  // phone) claims its size; resizing the window only updates a size we own.
  const size = useRef({ cols, rows, zoom });
  size.current = { cols, rows, zoom };
  const shown = useRef<{ tab: TabId; zoom: PaneId | null } | null>(null);
  useEffect(() => {
    client.claim = (t) => {
      const s = size.current;
      if (s.cols) client.view(t, s.cols, s.rows, s.zoom, true);
    };
  }, [client]);
  useEffect(() => {
    if (!cols) return;
    const prev = shown.current;
    const claim = !prev || prev.tab !== tab.id || prev.zoom !== zoom;
    shown.current = { tab: tab.id, zoom };
    client.view(tab.id, cols, rows, zoom, claim);
  }, [client, tab.id, cols, rows, zoom]);

  // Keyboard focus follows the active pane (not on phones: that would pop
  // up the keyboard on every switch).
  useEffect(() => {
    if (!phone && active !== null) client.panes.get(active)?.view.focus();
  }, [client, tab.id, active, phone]);
  // ...and comes back to it after menus and buttons, unless something else
  // (a rename box) is using the keyboard.
  const rev = client.state?.rev;
  useEffect(() => {
    const el = document.activeElement;
    const idle = !el || el === document.body || el.closest(".menu, .bar button, .tab");
    if (!phone && active !== null && idle) client.panes.get(active)?.view.focus();
  }, [client, rev, active, phone]);

  const gridW = tab.cols * cell.width;
  const gridH = tab.rows * cell.height;
  const scale = area ? Math.min(1, area.w / gridW, area.h / gridH) : 1;
  const elsewhere = tab.owner !== null && tab.owner !== client.clientId;

  return (
    <div class="tab-area" ref={ref}>
      <div
        class="grid"
        style={{ width: len(gridW), height: len(gridH), transform: scale < 1 ? `scale(${scale})` : undefined }}
      >
        {tab.layout.panes.map(([id, r]) => (
          <PaneSlot key={id} client={client} id={id} rect={r} cell={cell} active={id === active} phone={phone} />
        ))}
        {!phone &&
          tab.layout.splits.map((s) =>
            s.extents.slice(0, -1).map((_, i) => (
              <Divider key={`${s.id}-${i}`} client={client} split={s} index={i} cell={cell} scale={scale} />
            )),
          )}
        <DropOverlay client={client} tab={tab} cell={cell} />
      </div>
      {elsewhere && (
        <button class="sized-elsewhere" onClick={() => client.claim(tab.id)}>
          Sized for another window · use this size
        </button>
      )}
    </div>
  );
}

/** CSS length (Preact 11 no longer appends "px" to numbers). */
const len = (n: number) => `${n}px`;

function px(r: Rect, cell: Cell) {
  return {
    left: len(r.x * cell.width),
    top: len(r.y * cell.height),
    width: len(r.cols * cell.width),
    height: len(r.rows * cell.height),
  };
}

function PaneSlot({
  client,
  id,
  rect,
  cell,
  active,
  phone,
}: {
  client: Client;
  id: PaneId;
  rect: Rect;
  cell: Cell;
  active: boolean;
  phone: boolean;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const entry = client.panes.get(id);

  // Move the pane's terminal in, rather than creating one: moving a pane
  // around the layout never restarts or redraws its terminal from scratch.
  useLayoutEffect(() => {
    const slot = ref.current;
    if (!entry || !slot) return;
    slot.appendChild(entry.view.host);
    entry.view.setVisible(true);
    return () => {
      if (entry.view.host.parentElement === slot) {
        parking.appendChild(entry.view.host);
        entry.view.setVisible(false);
      }
    };
  }, [entry]);

  const info = client.info(id);

  // Right-clicking a command's mark.
  useEffect(() => {
    entry?.view.onMarkMenu((mark, e) => {
      const view = entry.view;
      openMenu(e, [
        { header: mark.text || "command" },
        { label: "Select output", run: () => view.selectOutput(mark) },
        { label: "Copy output", run: () => void navigator.clipboard?.writeText(view.outputText(mark)) },
        { label: "Copy command", disabled: !mark.text, run: () => void navigator.clipboard?.writeText(mark.text) },
        "separator",
        {
          label: "Run again",
          disabled: !mark.text || client.info(id)?.current !== null,
          run: () => client.input(id, new TextEncoder().encode(`${mark.text}\r`)),
        },
      ]);
    });
  }, [entry, client, id]);

  const menu = (e: MouseEvent) => {
    // A program that tracks the mouse gets right-clicks; Shift reaches us.
    if (entry?.view.mouseTracking && !e.shiftKey) return;
    const cwd = client.cwd(id);
    openMenu(e, [
      { label: "Split right", run: () => client.intent({ op: "split", pane: id, edge: "right" }) },
      { label: "Split down", run: () => client.intent({ op: "split", pane: id, edge: "bottom" }) },
      "separator",
      {
        label: "Move to new tab",
        disabled: (client.tabOfPane(id) && paneIds(client.tabOfPane(id)!).length < 2) ?? true,
        run: () => client.intent({ op: "break_pane", pane: id, session: client.session!, index: null }),
      },
      { label: "Copy working directory", disabled: !cwd, run: () => cwd && void navigator.clipboard?.writeText(cwd) },
      "separator",
      ...restartItems(client, id),
      "separator",
      ...(info && (info.attention === "needs_input" || info.attention === "done")
        ? [{ label: "Dismiss", run: () => client.paneOp(id, { op: "attention", state: "idle" }) } as MenuItem]
        : []),
      {
        label: "Shell integration (new shells)",
        checked: info?.integration ?? true,
        run: () => client.paneOp(id, { op: "set_integration", on: !(info?.integration ?? true) }),
      },
      {
        label: "Forget history",
        run: () => client.paneOp(id, { op: "purge" }),
      },
      { label: "Close pane", danger: true, run: () => client.intent({ op: "close_pane", pane: id }) },
    ]);
  };

  const waiting = client.info(id)?.running === false;
  return (
    <div
      ref={ref}
      class={active ? "pane active" : "pane"}
      data-pane={id}
      style={px(rect, cell)}
      onPointerDownCapture={() => client.setActive(id)}
      onContextMenu={menu}
    >
      {!active && (info?.attention === "needs_input" || info?.attention === "done") && (
        <div class={`pane-badge ${info.attention}`}>{info.attention === "done" ? "done" : "needs you"}</div>
      )}
      {waiting && (
        <button
          class="start-pane"
          onPointerDown={(e) => e.stopPropagation()}
          onClick={() => client.input(id, new TextEncoder().encode("\r"))}
        >
          {client.info(id)?.policy.kind === "rerun" ? "Re-run" : "Start shell"}
        </button>
      )}
      {!phone && (
        <div
          class="grip"
          title="Drag to move this pane"
          onPointerDown={(e) => {
            e.stopPropagation();
            startDrag(e, { kind: "pane", pane: id }, client.panes.get(id)?.title || `pane %${id}`, {
              onDrop: (what, target) => drop(client, what, target),
            });
          }}
        >
          ⠿
        </div>
      )}
    </div>
  );
}

/** What the pane does when the daemon starts again, e.g. after a reboot. */
function restartItems(client: Client, id: PaneId): MenuItem[] {
  const info = client.info(id);
  const p = info?.policy ?? { kind: "shell" };
  const cmd = info?.command;
  const set = (policy: Policy) => client.paneOp(id, { op: "set_policy", policy });
  const short = (s: string) => (s.length > 32 ? `${s.slice(0, 31)}…` : s);
  return [
    { header: "After a restart" },
    { label: "Start a shell here", checked: p.kind === "shell", run: () => set({ kind: "shell" }) },
    {
      label: cmd ? `Re-run ${short(cmd)}, asking first` : "Re-run the command, asking first",
      checked: p.kind === "rerun" && p.confirm,
      run: () => set({ kind: "rerun", confirm: true }),
    },
    {
      label: cmd ? `Re-run ${short(cmd)}` : "Re-run the command",
      checked: p.kind === "rerun" && !p.confirm,
      run: () => set({ kind: "rerun", confirm: false }),
    },
    {
      label: p.kind === "hook" ? `Run ${short(p.command)}` : "Run a command…",
      checked: p.kind === "hook",
      run: async () => {
        const command = await askText("Run when restored", p.kind === "hook" ? p.command : "", "claude --continue");
        if (command?.trim()) set({ kind: "hook", command: command.trim() });
      },
    },
    { label: "Nothing (wait for Enter)", checked: p.kind === "none", run: () => set({ kind: "none" }) },
  ];
}

function Divider({
  client,
  split,
  index,
  cell,
  scale,
}: {
  client: Client;
  split: SplitRect;
  index: number;
  cell: Cell;
  scale: number;
}) {
  const row = split.dir === "row";
  const at = split.extents.slice(0, index + 1).reduce((a, b) => a + b, 0) + index;
  const r = split.rect;
  const style = row
    ? px({ x: r.x + at, y: r.y, cols: 1, rows: r.rows }, cell)
    : px({ x: r.x, y: r.y + at, cols: r.cols, rows: 1 }, cell);

  const onPointerDown = (e: PointerEvent) => {
    if (e.button !== 0) return;
    e.preventDefault();
    const el = e.currentTarget as HTMLElement;
    el.setPointerCapture(e.pointerId);
    const start = row ? e.clientX : e.clientY;
    const unit = (row ? cell.width : cell.height) * scale;
    const base = split.extents.slice();
    let last = 0;
    let timer: number | undefined;
    let pending: number[] | null = null;
    const flush = () => {
      timer = undefined;
      if (pending) client.intent({ op: "resize_split", split: split.id, weights: pending });
      pending = null;
    };
    const move = (ev: PointerEvent) => {
      const d = Math.round(((row ? ev.clientX : ev.clientY) - start) / unit);
      if (d === last) return;
      last = d;
      const ext = base.slice();
      const a = Math.max(1, Math.min(base[index] + d, base[index] + base[index + 1] - 1));
      ext[index] = a;
      ext[index + 1] = base[index] + base[index + 1] - a;
      pending = ext;
      // Live, but not faster than the panes can sensibly redraw.
      timer ??= window.setTimeout(flush, 50);
    };
    const up = () => {
      el.removeEventListener("pointermove", move);
      el.removeEventListener("pointerup", up);
      clearTimeout(timer);
      flush();
    };
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", up);
  };

  return <div class={row ? "divider col-resize" : "divider row-resize"} style={style} onPointerDown={onPointerDown} />;
}

function DropOverlay({ client, tab, cell }: { client: Client; tab: TabView; cell: Cell }) {
  useSubscribe(drag.subscribe);
  const d = drag.current;
  const t = d?.target;
  if (!d || t?.kind !== "pane" || !dropAllowed(client, d.what, t)) return null;
  const rect = tab.layout.panes.find(([id]) => id === t.pane)?.[1];
  if (!rect) return null;
  const half = (r: Rect, edge: Edge): Rect => {
    const w = Math.max(1, Math.floor(r.cols / 2));
    const h = Math.max(1, Math.floor(r.rows / 2));
    switch (edge) {
      case "left":
        return { ...r, cols: w };
      case "right":
        return { ...r, x: r.x + r.cols - w, cols: w };
      case "top":
        return { ...r, rows: h };
      case "bottom":
        return { ...r, y: r.y + r.rows - h, rows: h };
      default:
        return r;
    }
  };
  return <div class="drop-zone" style={px(half(rect, t.edge), cell)} />;
}

function DragGhost() {
  useSubscribe(drag.subscribe);
  const d = drag.current;
  if (!d) return null;
  return (
    <div class="drag-ghost" style={{ left: len(d.x + 14), top: len(d.y + 14) }}>
      {d.label}
    </div>
  );
}

function StatusPill({ client }: { client: Client }) {
  const text = client.error ?? (client.connected ? null : client.state ? "reconnecting…" : "connecting…");
  if (!text) return null;
  return (
    <div id="status" role="status" class={client.error ? "error" : ""}>
      {text}
    </div>
  );
}

// ---------------------------------------------------------------- drops

function dropAllowed(client: Client, what: Dragged, t: Exclude<Target, null>): boolean {
  if (t.kind !== "pane") return true;
  if (what.kind === "pane") return what.pane !== t.pane;
  return t.edge !== "center" && client.tabOfPane(t.pane)?.id !== what.tab;
}

function drop(client: Client, what: Dragged, target: Target) {
  closeMenu();
  if (!target || !dropAllowed(client, what, target)) return;
  const session = client.state?.sessions.find((s) => s.id === client.session);
  if (!session) return;
  let intent: Intent | null = null;
  if (what.kind === "pane" && target.kind === "pane") {
    intent = { op: "move_pane", pane: what.pane, target: target.pane, edge: target.edge };
  } else if (what.kind === "pane" && target.kind === "tabbar") {
    intent = { op: "break_pane", pane: what.pane, session: session.id, index: Math.min(target.index, session.tabs.length) };
  } else if (what.kind === "tab" && target.kind === "pane") {
    intent = { op: "dock_tab", tab: what.tab, target: target.pane, edge: target.edge };
  } else if (what.kind === "tab" && target.kind === "tabbar") {
    const from = session.tabs.indexOf(what.tab);
    let to = Math.min(target.index, session.tabs.length);
    if (from !== -1 && to > from) to -= 1;
    if (to !== from) intent = { op: "move_tab", tab: what.tab, session: session.id, index: to };
  }
  if (intent) client.intent(intent);
}

/** While dragging a pane, resting on a tab for a moment switches to it,
 * so a pane can be dropped into another tab's layout. */
function useHoverToSwitchTabs(client: Client) {
  useEffect(() => {
    let timer: number | undefined;
    let over: TabId | null = null;
    return drag.subscribe(() => {
      const d = drag.current;
      const el = d && document.elementFromPoint(d.x, d.y)?.closest<HTMLElement>("[data-tab-id]");
      const id = el ? Number(el.dataset.tabId) : null;
      if (id === over) return;
      over = id;
      clearTimeout(timer);
      if (d?.what.kind === "pane" && id !== null && id !== client.tab) {
        timer = window.setTimeout(() => client.selectTab(id), 500);
      }
    });
  }, [client]);
}

// ---------------------------------------------------------------- attention

/** Tell the daemon which pane this window is looking at, so it doesn't
 * notify you about the pane in front of you. */
function useReportFocus(client: Client, phone: boolean) {
  const active = client.active();
  useEffect(() => {
    const report = () => {
      const looking = document.visibilityState === "visible" && (phone || document.hasFocus());
      client.focusPane(looking ? (client.active() ?? null) : null);
    };
    report();
    window.addEventListener("focus", report);
    window.addEventListener("blur", report);
    document.addEventListener("visibilitychange", report);
    return () => {
      window.removeEventListener("focus", report);
      window.removeEventListener("blur", report);
      document.removeEventListener("visibilitychange", report);
    };
  }, [client, active, phone, client.connected]);
}

// ---------------------------------------------------------------- push

let push: PushState = "unsupported";
void pushState().then((s) => (push = s));

function notificationItems(client: Client): MenuItem[] {
  if (push === "unsupported") return [{ label: "Notifications need HTTPS", disabled: true, run: () => {} }];
  if (push === "denied") return [{ label: "Notifications are blocked", disabled: true, run: () => {} }];
  return [
    {
      label: "Notify this device",
      checked: push === "on",
      run: async () => {
        try {
          push = push === "on" ? await disablePush() : await enablePush();
        } catch (e) {
          client.toast(String(e));
        }
        client.emit();
      },
    },
  ];
}
