// Threads on panes and sessions (M61): the people working on something talk
// beside it. The daemon that owns the pane keeps them; this is the panel
// (a drawer on the right, the whole screen on a phone), the badge on a
// pane, and the menu items that open them.

import { useEffect, useLayoutEffect, useRef, useState } from "preact/hooks";
import type { Client } from "../client";
import { threadKey, type PaneId, type ThreadMsg, type ThreadTarget } from "../proto";
import { colorOf } from "./people";
import type { MenuItem } from "./menu";

interface Quote {
  pane: PaneId;
  text: string;
}

let open: { client: Client; target: ThreadTarget; quote?: Quote } | null = null;
const listeners = new Set<() => void>();
const changed = () => listeners.forEach((fn) => fn());

/** Show a thread, with output to quote in the next message. */
export function openThread(client: Client, target: ThreadTarget, quote?: Quote) {
  open = { client, target, quote };
  changed();
}

export function closeThread() {
  open = null;
  changed();
}

/** "3 new", "@ 2 new", or nothing. */
function unreadLabel(client: Client, target: ThreadTarget): string {
  const s = client.thread(target);
  if (!s?.unread) return "";
  return `${s.mention ? "@ " : ""}${s.unread} new`;
}

/** The pane menu's thread items. */
export function threadItems(client: Client, pane: PaneId): MenuItem[] {
  if (!client.hasThreads()) return [];
  const n = unreadLabel(client, { pane });
  const selected = client.panes.get(pane)?.view.selection().trim() ?? "";
  return [
    { label: n ? `Thread (${n})` : "Thread", run: () => openThread(client, { pane }) },
    ...(client.mayPost({ pane })
      ? [
          {
            label: "Quote selection in thread",
            disabled: !selected,
            run: () => openThread(client, { pane }, { pane, text: selected }),
          } as MenuItem,
        ]
      : []),
  ];
}

/** The session menu's thread item. */
export function sessionThreadItems(client: Client, session: number): MenuItem[] {
  if (!client.hasThreads()) return [];
  const n = unreadLabel(client, { session });
  return [{ label: n ? `Session thread (${n})` : "Session thread", run: () => openThread(client, { session }) }];
}

/** On a pane: its unread messages, or that it has a thread. */
export function ThreadBadge({ client, pane }: { client: Client; pane: PaneId }) {
  const s = client.thread({ pane });
  if (!s) return null;
  const n = s.unread ?? 0;
  return (
    <button
      class={`thread-badge${n ? " unread" : ""}${s.mention ? " mention" : ""}`}
      title={n ? `${n} new in this pane's thread` : "This pane's thread"}
      onPointerDown={(e) => e.stopPropagation()}
      onClick={() => openThread(client, { pane })}
    >
      <Bubble />
      {n ? <span>{s.mention ? `@ ${n}` : n}</span> : null}
    </button>
  );
}

function Bubble() {
  return (
    <svg viewBox="0 0 16 16" width="12" height="12" aria-hidden="true">
      <path
        d="M3 2.5h10a1.5 1.5 0 0 1 1.5 1.5v6a1.5 1.5 0 0 1-1.5 1.5H7l-3 2.5V11.5H3A1.5 1.5 0 0 1 1.5 10V4A1.5 1.5 0 0 1 3 2.5z"
        fill="currentColor"
      />
    </svg>
  );
}

export function ThreadLayer({ phone }: { phone: boolean }) {
  const [, setTick] = useState(0);
  useEffect(() => {
    const fn = () => setTick((t) => t + 1);
    listeners.add(fn);
    return () => {
      listeners.delete(fn);
    };
  }, []);
  if (!open) return null;
  return <ThreadPanel key={threadKey(open.target)} client={open.client} target={open.target} quote={open.quote} phone={phone} />;
}

function title(client: Client, target: ThreadTarget): string {
  if ("session" in target) {
    const name = client.state?.sessions.find((s) => s.id === target.session)?.name;
    return `Session thread · ${name ?? target.session}`;
  }
  const t = client.title(target.pane);
  return `Thread · %${target.pane}${t ? ` ${t}` : ""}`;
}

function ThreadPanel({
  client,
  target,
  quote: initialQuote,
  phone,
}: {
  client: Client;
  target: ThreadTarget;
  quote?: Quote;
  phone: boolean;
}) {
  const [msgs, setMsgs] = useState<ThreadMsg[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [text, setText] = useState("");
  const [quote, setQuote] = useState<Quote | undefined>(initialQuote);
  const [sending, setSending] = useState(false);
  const [, setTick] = useState(0);
  const list = useRef<HTMLDivElement>(null);
  const input = useRef<HTMLTextAreaElement>(null);
  const mayPost = client.mayPost(target);

  // The messages so far, then each new one as it comes.
  useEffect(() => {
    let live = true;
    const seen = new Map<number, ThreadMsg>();
    const show = () => live && setMsgs([...seen.values()].sort((a, b) => a.id - b.id));
    const off = client.onThread(target, (m) => {
      seen.set(m.id, m);
      show();
    });
    client
      .loadThread(target)
      .then((all) => {
        for (const m of all) seen.set(m.id, m);
        show();
      })
      .catch((e: Error) => live && setError(e.message));
    return () => {
      live = false;
      off();
    };
  }, [client, target]);

  // Its pane or session went away, or the layout changed (names).
  useEffect(() => client.subscribe(() => setTick((t) => t + 1)), [client]);
  const gone = "pane" in target ? !client.info(target.pane) : !client.state?.sessions.some((s) => s.id === target.session);

  // What's shown is read; the newest stays in view.
  const last = msgs?.[msgs.length - 1]?.id ?? 0;
  useLayoutEffect(() => {
    const el = list.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [last]);
  useEffect(() => {
    if (last && document.visibilityState === "visible") client.markThreadRead(target, last);
  }, [client, target, last, client.thread(target)?.unread]);

  useEffect(() => {
    if (mayPost && !phone) input.current?.focus();
  }, [mayPost, phone]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") closeThread();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const send = async () => {
    if (sending || (!text.trim() && !quote)) return;
    setSending(true);
    setError(null);
    try {
      await client.postThread(target, text, quote);
      setText("");
      setQuote(undefined);
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setSending(false);
      input.current?.focus();
    }
  };

  const reveal = (q: Quote) => {
    const entry = client.panes.get(q.pane);
    if (!entry) {
      setError(`%${q.pane} is closed; the quote is all that's left`);
      return;
    }
    client.setActive(q.pane);
    if (!entry.view.reveal(q.text)) setError(`that output has scrolled out of %${q.pane}`);
    if (phone) closeThread();
  };

  return (
    <aside class={phone ? "thread-panel phone" : "thread-panel"} aria-label={title(client, target)}>
      <header>
        <strong>{title(client, target)}</strong>
        <button class="thread-close" title="Close (Esc)" onClick={closeThread}>
          ✕
        </button>
      </header>
      <div class="thread-list" ref={list}>
        {msgs === null && !error && <p class="thread-empty">Loading…</p>}
        {msgs?.length === 0 && (
          <p class="thread-empty">
            Nothing yet. {"pane" in target ? "Talk about this pane here" : "Talk about this session here"}; write @agent to reach
            the agent in a pane, or @name to notify someone.
          </p>
        )}
        {msgs?.map((m, i) => (
          <Message key={m.id} client={client} m={m} prev={msgs[i - 1]} reveal={reveal} />
        ))}
      </div>
      {error && <p class="thread-error">{error}</p>}
      {gone ? (
        <p class="thread-note">{"pane" in target ? "This pane is closed." : "This session is closed."} Its thread stays in search.</p>
      ) : mayPost ? (
        <form
          class="thread-compose"
          onSubmit={(e) => {
            e.preventDefault();
            void send();
          }}
        >
          {quote && (
            <div class="thread-quote pending">
              <pre>{quote.text}</pre>
              <button type="button" title="Don't quote it" onClick={() => setQuote(undefined)}>
                ✕
              </button>
            </div>
          )}
          <textarea
            ref={input}
            rows={2}
            value={text}
            placeholder={"pane" in target ? "Message (@agent reaches its agent)" : "Message"}
            onInput={(e) => setText((e.target as HTMLTextAreaElement).value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
                e.preventDefault();
                void send();
              }
            }}
          />
          <button class="primary" type="submit" disabled={sending || (!text.trim() && !quote)}>
            Send
          </button>
        </form>
      ) : (
        <p class="thread-note">You're watching this session: you can read its threads, not post.</p>
      )}
    </aside>
  );
}

function when(at: number): string {
  const d = new Date(at);
  const today = new Date().toDateString() === d.toDateString();
  return today
    ? d.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" })
    : d.toLocaleString([], { month: "short", day: "numeric", hour: "numeric", minute: "2-digit" });
}

function Message({
  client,
  m,
  prev,
  reveal,
}: {
  client: Client;
  m: ThreadMsg;
  prev?: ThreadMsg;
  reveal: (q: Quote) => void;
}) {
  // Runs of one person's messages share a heading.
  const head = !prev || prev.who !== m.who || m.at - prev.at > 5 * 60_000;
  const mine = m.who === client.me();
  return (
    <div class={`thread-msg${mine ? " mine" : ""}${m.mentions?.includes(client.me()) ? " for-me" : ""}`}>
      {head && (
        <div class="thread-head">
          <span class="thread-name" style={{ color: m.agent ? "var(--accent)" : colorOf(m.who) }}>
            {m.agent ? m.name : m.name.split("@")[0]}
          </span>
          <time title={new Date(m.at).toLocaleString()}>{when(m.at)}</time>
        </div>
      )}
      {m.text && <p class="thread-text">{withMentions(m.text)}</p>}
      {m.quote && (
        <button class="thread-quote" title="Show it in the pane" onClick={() => reveal(m.quote!)}>
          <span class="thread-quote-from">%{m.quote.pane}</span>
          <pre>{m.quote.text}</pre>
        </button>
      )}
      {m.to_agent && <div class="thread-tag">sent to the pane's agent</div>}
    </div>
  );
}

/** Text with each @mention marked. */
function withMentions(text: string) {
  const parts = text.split(/(^|[^\w])(@[\w.-]+)/g);
  return parts.map((p, i) => (p.startsWith("@") && p.length > 1 ? <b key={i}>{p}</b> : p));
}
