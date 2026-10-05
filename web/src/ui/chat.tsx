// The chat view: every thread (M61) on every machine in one place, like a
// team chat. Sessions are the channels, each pane's thread sits under its
// session, and each one links to the pane or session it's about. It's
// `/#chat`, over the tabs and under the bar (the bar is the desktop app's
// titlebar); on a phone the list and a thread take the screen in turn.
//
// The shown host's threads come from the page's own connection; the other
// hosts' from the fleet's summary connections, which get threads too.

import { useEffect, useRef, useState } from "preact/hooks";
import type { Client } from "../client";
import type { Fleet } from "../fleet";
import { directory } from "../hosts";
import { threadKey, type SessionId, type ThreadSummary, type ThreadTarget } from "../proto";
import { getFleet } from "./hosts";
import { usePhone } from "./hooks";
import { closeThread, ThreadBody, type Quote } from "./threads";

// ---- the route

const listeners = new Set<() => void>();
const changed = () => listeners.forEach((fn) => fn());
addEventListener("hashchange", changed);

/** `#chat`, or `#chat=<host>/<pane-N|session-N>` for one thread. */
function chatRoute(): { host?: string; key?: string } | null {
  const m = /^#chat(?:=(.*)\/((?:pane|session)-\d+))?$/.exec(location.hash);
  if (!m) return null;
  return m[2] ? { host: decodeURIComponent(m[1]), key: m[2] } : {};
}

// Escape closes it, wherever the focus is: a terminal under the view can
// keep it after a host switch, and doesn't get the key.
addEventListener(
  "keydown",
  (e) => {
    if (e.key !== "Escape" || !chatRoute() || document.querySelector(".menu, .prompt")) return;
    if (!(e.target as HTMLElement).closest?.(".chat")) e.stopPropagation();
    closeChat();
  },
  true,
);

export function openChat(host?: string, target?: ThreadTarget) {
  closeThread();
  const hash = host !== undefined && target ? `chat=${encodeURIComponent(host)}/${threadKey(target)}` : "chat";
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

interface Source {
  host: string;
  client: Client;
  /** The page's own connection (it can show panes; the rest go there). */
  shown: boolean;
}

/** Every host with threads to show: the shown one through the page's own
 * client, the others through the fleet's, when they keep threads. */
function sources(client: Client, fleet: Fleet | null): Source[] {
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

// ---- the bar's button

export function ChatButton({ client }: { client: Client }) {
  useChatTick(client);
  if (!client.hasThreads()) return null;
  const { n, mention } = chatUnread(client);
  const open = isChatOpen();
  return (
    <button
      class={`chat-button${open ? " open" : ""}`}
      title="Chat: every thread, on every machine"
      data-open-chat
      onClick={() => (open ? closeChat() : openChat())}
    >
      Chat
      {n > 0 && <span class={mention ? "chat-count mention" : "chat-count"}>{mention ? `@${n}` : n}</span>}
    </button>
  );
}

// ---- the view

interface Row {
  host: string;
  target: ThreadTarget;
  label: string;
  /** A pane's thread is shown under its session. */
  sub: boolean;
  summary?: ThreadSummary;
}

/** A host's channels: each session, with its panes' threads under it,
 * newest first. */
function rowsOf(s: Source): Row[] {
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

function paneLabel(client: Client, pane: number): string {
  const t = client.title(pane) || client.cwd(pane)?.split("/").filter(Boolean).pop() || "";
  return t ? `%${pane} ${t}` : `%${pane}`;
}

function sessionName(client: Client, session: SessionId): string {
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

export function ChatLayer({ client }: { client: Client }) {
  useChatTick(client);
  const route = chatRoute();
  if (!route || !client.hasThreads()) return null;
  return <ChatView client={client} route={route} />;
}

function ChatView({ client, route }: { client: Client; route: { host?: string; key?: string } }) {
  const phone = usePhone();
  const fleet = getFleet();
  const all = sources(client, fleet);
  const multi = all.length > 1;
  const rows = all.map((s) => ({ s, rows: rowsOf(s) }));

  // The thread shown: the route's, else the newest unread, else the newest
  // (on a phone, the list until one is picked).
  let picked: { s: Source; target: ThreadTarget } | null = null;
  const fromRoute = route.key ? targetOf(route.key) : null;
  const src = all.find((s) => s.host === route.host);
  if (fromRoute && src) picked = { s: src, target: fromRoute };
  if (!picked && !phone) {
    const flat = rows.flatMap(({ s, rows }) => rows.map((r) => ({ s, r })));
    const best =
      flat.filter((x) => x.r.summary?.unread).sort((a, b) => b.r.summary!.at - a.r.summary!.at)[0] ??
      flat.filter((x) => x.r.summary).sort((a, b) => b.r.summary!.at - a.r.summary!.at)[0] ??
      flat[0];
    if (best) picked = { s: best.s, target: best.r.target };
  }

  // Picking a tab or a session in the bar goes back to the panes.
  const at = `${client.session}/${client.tab}`;
  const first = useRef(at);
  useEffect(() => {
    if (at !== first.current) closeChat();
  }, [at]);

  const pickedKey = picked ? `${picked.s.host}/${threadKey(picked.target)}` : null;
  const list = (
    <nav class="chat-list" aria-label="Threads">
      {rows.map(({ s, rows }) => (
        <section key={s.host}>
          {multi && <h2>{s.host || "this machine"}</h2>}
          {rows.length === 0 && <p class="chat-none">No sessions.</p>}
          {rows.map((r) => {
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
                {n > 0 && <span class={r.summary?.mention ? "chat-count mention" : "chat-count"}>{r.summary?.mention ? `@${n}` : n}</span>}
              </button>
            );
          })}
        </section>
      ))}
    </nav>
  );

  const thread = picked && <ChatThread key={pickedKey} s={picked.s} target={picked.target} phone={phone} multi={multi} />;

  return (
    <div class={phone ? "chat phone" : "chat"} data-chat>
      {phone ? (
        picked ? (
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
        )
      ) : (
        <>
          {list}
          {thread ?? <div class="chat-thread chat-empty">No threads yet. Start one from a pane's menu, or pick a session.</div>}
        </>
      )}
    </div>
  );
}

function ChatThread({ s, target, phone, multi }: { s: Source; target: ThreadTarget; phone: boolean; multi: boolean }) {
  const c = s.client;
  const pane = "pane" in target ? target.pane : null;
  const session = pane !== null ? c.sessionOfPane(pane) : (target as { session: number }).session;
  const name = pane !== null ? paneLabel(c, pane) : `# ${sessionName(c, session!)}`;
  const where = [multi ? s.host || "this machine" : null, pane !== null && session !== null ? `# ${sessionName(c, session)}` : null]
    .filter(Boolean)
    .join(" · ");
  const alive = pane !== null ? !!c.info(pane) : !!c.state?.sessions.some((x) => x.id === session);

  // A quote: its pane, with the output shown when this page has it.
  const reveal = (q: Quote): string | null => {
    if (!c.info(q.pane)) return `%${q.pane} is closed; the quote is all that's left`;
    goTo(s, { pane: q.pane }, (cl) => cl.panes.get(q.pane)?.view.reveal(q.text));
    return null;
  };

  return (
    <section class="chat-thread" aria-label={name}>
      <header class="chat-head">
        {phone && (
          <button class="chat-back" title="Every thread" onClick={() => openChat()}>
            ‹
          </button>
        )}
        <div class="chat-title">
          <strong>{name}</strong>
          {where && <span>{where}</span>}
        </div>
        {alive && (
          <button class="chat-go" data-chat-go onClick={() => goTo(s, target)}>
            {pane !== null ? "Go to pane" : "Go to session"}
          </button>
        )}
        {phone && (
          <button class="thread-close" title="Close" onClick={closeChat}>
            ✕
          </button>
        )}
      </header>
      <ThreadBody client={c} target={target} phone={phone} reveal={reveal} />
    </section>
  );
}
