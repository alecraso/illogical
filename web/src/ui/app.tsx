import { Fragment } from "preact";
import { useEffect, useLayoutEffect, useRef, useState } from "preact/hooks";
import { paneIds, tabLabel, type Client } from "../client";
import type { Edge, Intent, PaneId, Rect, SplitRect, TabId, TabView } from "../proto";
import type { Cell } from "./cells";
import { drag, startDrag, type Dragged, type Target } from "./drag";
import { useSubscribe, usePhone } from "./hooks";
import { closeMenu, MenuLayer, openMenu, PromptLayer, type MenuItem } from "./menu";
import { KeyBar, PhoneHeader } from "./phone";
import { ThreadBadge, ThreadLayer } from "./threads";
import { HuddleBar, HuddleButton } from "./huddle";
import { ChatPage, Places, useChatOpen } from "./chat";
import { AttentionBadge, tabAttention } from "./attention";
import { HostButton, HostPicker } from "./hosts";
import { ControlRequests, PaneMarks, PeopleBar, ShareDialog, TabPeople } from "./people";
import { directory } from "../hosts";
import { SandboxesLayer } from "./sandboxes";
import { RulesLayer } from "./rules";
import { useWorkspaceDir } from "../blocks";
import { AgentDialogLayer } from "./agent-dialog";
import { ConversationsLayer } from "./conversations";
import { AppsLayer } from "./apps";
import { PickerLayer, usePickerShortcut } from "./picker";
import { TermAnswered, TermAsk, TermDiff } from "./term-ask";
import { InstallHint } from "./notify";
import { GettingStartedLayer, useFirstRun } from "./welcome";
import { machineState, newTabItems, PALETTE_KEY, paneItems, sessionItems, tabItems } from "./commands";
import { openPalette, PaletteLayer, usePaletteShortcut } from "./palette";
import { UpdateChip } from "./update";
import { ControlBanner } from "./control-state";
import { AgentsNudge } from "./agents-setup";
import { WindowButtons } from "./window-buttons";

/** Where hidden panes' terminals live: off the page but still alive. */
const parking = document.createElement("div");
parking.id = "parking";
document.body.appendChild(parking);

type Renaming = { kind: "tab" | "session"; id: number } | null;

/** How long a hidden page keeps its connection to a sandbox host. */
const HIDDEN_GRACE_MS = 10_000;

export function App({ client, cell }: { client: Client; cell: Cell }) {
  useSubscribe((fn) => client.subscribe(fn));
  const phone = usePhone();
  const [renaming, setRenaming] = useState<Renaming>(null);

  useEffect(() => {
    // Focusing the window doesn't take the size from another window
    // (#333): typing here, showing a tab or "use this size" does.
    // A sandbox host sleeps when nothing holds it awake, and an open
    // connection does: let go of it while the page is hidden.
    let hidden: number | undefined;
    const visible = () => {
      clearTimeout(hidden);
      if (document.visibilityState === "visible") client.wake();
      else if (directory.sleeps) hidden = window.setTimeout(() => client.sleep(), HIDDEN_GRACE_MS);
    };
    document.addEventListener("visibilitychange", visible);
    return () => {
      clearTimeout(hidden);
      document.removeEventListener("visibilitychange", visible);
    };
  }, [client]);

  // Following someone stops as soon as you do something yourself.
  useEffect(() => {
    const stop = (e: Event) => {
      if (client.following !== null && !(e.target as HTMLElement).closest?.(".avatar")) client.follow(null);
    };
    window.addEventListener("pointerdown", stop, true);
    window.addEventListener("keydown", stop, true);
    return () => {
      window.removeEventListener("pointerdown", stop, true);
      window.removeEventListener("keydown", stop, true);
    };
  }, [client]);

  useHoverToSwitchTabs(client);
  useReportFocus(client, phone);
  usePickerShortcut(client, phone);
  usePaletteShortcut(client, phone);
  useFirstRun(client);

  const state = client.state;
  const tab = client.tabView();
  // M73: the chat page covers the panes, which stay laid out under it (so
  // their terminals keep their size) but can't be reached.
  const chatOpen = useChatOpen() && !!state && client.hasThreads();
  return (
    <div class={phone ? "app phone" : "app"}>
      {state && (phone ? (
        <PhoneHeader client={client} />
      ) : (
        <TopBar client={client} renaming={renaming} setRenaming={setRenaming} inert={chatOpen} />
      ))}
      {state && <ControlBanner client={client} />}
      {state && <AgentsNudge client={client} />}
      <main class="main" inert={chatOpen}>
        {!state ? (
          <HostPicker />
        ) : state.sessions.length === 0 ? (
          <NoSessions client={client} phone={phone} />
        ) : tab ? (
          <TabArea client={client} tab={tab} cell={cell} phone={phone} />
        ) : null}
      </main>
      {phone && state && state.sessions.length > 0 && client.panes.has(client.active() ?? -1) && <KeyBar client={client} />}
      <MenuLayer />
      <PromptLayer />
      <AgentDialogLayer />
      <ThreadLayer phone={phone} />
      <HuddleBar />
      {state && <ChatPage client={client} />}
      <ConversationsLayer />
      <AppsLayer />
      <SandboxesLayer />
      <RulesLayer />
      <PickerLayer />
      <PaletteLayer />
      <GettingStartedLayer />
      {phone && state && <InstallHint />}
      <DragGhost />
      <ControlRequests client={client} />
      <ShareDialog client={client} />
      <StatusPill client={client} />
    </div>
  );
}

/** The empty state, below the bar. Through control it says which machine
 * this is and where to switch, since the machine may not be the one meant. */
function NoSessions({ client, phone }: { client: Client; phone: boolean }) {
  useSubscribe((fn) => directory.subscribe(fn));
  return (
    <div class="empty">
      <p>No sessions.</p>
      {directory.control && (
        <p data-no-sessions-host>
          No sessions on {directory.current}. Switch machines from the {phone ? "menu at the top" : "menu at the top left"}.
        </p>
      )}
      <button class="primary" onClick={() => client.intent({ op: "new_session", name: null, from_pane: null })}>
        New session
      </button>
    </div>
  );
}

// ---------------------------------------------------------------- top bar

function TopBar({
  client,
  renaming,
  setRenaming,
  inert,
}: {
  client: Client;
  renaming: Renaming;
  setRenaming: (r: Renaming) => void;
  inert: boolean;
}) {
  useSubscribe(drag.subscribe);
  const state = client.state!;
  // None at all, on a machine with no sessions: the bar is still here for
  // the host and account menus.
  const session = state.sessions.find((s) => s.id === client.session) ?? state.sessions[0];
  const target = drag.current?.target;
  const marker = session && target?.kind === "tabbar" ? Math.min(target.index, session.tabs.length) : null;

  const sessionMenu = (e: MouseEvent) => {
    if (!session) return;
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    openFresh(client, { clientX: r.left, clientY: r.bottom + 4, preventDefault: () => e.preventDefault() }, () => [
      ...state.sessions.map((s) => ({
        label: `${s.id === session.id ? "✓ " : "    "}${s.name}`,
        run: () => client.selectSession(s.id),
      })),
      "separator",
      ...sessionItems(client, session.id, () => setRenaming({ kind: "session", id: session.id })),
      { label: "Command palette…", shortcut: PALETTE_KEY, run: () => openPalette(client) },
    ]);
  };

  return (
    <header class="bar" data-tauri-drag-region inert={inert}>
      <HostButton />
      <Places client={client} at="panes" />
      {session && (
        <>
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
              {session.name}
              {/* M61: the session's thread has messages you haven't read. */}
              {client.thread({ session: session.id })?.unread ? (
                <span class={client.thread({ session: session.id })?.mention ? "session-unread mention" : "session-unread"} title="New in the session thread" />
              ) : null}{" "}
              <span class="caret">▾</span>
            </button>
          )}
          <HuddleButton client={client} session={session.id} />
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
              title={client.has("vms") ? "New tab (right-click for a VM tab)" : "New tab (right-click for more)"}
              onClick={() => client.intent({ op: "new_tab", session: session.id, from_pane: client.active() ?? null })}
              onContextMenu={(e) => openFresh(client, e, () => newTabItems(client, session.id))}
            >
              +
            </button>
          </div>
        </>
      )}
      <div class="bar-fill" data-tauri-drag-region />
      <UpdateChip client={client} />
      <PeopleBar client={client} />
      <WindowButtons />
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
  const machine = client.tabMachine(tab.id);
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
      onContextMenu={(e) => openFresh(client, e, () => tabItems(client, tab, () => setRenaming({ kind: "tab", id: tab.id })))}
    >
      {machine ? (
        <span
          class={`host-tag ${machineState(client, machine)}`}
          title={`${machine.name ?? machine.sprite} (${machine.sprite}): ${machineState(client, machine)}; deleted with the tab`}
        >
          VM
        </span>
      ) : (
        paneIds(tab).some((p) => client.machine(p)) && (
          <span class="host-tag" title="A pane here runs on a throwaway VM">
            VM
          </span>
        )
      )}
      <RemoteTag client={client} tab={tab} />
      <span class="tab-label">{label}</span>
      <TabPeople client={client} tab={tab.id} />
      <AttentionBadge {...tabAttention(client, tab)} client={client} />
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

/** The other hosts a tab's panes run on (#17). */
function RemoteTag({ client, tab }: { client: Client; tab: TabView }) {
  const hosts = [
    ...new Set(
      paneIds(tab)
        .map((p) => client.blocks.get(p)?.state as { host?: string } | null | undefined)
        .flatMap((s) => (s?.host ? [s.host] : [])),
    ),
  ];
  if (!hosts.length) return null;
  return (
    <span class="host-tag remote" title={`Runs on ${hosts.join(", ")}`}>
      {hosts.join(" ")}
    </span>
  );
}

function RenameInput({ value: initial, onDone }: { value: string; onDone: (v: string | null) => void }) {
  // What's typed, so a re-render (live pane updates arrive every second)
  // doesn't put the old name back.
  const [value, setValue] = useState(initial);
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
      onInput={(e) => setValue((e.target as HTMLInputElement).value)}
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
    // Inside its padding: the grid keeps clear of the window's edges (#164).
    const measure = () => {
      const s = getComputedStyle(el);
      const padX = parseFloat(s.paddingLeft) + parseFloat(s.paddingRight);
      const padY = parseFloat(s.paddingTop) + parseFloat(s.paddingBottom);
      setArea({ w: el.clientWidth - padX, h: el.clientHeight - padY });
    };
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
    client.claim = (t, typed) => {
      const s = size.current;
      if (s.cols) client.view(t, s.cols, s.rows, s.zoom, true, typed);
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
    if (!phone && active !== null) client.viewOf(active)?.focus();
  }, [client, tab.id, active, phone]);
  // ...and comes back to it after menus and buttons, unless something else
  // (a rename box) is using the keyboard.
  const rev = client.state?.rev;
  useEffect(() => {
    const el = document.activeElement;
    const idle = !el || el === document.body || el.closest(".menu, .bar button, .tab");
    if (!phone && active !== null && idle) client.viewOf(active)?.focus();
  }, [client, rev, active, phone]);

  const gridW = tab.cols * cell.width;
  const gridH = tab.rows * cell.height;
  const scale = area ? Math.min(1, area.w / gridW, area.h / gridH) : 1;
  const elsewhere = tab.owner !== null && tab.owner !== client.clientId;

  return (
    <div class={scale < 1 ? "tab-area scaled" : "tab-area"} ref={ref}>
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

/** A text box of the page's own, not xterm's hidden one (a terminal's
 * right-click lands there). */
function editable(t: EventTarget | null): boolean {
  return t instanceof HTMLElement && t.matches("textarea, input") && !t.closest(".xterm");
}

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
  // A terminal's entry (for terminal-only features), and the view of any
  // block type.
  const entry = client.panes.get(id);
  const view = client.viewOf(id);

  // Move the block's view in, rather than creating one: moving a block
  // around the layout never restarts or redraws it from scratch.
  useLayoutEffect(() => {
    const slot = ref.current;
    if (!view || !slot) return;
    slot.appendChild(view.host);
    view.setVisible(true);
    return () => {
      if (view.host.parentElement === slot) {
        parking.appendChild(view.host);
        view.setVisible(false);
      }
    };
  }, [view]);

  const info = client.info(id);
  // M34: a chant workspace where it runs, to open as one.
  const workspaceDir = client.cwd(id);
  const isWorkspace = useWorkspaceDir(client, id, info?.type === "terminal" ? workspaceDir : null);

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
    // A text box's own menu (an agent's composer): on a phone, its Paste
    // is the one that can paste an image (Chrome refuses ours).
    if (editable(e.target)) return;
    const items = () => paneItems(client, id, phone, isWorkspace ? workspaceDir : null);
    // A remote pane's menu doesn't depend on this daemon's features.
    if (info?.type === "remote") openMenu(e, items());
    else openFresh(client, e, items);
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
      <HostBadge client={client} id={id} />
      <StartedByBadge client={client} id={id} />
      <PaneMarks client={client} pane={id} />
      {info?.type !== "remote" && <ThreadBadge client={client} pane={id} />}
      {!active && (info?.attention === "needs_input" || info?.attention === "done") && (
        <div class={`pane-badge ${info.attention}`} title={info.reason?.headline}>
          {info.reason?.kind === "failed" ? "failed" : info.reason?.kind === "exited" ? "exited" : info.attention === "done" ? "done" : "needs you"}
        </div>
      )}
      {info?.diff ? <TermDiff client={client} id={id} diff={info.diff} /> : info?.ask && <TermAsk client={client} id={id} ask={info.ask} />}
      {!info?.ask && !info?.diff && info?.answered && info.type === "terminal" && <TermAnswered client={client} id={id} answered={info.answered} />}
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
            startDrag(e, { kind: "pane", pane: id }, client.title(id) || `pane %${id}`, {
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

/** What started a pane, when it was an MCP client (M16): an agent outside, or an agent block's. */
function StartedByBadge({ client, id }: { client: Client; id: PaneId }) {
  const s = client.info(id)?.started_by;
  if (!s) return null;
  const via = s.block !== undefined ? `, from agent block %${s.block}` : "";
  return (
    <div class="started-by" data-started-by={s.by} title={`Started through MCP by ${s.by}${via}. What it typed is in history as theirs.`}>
      started by {s.by}
    </div>
  );
}

/**
 * Where a pane runs, when that isn't where its tab says: a VM pane in an
 * ordinary tab, or a local pane in a VM tab.
 */
function HostBadge({ client, id }: { client: Client; id: PaneId }) {
  const m = client.machine(id);
  const tab = client.tabOfPane(id);
  const tabMachine = tab ? client.tabMachine(tab.id) : undefined;
  if (tabMachine && !m) {
    return (
      <div class="host-badge local" title="Runs on this host, not the tab's machine">
        local
      </div>
    );
  }
  if (!m || m.id === tabMachine?.id) return null;
  const state = m.state === "running" ? "" : ` · ${m.state === "gone" ? "gone" : "starting"}`;
  if (m.borrowed) {
    return (
      <div
        class={`host-badge borrowed ${m.state}`}
        title={`A shell on ${m.sprite} (${m.provider}) with no daemon there: disposable. Its output is kept here while it's attached; the sandbox stays when this pane closes. Make it resident (Sandboxes…) for history that survives.`}
      >
        {m.sprite} · shell{state}
      </div>
    );
  }
  return (
    <div
      class={`host-badge ${m.state}`}
      title={`${m.name ? `${m.name}: ` : ""}${m.sprite} (${m.provider}${m.image ? `, ${m.image}` : ""}); deleted when this pane closes`}
    >
      VM{state}
    </div>
  );
}

/** A menu whose items depend on what the daemon is set up for: read that
 * again first (briefly), so a studio linked a moment ago shows (#180). */
function openFresh(client: Client, e: { clientX: number; clientY: number; preventDefault(): void }, build: () => MenuItem[]) {
  e.preventDefault();
  const at = { clientX: e.clientX, clientY: e.clientY, preventDefault() {} };
  const wait = new Promise((r) => setTimeout(r, 300));
  void Promise.race([client.loadFeatures(), wait]).then(() => openMenu(at, build()));
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
