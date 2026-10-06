// The chat page: every thread (M61) on every machine in one place, like a
// team chat. Sessions are the channels, each pane's thread sits under its
// session, and each one links to the pane or session it's about. It's
// `/#chat`, a page of its own beside the panes and the swarm (M73): its own
// bar (the desktop app's titlebar), a sidebar of channels and the thread.
// The panes stay mounted under it, so their terminals keep their size. On
// a phone the list and a thread take the screen in turn.
//
// The shown host's threads come from the page's own connection; the other
// hosts' from the fleet's summary connections, which get threads too.
//
// A session's huddle (M63) shows on its channel row and in its header.

import { useEffect, useState } from "preact/hooks";
import type { Client } from "../client";
import type { Fleet } from "../fleet";
import { directory } from "../hosts";
import { openSwarm } from "../swarm/route";
import { threadKey, type SessionId, type ThreadMsg, type ThreadSummary, type ThreadTarget } from "../proto";
import { getFleet } from "./hosts";
import { usePhone } from "./hooks";
import { openMenu, type MenuItem } from "./menu";
import { ActivityView, SearchField, SearchView, Switcher, activityCount, useChatKeys } from "./chat-find";
import { closeThread, ThreadBody, type Quote } from "./threads";
import { HuddleButton, HuddleChip, useHuddle } from "./huddle";
import { Avatar } from "./people";
import { UpdateChip } from "./update";
import { WindowButtons } from "./window-buttons";

// ---- the route

const listeners = new Set<() => void>();
const changed = () => listeners.forEach((fn) => fn());
addEventListener("hashchange", changed);

export interface ChatRoute {
  host?: string;
  key?: string;
  /** M74: a message to show (a copied link). */
  msg?: number;
  /** M75: Activity (your mentions), or search results for `q`. */
  view?: "activity" | "search";
  q?: string;
}

/** `#chat`, `#chat=<host>/<pane-N|session-N>` for one thread, and
 * `…&msg=N` for one message in it; `#chat=activity`, and
 * `#chat=search/<words>`. */
function chatRoute(): ChatRoute | null {
  if (location.hash === "#chat=activity") return { view: "activity" };
  const q = /^#chat=search\/(.*)$/.exec(location.hash);
  if (q) return { view: "search", q: safeDecode(q[1]) };
  const m = /^#chat(?:=(.*)\/((?:pane|session)-\d+)(?:&msg=(\d+))?)?$/.exec(location.hash);
  if (!m) return null;
  return m[2] ? { host: safeDecode(m[1]), key: m[2], msg: m[3] ? Number(m[3]) : undefined } : {};
}

function safeDecode(s: string): string {
  try {
    return decodeURIComponent(s);
  } catch {
    return s;
  }
}

/** Activity, or search results for `q` (M75). */
export function openChatView(view: "activity" | "search", q = "") {
  closeThread();
  const hash = view === "activity" ? "chat=activity" : `chat=search/${encodeURIComponent(q)}`;
  if (location.hash === `#${hash}`) return changed();
  // Typing in search replaces the entry, so Back doesn't step through it.
  if (view === "search" && chatRoute()?.view === "search") {
    history.replaceState(null, "", `#${hash}`);
    changed();
  } else location.hash = hash;
}

/** Whether the chat page is shown, kept current with the route. */
export function useChatOpen(): boolean {
  const [, setTick] = useState(0);
  useEffect(() => {
    const fn = () => setTick((t) => t + 1);
    listeners.add(fn);
    return () => void listeners.delete(fn);
  }, []);
  return isChatOpen();
}

export function openChat(host?: string, target?: ThreadTarget, msg?: number) {
  closeThread();
  const hash =
    host !== undefined && target ? `chat=${encodeURIComponent(host)}/${threadKey(target)}${msg !== undefined ? `&msg=${msg}` : ""}` : "chat";
  if (location.hash !== `#${hash}`) location.hash = hash;
  else changed();
}

export function closeChat() {
  history.replaceState(null, "", location.pathname + location.search);
  changed();
}

export function isChatOpen(): boolean {
  return chatRoute() !== null;
}

function targetOf(key: string): ThreadTarget | null {
  const m = /^(pane|session)-(\d+)$/.exec(key);
  if (!m) return null;
  return m[1] === "pane" ? { pane: Number(m[2]) } : { session: Number(m[2]) };
}

// ---- where the threads come from

export interface Source {
  host: string;
  client: Client;
  /** The page's own connection (it can show panes; the rest go there). */
  shown: boolean;
}

/** Every host with threads to show: the shown one through the page's own
 * client, the others through the fleet's, when they keep threads. */
export function sources(client: Client, fleet: Fleet | null): Source[] {
  const current = directory.current;
  const out: Source[] = [{ host: current, client, shown: true }];
  for (const h of fleet?.list ?? []) {
    if (h.name === current) continue;
    const c = fleet!.clientOf(h.name);
    if (c?.state && c.hasThreads()) out.push({ host: h.name, client: c, shown: false });
  }
  return out;
}

/** Unread messages in every thread this person sees, and whether one
 * mentions them: for the bar's button. */
export function chatUnread(client: Client): { n: number; mention: boolean } {
  let n = 0;
  let mention = false;
  for (const s of sources(client, getFleet())) {
    for (const t of s.client.state?.threads ?? []) {
      n += t.unread ?? 0;
      mention ||= !!t.mention;
    }
  }
  return { n, mention };
}

/** Re-render on any host's change, and on the route's. */
function useChatTick(client: Client) {
  const [, setTick] = useState(0);
  useEffect(() => {
    const fn = () => setTick((t) => t + 1);
    listeners.add(fn);
    const offs = [client.subscribe(fn), getFleet()?.subscribe(fn)];
    return () => {
      listeners.delete(fn);
      for (const off of offs) off?.();
    };
  }, [client]);
}

// ---- the places: the panes, the swarm and chat

/** Panes · Swarm · Chat, in the panes' bar and in chat's. */
export function Places({ client, at }: { client: Client; at: "panes" | "chat" }) {
  useChatTick(client);
  const { n, mention } = client.hasThreads() ? chatUnread(client) : { n: 0, mention: false };
  return (
    <div class="places" role="group" aria-label="Places">
      <button class={at === "panes" ? "place here" : "place"} title="Your panes" data-open-panes aria-pressed={at === "panes"} onClick={() => at === "chat" && closeChat()}>
        Panes
      </button>
      <button class="place" title="Every pane, everywhere (the swarm)" data-open-swarm onClick={openSwarm}>
        Swarm
      </button>
      {client.hasThreads() && (
        <button
          class={at === "chat" ? "place here" : "place"}
          title="Chat: every thread, on every machine"
          data-open-chat
          aria-pressed={at === "chat"}
          onClick={() => at === "panes" && openChat()}
        >
          Chat
          {n > 0 && <span class={mention ? "chat-count mention" : "chat-count"}>{mention ? `@${n}` : n}</span>}
        </button>
      )}
    </div>
  );
}

/** Chat's own bar, which is the desktop app's titlebar on this page. */
function ChatBar({ client, route }: { client: Client; route: ChatRoute }) {
  return (
    <header class="bar chat-bar" data-tauri-drag-region>
      <Places client={client} at="chat" />
      <div class="bar-fill" data-tauri-drag-region />
      <SearchField route={route} />
      <div class="bar-fill" data-tauri-drag-region />
      <UpdateChip client={client} />
      <WindowButtons />
    </header>
  );
}

// ---- what this browser remembers about the sidebar

function remembered<T>(key: string, fallback: T): T {
  try {
    const v = localStorage.getItem(key);
    return v === null ? fallback : (JSON.parse(v) as T);
  } catch {
    return fallback;
  }
}

function remember(key: string, v: unknown) {
  try {
    localStorage.setItem(key, JSON.stringify(v));
  } catch {
    // private window: it lasts for this page only
  }
}

const SIDE_MIN = 200;
const SIDE_MAX = 420;
let sideWidth = remembered("chat.side", 260);
let collapsed = new Set<string>(remembered<string[]>("chat.collapsed", []));
let unreadOnly = remembered("chat.unreadOnly", false);

// ---- the view

export interface Row {
  host: string;
  target: ThreadTarget;
  label: string;
  /** A pane's thread is shown under its session. */
  sub: boolean;
  summary?: ThreadSummary;
}

/** A host's channels: each session, with its panes' threads under it,
 * newest first. */
export function rowsOf(s: Source): Row[] {
  const st = s.client.state;
  if (!st) return [];
  const summaries = new Map((st.threads ?? []).map((t) => [threadKey(t.target), t]));
  const out: Row[] = [];
  const placed = new Set<string>();
  for (const session of st.sessions) {
    const key = threadKey({ session: session.id });
    out.push({ host: s.host, target: { session: session.id }, label: session.name, sub: false, summary: summaries.get(key) });
    placed.add(key);
    const panes = (st.threads ?? [])
      .filter((t): t is ThreadSummary & { target: { pane: number } } => "pane" in t.target)
      .filter((t) => s.client.sessionOfPane(t.target.pane) === session.id)
      .sort((a, b) => b.at - a.at);
    for (const t of panes) {
      placed.add(threadKey(t.target));
      out.push({ host: s.host, target: t.target, label: paneLabel(s.client, t.target.pane), sub: true, summary: t });
    }
  }
  // A pane whose session this client can't place (a remote pane, say).
  for (const t of st.threads ?? []) {
    if (placed.has(threadKey(t.target)) || !("pane" in t.target)) continue;
    out.push({ host: s.host, target: t.target, label: paneLabel(s.client, t.target.pane), sub: false, summary: t });
  }
  return out;
}

export function paneLabel(client: Client, pane: number): string {
  const t = client.title(pane) || client.cwd(pane)?.split("/").filter(Boolean).pop() || "";
  return t ? `%${pane} ${t}` : `%${pane}`;
}

export function sessionName(client: Client, session: SessionId): string {
  return client.state?.sessions.find((s) => s.id === session)?.name ?? `session ${session}`;
}

/** Where a thread links to, and how to get there. */
function goTo(s: Source, target: ThreadTarget, then?: (c: Client) => void) {
  closeChat();
  if (s.shown) {
    if ("pane" in target) s.client.setActive(target.pane);
    else s.client.selectSession(target.session);
    then?.(s.client);
    return;
  }
  // Another host: the page switches to it, then shows the pane.
  const fleet = getFleet();
  if ("pane" in target) fleet?.open(s.host, target.pane);
  else directory.select(s.host);
}

export function ChatPage({ client }: { client: Client }) {
  useChatTick(client);
  const route = chatRoute();
  const open = !!route && client.hasThreads();
  // The huddle bar docks in the sidebar's footer on this page (style.css).
  useEffect(() => {
    document.documentElement.classList.toggle("chat-open", open);
    return () => document.documentElement.classList.remove("chat-open");
  }, [open]);
  if (!open) return null;
  return <ChatView client={client} route={route!} />;
}

/** The sidebar's title: the team's, when every machine here is one team's. */
function workspaceName(all: Source[], fleet: Fleet | null): string {
  const people = new Set<string>();
  let team: string | null = null;
  for (const s of all) {
    const h = fleet?.host(s.host);
    const p = h ? fleet!.personOf(h) : { id: "me", name: "", kind: "me" as const };
    people.add(p.id);
    if (p.kind === "team") team = p.name;
  }
  if (people.size === 1 && team) return team;
  return all.length > 1 ? "Your machines" : all[0]?.host || "This machine";
}

function markAllRead(all: Source[]) {
  for (const s of all) for (const t of s.client.state?.threads ?? []) if (t.unread) s.client.markThreadRead(t.target, t.last);
}

function ChatView({ client, route }: { client: Client; route: ChatRoute }) {
  const phone = usePhone();
  const fleet = getFleet();
  const huddle = useHuddle();
  const [, setTick] = useState(0);
  const redraw = () => setTick((t) => t + 1);
  const all = sources(client, fleet);
  const multi = all.length > 1;
  const rows = all.map((s) => ({ s, rows: rowsOf(s) }));

  // The thread shown: the route's, else the newest unread, else the newest
  // (on a phone, the list until one is picked).
  let picked: { s: Source; target: ThreadTarget } | null = null;
  const fromRoute = route.key ? targetOf(route.key) : null;
  const src = all.find((s) => s.host === route.host);
  if (fromRoute && src) picked = { s: src, target: fromRoute };
  if (!picked && !phone && !route.view) {
    const flat = rows.flatMap(({ s, rows }) => rows.map((r) => ({ s, r })));
    const best =
      flat.filter((x) => x.r.summary?.unread).sort((a, b) => b.r.summary!.at - a.r.summary!.at)[0] ??
      flat.filter((x) => x.r.summary).sort((a, b) => b.r.summary!.at - a.r.summary!.at)[0] ??
      flat[0];
    if (best) picked = { s: best.s, target: best.r.target };
  }

  // The sidebar's width, on the root: the docked huddle bar reads it too.
  useEffect(() => {
    document.documentElement.style.setProperty("--chat-side", `${sideWidth}px`);
  });
  useEffect(() => () => document.documentElement.style.removeProperty("--chat-side"), []);

  const pickedKey = picked ? `${picked.s.host}/${threadKey(picked.target)}` : null;

  const row = (s: Source, r: Row) => {
    const key = `${r.host}/${threadKey(r.target)}`;
    const n = r.summary?.unread ?? 0;
    return (
      <button
        key={key}
        class={`chat-row${r.sub ? " sub" : ""}${key === pickedKey ? " selected" : ""}${n ? " unread" : ""}${r.summary ? "" : " quiet"}`}
        data-chat-thread={threadKey(r.target)}
        title={"pane" in r.target ? `This pane's thread` : `The session's thread`}
        onClick={() => openChat(r.host, r.target)}
      >
        <span class="chat-sigil">{"pane" in r.target ? "↳" : "#"}</span>
        <span class="chat-label">{r.label}</span>
        {"session" in r.target && <HuddleChip client={s.client} session={r.target.session} />}
        {n > 0 && <span class={r.summary?.mention ? "chat-count mention" : "chat-count"}>{r.summary?.mention ? `@${n}` : n}</span>}
      </button>
    );
  };

  // Sessions with a huddle on, at the top, as in a team chat's sidebar.
  const huddles = rows.flatMap(({ s, rows }) => rows.filter((r) => "session" in r.target && s.client.call(r.target.session)).map((r) => ({ s, r })));

  const toggle = (host: string) => {
    if (collapsed.has(host)) collapsed.delete(host);
    else collapsed.add(host);
    remember("chat.collapsed", [...collapsed]);
    redraw();
  };

  // M75: Alt+↑/↓ through the channels, Shift+Esc marks everything read.
  const flatRows = rows.flatMap(({ s, rows }) => (phone || !collapsed.has(s.host) ? rows : []).map((r) => ({ s, r })));
  useChatKeys(flatRows, pickedKey, () => markAllRead(all));
  const mentions = activityCount(all);

  const list = (
    <nav class="chat-list" aria-label="Threads">
      <div class="chat-top">
        <button class={`chat-row chat-activity${route.view === "activity" ? " selected" : ""}${mentions ? " unread" : ""}`} data-chat-activity onClick={() => openChatView("activity")}>
          <span class="chat-sigil">@</span>
          <span class="chat-label">Activity</span>
          {mentions > 0 && <span class="chat-count mention">{mentions}</span>}
        </button>
      </div>
      {huddles.length > 0 && (
        <section data-chat-huddles>
          <h2 class="chat-section">Huddles</h2>
          {huddles.map(({ s, r }) => row(s, { ...r, sub: false, label: multi ? `${r.label} · ${s.host || "this machine"}` : r.label }))}
        </section>
      )}
      {rows.map(({ s, rows }) => {
        // One machine: its name is the sidebar's title already.
        const name = multi ? s.host || "this machine" : "Sessions";
        const shut = !phone && collapsed.has(s.host);
        const shown = unreadOnly ? rows.filter((r) => r.summary?.unread || `${r.host}/${threadKey(r.target)}` === pickedKey) : rows;
        const unread = rows.reduce((n, r) => n + (r.summary?.unread ?? 0), 0);
        return (
          <section key={s.host}>
            <h2 class="chat-section">
              <button aria-expanded={!shut} onClick={() => toggle(s.host)} title={shut ? "Show its channels" : "Hide its channels"}>
                <span class="chat-caret" aria-hidden="true">
                  {shut ? "▸" : "▾"}
                </span>
                {name}
                {shut && unread > 0 && <span class="chat-count">{unread}</span>}
              </button>
            </h2>
            {!shut && rows.length === 0 && <p class="chat-none">No sessions.</p>}
            {!shut && rows.length > 0 && shown.length === 0 && <p class="chat-none">Nothing unread.</p>}
            {!shut && shown.map((r) => row(s, r))}
          </section>
        );
      })}
    </nav>
  );

  const thread = route.view ? (
    route.view === "activity" ? (
      <ActivityView all={all} multi={multi} phone={phone} />
    ) : (
      <SearchView all={all} q={route.q ?? ""} multi={multi} phone={phone} />
    )
  ) : (
    picked && <ChatThread key={pickedKey} s={picked.s} target={picked.target} phone={phone} multi={multi} focus={route.msg} />
  );

  if (phone) {
    return (
      <div class="chat phone" data-chat>
        {picked || route.view ? (
          thread
        ) : (
          <>
            <header class="chat-head">
              <strong>Chat</strong>
              <button class="thread-close" title="Close" onClick={closeChat}>
                ✕
              </button>
            </header>
            {list}
          </>
        )}
      </div>
    );
  }

  const workspaceMenu = (e: MouseEvent) => {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    const items: MenuItem[] = [
      { label: "New session", run: () => client.intent({ op: "new_session", name: null, from_pane: null }) },
      { label: "Mark all as read", run: () => markAllRead(all) },
      "separator",
      { label: "Show every channel", checked: !unreadOnly, run: () => ((unreadOnly = false), remember("chat.unreadOnly", false), redraw()) },
      { label: "Show unread only", checked: unreadOnly, run: () => ((unreadOnly = true), remember("chat.unreadOnly", true), redraw()) },
    ];
    openMenu({ clientX: r.left, clientY: r.bottom + 4, preventDefault: () => e.preventDefault() }, items);
  };

  // Drag the sidebar's edge to widen it; this browser keeps the width.
  const resize = (e: PointerEvent) => {
    e.preventDefault();
    const x0 = e.clientX;
    const w0 = sideWidth;
    const move = (m: PointerEvent) => {
      sideWidth = Math.max(SIDE_MIN, Math.min(SIDE_MAX, w0 + m.clientX - x0));
      redraw();
    };
    const up = () => {
      removeEventListener("pointermove", move);
      removeEventListener("pointerup", up);
      remember("chat.side", sideWidth);
    };
    addEventListener("pointermove", move);
    addEventListener("pointerup", up);
  };

  return (
    <div class="chat" data-chat>
      <ChatBar client={client} route={route} />
      <Switcher rows={flatRows} multi={multi} />
      <div class="chat-body">
        <aside class={huddle ? "chat-side huddling" : "chat-side"}>
          <header class="chat-side-head">
            <button class="chat-workspace" data-chat-workspace onClick={workspaceMenu} onContextMenu={workspaceMenu}>
              {workspaceName(all, fleet)} <span class="caret">▾</span>
            </button>
          </header>
          {list}
          <div class="chat-resize" role="separator" aria-orientation="vertical" title="Drag to resize" onPointerDown={resize} />
        </aside>
        {thread ?? <div class="chat-thread chat-empty">No threads yet. Start one from a pane's menu, or pick a session.</div>}
      </div>
    </div>
  );
}

/** A thread's link, as the page's address with its fragment. */
function linkTo(host: string, target: ThreadTarget, msg?: number): string {
  const hash = `#chat=${encodeURIComponent(host)}/${threadKey(target)}${msg !== undefined ? `&msg=${msg}` : ""}`;
  return `${location.origin}${location.pathname}${location.search}${hash}`;
}

/** What the header's second line says: where it is and what it's doing. */
function topicOf(s: Source, target: ThreadTarget, multi: boolean): string {
  const c = s.client;
  const parts: string[] = [];
  if (multi) parts.push(s.host || "this machine");
  if ("pane" in target) {
    const session = c.sessionOfPane(target.pane);
    if (session !== null) parts.push(`# ${sessionName(c, session)}`);
    const info = c.info(target.pane);
    if (info?.cwd) parts.push(info.cwd.replace(/^\/home\/[^/]+|^\/Users\/[^/]+/, "~"));
    const doing = info?.current?.text ?? info?.command;
    if (doing) parts.push(doing);
  } else {
    const n = panesOf(c, target.session).length;
    parts.push(n === 1 ? "1 pane" : `${n} panes`);
  }
  return parts.join(" · ");
}

function panesOf(c: Client, session: SessionId): number[] {
  return (c.state?.panes ?? []).filter((p) => c.sessionOfPane(p.id) === session && p.type !== "remote").map((p) => p.id);
}

/** Who's in it: people here now, then whoever has posted. */
function membersOf(c: Client, target: ThreadTarget, msgs: ThreadMsg[]): { who: string; name: string; pic?: string }[] {
  const out = new Map<string, { who: string; name: string; pic?: string }>();
  const session = "pane" in target ? c.sessionOfPane(target.pane) : target.session;
  for (const p of c.state?.presence ?? []) {
    const here = p.tab === undefined || session === null || c.sessionOfTab(p.tab) === session;
    if (here && !out.has(p.who)) out.set(p.who, { who: p.who, name: p.name, pic: p.pic });
  }
  for (const m of msgs) if (!m.agent && !out.has(m.who)) out.set(m.who, { who: m.who, name: m.name, pic: m.pic });
  return [...out.values()];
}

let detailsOpen = (() => {
  try {
    return localStorage.getItem("chat.details") === "1";
  } catch {
    return false;
  }
})();

function ChatThread({ s, target, phone, multi, focus }: { s: Source; target: ThreadTarget; phone: boolean; multi: boolean; focus?: number }) {
  const c = s.client;
  const pane = "pane" in target ? target.pane : null;
  const session = pane !== null ? c.sessionOfPane(pane) : (target as { session: number }).session;
  const name = pane !== null ? paneLabel(c, pane) : sessionName(c, session!);
  const alive = pane !== null ? !!c.info(pane) : !!c.state?.sessions.some((x) => x.id === session);
  const [msgs, setMsgs] = useState<ThreadMsg[]>([]);
  const [details, setDetails] = useState(detailsOpen && !phone);
  const members = membersOf(c, target, msgs);

  // A quote: its pane, with the output shown when this page has it.
  const reveal = (q: Quote): string | null => {
    if (!c.info(q.pane)) return `%${q.pane} is closed; the quote is all that's left`;
    goTo(s, { pane: q.pane }, (cl) => cl.panes.get(q.pane)?.view.reveal(q.text));
    return null;
  };

  const toggleDetails = () => {
    detailsOpen = !details;
    try {
      localStorage.setItem("chat.details", detailsOpen ? "1" : "0");
    } catch {
      // kept for this page only
    }
    setDetails(detailsOpen);
  };

  return (
    <div class="chat-main">
      <section class="chat-thread" aria-label={name}>
        <header class="chat-head">
          {phone && (
            <button class="chat-back" title="Every thread" onClick={() => openChat()}>
              ‹
            </button>
          )}
          <div class="chat-title">
            <strong>
              <span class="chat-title-sigil">{pane !== null ? "↳" : "#"}</span>
              {name}
            </strong>
            <span data-chat-topic>{topicOf(s, target, multi)}</span>
          </div>
          {!phone && members.length > 0 && (
            <button class="chat-members" title={members.map((m) => m.name).join(", ")} onClick={() => !details && toggleDetails()}>
              {members.slice(0, 3).map((m) => (
                <Avatar key={m.who} p={m} />
              ))}
              <span>{members.length}</span>
            </button>
          )}
          {session !== null && c.state?.sessions.some((x) => x.id === session) && (
            <HuddleButton client={c} session={session} label />
          )}
          {alive && (
            <button class="chat-go" data-chat-go onClick={() => goTo(s, target)}>
              {pane !== null ? "Go to pane" : "Go to session"}
            </button>
          )}
          {!phone && (
            <button
              class={details ? "chat-info on" : "chat-info"}
              title={details ? "Hide details" : "Details: the pane, and who's here"}
              aria-pressed={details}
              data-chat-details
              onClick={toggleDetails}
            >
              ⓘ
            </button>
          )}
          {phone && (
            <button class="thread-close" title="Close" onClick={closeChat}>
              ✕
            </button>
          )}
        </header>
        <ThreadBody
          client={c}
          target={target}
          phone={phone}
          reveal={reveal}
          extras={{
            link: (id) => linkTo(s.host, target, id),
            goTo: alive ? () => goTo(s, target) : undefined,
            onMessages: setMsgs,
            focus,
            draftKey: `${s.host}/${threadKey(target)}`,
            name: pane !== null ? name : `#${name}`,
          }}
        />
      </section>
      {details && <ChatDetails s={s} target={target} members={members} close={toggleDetails} />}
    </div>
  );
}

/** The right-hand panel: the pane itself, live (or the session's panes),
 * and who's in it. */
function ChatDetails({
  s,
  target,
  members,
  close,
}: {
  s: Source;
  target: ThreadTarget;
  members: { who: string; name: string; pic?: string }[];
  close: () => void;
}) {
  const c = s.client;
  const panes = "pane" in target ? [target.pane] : panesOf(c, target.session);
  return (
    <aside class="chat-details" aria-label="Details" data-chat-details-panel>
      <header class="chat-head">
        <strong>Details</strong>
        <button class="thread-close" title="Close" onClick={close}>
          ✕
        </button>
      </header>
      <div class="chat-details-body">
        <h3>{panes.length === 1 && "pane" in target ? "The pane, live" : "Its panes, live"}</h3>
        {panes.length === 0 && <p class="chat-none">No panes.</p>}
        {panes.map((p) => (
          <PanePeek key={p} s={s} pane={p} lines={"pane" in target ? 24 : 6} open={() => openChat(s.host, { pane: p })} single={"pane" in target} />
        ))}
        <h3>People</h3>
        {members.length === 0 && <p class="chat-none">Just you, so far.</p>}
        <ul class="chat-people">
          {members.map((m) => (
            <li key={m.who}>
              <Avatar p={m} />
              <span>{m.name}</span>
            </li>
          ))}
        </ul>
      </div>
    </aside>
  );
}

/** A pane's screen as text, refreshed while it's shown. */
function PanePeek({ s, pane, lines, open, single }: { s: Source; pane: number; lines: number; open: () => void; single: boolean }) {
  const [text, setText] = useState<string | null>(null);
  useEffect(() => {
    let live = true;
    let timer: number | undefined;
    const tick = async () => {
      try {
        const res = await s.client.request("GET", `/api/panes/${pane}/capture?format=text`);
        const t = res.ok && res.text ? await res.text() : "";
        const all = t.split("\n").map((l) => l.trimEnd());
        while (all.length && !all[all.length - 1]) all.pop();
        if (live) setText(all.slice(-lines).join("\n"));
      } catch {
        if (live) setText((t) => t ?? "");
      }
      if (live) timer = window.setTimeout(tick, document.visibilityState === "visible" ? 1500 : 5000);
    };
    void tick();
    return () => {
      live = false;
      clearTimeout(timer);
    };
  }, [s.client, pane, lines]);
  return (
    <div class="chat-peek">
      {!single && (
        <button class="chat-peek-title" onClick={open} title="Its thread">
          ↳ {paneLabel(s.client, pane)}
        </button>
      )}
      <pre data-chat-peek={pane}>{text ?? "…"}</pre>
    </div>
  );
}
