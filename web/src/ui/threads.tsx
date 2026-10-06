// Threads on panes and sessions (M61): the people working on something talk
// beside it. The daemon that owns the pane keeps them; this is the panel
// (a drawer on the right, the whole screen on a phone), the badge on a
// pane, and the menu items that open them.

import { Fragment } from "preact";
import { useEffect, useLayoutEffect, useRef, useState } from "preact/hooks";
import type { Client } from "../client";
import { threadKey, type Invitable, type PaneId, type ThreadMsg, type ThreadTarget, type Unreached } from "../proto";
import { colorOf } from "./people";
import type { MenuItem } from "./menu";
import { Markup, mentionToken } from "./markup";

export interface Quote {
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

export function title(client: Client, target: ThreadTarget): string {
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
  quote,
  phone,
}: {
  client: Client;
  target: ThreadTarget;
  quote?: Quote;
  phone: boolean;
}) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") closeThread();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const reveal = (q: Quote): string | null => {
    const entry = client.panes.get(q.pane);
    if (!entry) return `%${q.pane} is closed; the quote is all that's left`;
    client.setActive(q.pane);
    const shown = entry.view.reveal(q.text);
    if (phone) closeThread();
    return shown ? null : `that output has scrolled out of %${q.pane}`;
  };

  return (
    <aside class={phone ? "thread-panel phone" : "thread-panel"} aria-label={title(client, target)}>
      <header>
        <strong>{title(client, target)}</strong>
        <button class="thread-close" title="Close (Esc)" onClick={closeThread}>
          ✕
        </button>
      </header>
      <ThreadBody client={client} target={target} quote={quote} phone={phone} reveal={reveal} />
    </aside>
  );
}

/** What the chat page adds to a thread (M74): a link to a message, a way
 * to its pane, the messages as they change, and a message to show. */
export interface ThreadExtras {
  /** A link to message `id` (the chat page's `#chat=…&msg=id`). */
  link?: (id: number) => string;
  /** Show the pane or session the thread is about. */
  goTo?: () => void;
  onMessages?: (msgs: ThreadMsg[]) => void;
  /** Scroll to this message and flash it. */
  focus?: number;
  /** Where the draft is kept (a host and thread); none keeps no draft. */
  draftKey?: string;
  /** What the composer calls the thread: "Message #name". */
  name?: string;
}

/** A thread's messages and the box to write in: the drawer's insides, and
 * the chat page's (ui/chat.tsx). `reveal` shows a quote's output, or says
 * why it can't. */
export function ThreadBody({
  client,
  target,
  quote: initialQuote,
  phone,
  reveal,
  extras = {},
}: {
  client: Client;
  target: ThreadTarget;
  quote?: Quote;
  phone: boolean;
  reveal: (q: Quote) => string | null;
  extras?: ThreadExtras;
}) {
  const [msgs, setMsgs] = useState<ThreadMsg[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [quote, setQuote] = useState<Quote | undefined>(initialQuote);
  const [, setTick] = useState(0);
  const list = useRef<HTMLDivElement>(null);
  const mayPost = client.mayPost(target);
  // Where the red "New" line goes: the first message unread when the thread
  // opened, kept while it stays open (reading it doesn't move the line).
  const newFrom = useRef<number | null | undefined>(undefined);

  // The messages so far, then each new one as it comes.
  useEffect(() => {
    let live = true;
    const seen = new Map<number, ThreadMsg>();
    const show = () => {
      if (!live) return;
      const all = [...seen.values()].sort((a, b) => a.id - b.id);
      setMsgs(all);
      extras.onMessages?.(all);
    };
    const off = client.onThread(target, (m) => {
      seen.set(m.id, m);
      show();
    });
    client
      .loadThread(target)
      .then((all) => {
        for (const m of all) seen.set(m.id, m);
        if (newFrom.current === undefined) {
          const sorted = [...seen.values()].sort((a, b) => a.id - b.id);
          const unread = client.thread(target)?.unread ?? 0;
          newFrom.current = unread > 0 && unread <= sorted.length ? sorted[sorted.length - unread].id : null;
        }
        show();
      })
      .catch((e: Error) => live && setError(e.message));
    return () => {
      live = false;
      off();
    };
  }, [client, threadKey(target)]);

  // Its pane or session went away, or the layout changed (names).
  useEffect(() => client.subscribe(() => setTick((t) => t + 1)), [client]);
  const gone = "pane" in target ? !client.info(target.pane) : !client.state?.sessions.some((s) => s.id === target.session);

  // What's shown is read; the newest stays in view (or the message a link
  // names, once).
  const last = msgs?.[msgs.length - 1]?.id ?? 0;
  const focused = useRef<number | null>(null);
  useLayoutEffect(() => {
    const el = list.current;
    if (!el || !msgs) return;
    const f = extras.focus;
    if (f !== undefined && focused.current !== f) {
      const m = el.querySelector<HTMLElement>(`[data-msg="${f}"]`);
      if (m) {
        focused.current = f;
        m.scrollIntoView({ block: "center" });
        m.classList.add("flash");
        setTimeout(() => m.classList.remove("flash"), 1600);
        return;
      }
    }
    if (focused.current === null) el.scrollTop = el.scrollHeight;
  }, [last, extras.focus, !!msgs]);
  useEffect(() => {
    if (last && document.visibilityState === "visible") client.markThreadRead(target, last);
  }, [client, threadKey(target), last, client.thread(target)?.unread]);

  const isMe = meMatcher(client);
  return (
    <>
      <div class="thread-list" ref={list}>
        {msgs === null && !error && <p class="thread-empty">Loading…</p>}
        {msgs?.length === 0 && (
          <p class="thread-empty">
            Nothing yet. {"pane" in target ? "Talk about this pane here" : "Talk about this session here"}; write @agent to reach
            the agent in a pane, or @name to notify someone.
          </p>
        )}
        {msgs?.map((m, i) => {
          const prev = msgs[i - 1];
          return (
            <Fragment key={m.id}>
              {(!prev || day(prev.at) !== day(m.at)) && (
                <div class="msg-day" role="separator">
                  <span>{dayLabel(m.at)}</span>
                </div>
              )}
              {m.id === newFrom.current && (
                <div class="msg-new" role="separator" data-new-line>
                  <span>New</span>
                </div>
              )}
              <Message
                client={client}
                m={m}
                prev={prev && day(prev.at) === day(m.at) && m.id !== newFrom.current ? prev : undefined}
                isMe={isMe}
                reveal={(q) => setError(reveal(q))}
                extras={extras}
                mayPost={mayPost && !gone}
                reply={(q) => composeRef.current?.reply(q)}
              />
            </Fragment>
          );
        })}
      </div>
      {error && <p class="thread-error">{error}</p>}
      {gone ? (
        <p class="thread-note">{"pane" in target ? "This pane is closed." : "This session is closed."} Its thread stays in search.</p>
      ) : mayPost ? (
        <Compose
          client={client}
          target={target}
          phone={phone}
          quote={quote}
          setQuote={setQuote}
          setError={setError}
          people={people(client, msgs ?? [])}
          draftKey={extras.draftKey}
          placeholder={extras.name ? undefined : "pane" in target ? "Message (@agent reaches its agent)" : "Message"}
          title={extras.name ?? title(client, target)}
        />
      ) : (
        <p class="thread-note">You're watching this session: you can read its threads, not post.</p>
      )}
    </>
  );
}

// The composer's handle for "Quote in reply" (one composer is shown at a
// time: the drawer's or the chat page's).
const composeRef: { current: { reply(text: string): void } | null } = { current: null };

/** Whether an @token names the person reading. */
function meMatcher(client: Client): (token: string) => boolean {
  const me = client.me();
  const mine = client.state?.presence?.find((p) => p.client === client.clientId)?.name ?? "";
  const login = me.split(":").pop()!.split("@")[0].toLowerCase();
  const tokens = new Set([mentionToken(mine), login, mine.toLowerCase().replace(/\s+/g, "")].filter(Boolean));
  return (t) => tokens.has(t);
}

/** People to @mention: who's here and who has posted, and the pane's agent. */
function people(client: Client, msgs: ThreadMsg[]): { token: string; name: string; who: string; pic?: string }[] {
  const out = new Map<string, { token: string; name: string; who: string; pic?: string }>();
  const me = client.me();
  for (const p of client.state?.presence ?? []) if (p.who !== me) out.set(p.who, { token: mentionToken(p.name), name: p.name, who: p.who, pic: p.pic });
  for (const m of msgs) if (!m.agent && m.who !== me && !out.has(m.who)) out.set(m.who, { token: mentionToken(m.name), name: m.name, who: m.who, pic: m.pic });
  return [...out.values()].filter((p) => p.token);
}

function Compose({
  client,
  target,
  phone,
  quote,
  setQuote,
  setError,
  people: who,
  draftKey,
  placeholder,
  title: name,
}: {
  client: Client;
  target: ThreadTarget;
  phone: boolean;
  quote?: Quote;
  setQuote: (q: Quote | undefined) => void;
  setError: (e: string | null) => void;
  people: { token: string; name: string; who: string; pic?: string }[];
  draftKey?: string;
  placeholder?: string;
  title: string;
}) {
  const storeKey = draftKey ? `chat.draft:${draftKey}` : null;
  const [text, setText] = useState(() => (storeKey ? load(storeKey) : ""));
  const [sending, setSending] = useState(false);
  const [pick, setPick] = useState(0);
  const input = useRef<HTMLTextAreaElement>(null);

  // Drafts last for this tab's session (a reload keeps them).
  useEffect(() => {
    if (storeKey) save(storeKey, text);
  }, [storeKey, text]);

  useEffect(() => {
    if (!phone) input.current?.focus();
  }, [phone]);

  // Grow with the text, to 40% of the window.
  useLayoutEffect(() => {
    const el = input.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, innerHeight * 0.4)}px`;
  }, [text]);

  useEffect(() => {
    composeRef.current = {
      reply(q: string) {
        const quoted = q
          .split("\n")
          .map((l) => `> ${l}`)
          .join("\n");
        setText((t) => `${quoted}\n${t}`);
        input.current?.focus();
      },
    };
    return () => {
      composeRef.current = null;
    };
  }, []);

  // @autocomplete: the word being typed at the caret.
  const caret = input.current?.selectionStart ?? text.length;
  const typing = /(?:^|[^\w])@([\w.-]*)$/.exec(text.slice(0, caret));
  const options = typing
    ? [...who, ...("pane" in target ? [{ token: "agent", name: "The pane's agent", who: "agent" }] : [])]
        .filter((p) => p.token.startsWith(typing[1].toLowerCase()) || p.name.toLowerCase().startsWith(typing[1].toLowerCase()))
        .slice(0, 6)
    : [];
  // A whole name typed already: Enter sends, as it would without the list.
  const whole = !!typing && options.some((p) => p.token === typing[1].toLowerCase());
  const choose = (token: string) => {
    const start = caret - typing![1].length;
    setText(`${text.slice(0, start)}${token} ${text.slice(caret)}`);
    setPick(0);
    requestAnimationFrame(() => {
      const at = start + token.length + 1;
      input.current?.setSelectionRange(at, at);
      input.current?.focus();
    });
  };

  // What the message just posted said to no one.
  const [missed, setMissed] = useState<string[]>([]);
  // Whom it named who can't see the thread (#297; the owner's alone).
  const [offer, setOffer] = useState<{ msg: ThreadMsg; people: Invitable[] } | null>(null);

  const send = async () => {
    if (sending || (!text.trim() && !quote)) return;
    setSending(true);
    setError(null);
    setMissed([]);
    try {
      const { message, unreached, invitable } = await client.postThread(target, text, quote);
      setMissed(unreached.map(unreachedNote));
      setOffer(invitable.length ? { msg: message, people: invitable } : null);
      setText("");
      setQuote(undefined);
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setSending(false);
      input.current?.focus();
    }
  };

  return (
    <form
      class="thread-compose"
      onSubmit={(e) => {
        e.preventDefault();
        void send();
      }}
    >
      {missed.map((n) => (
        <p key={n} class="thread-note unreached">
          {n}
        </p>
      ))}
      {offer?.people.map((p) => (
        <InviteOffer
          key={`${offer.msg.id}-${p.who}`}
          client={client}
          target={target}
          msg={offer.msg}
          person={p}
          // Two people for one @: say which is which.
          named={offer.people.filter((o) => o.token === p.token).length > 1}
          done={() => setOffer((o) => (o ? { ...o, people: o.people.filter((x) => x.who !== p.who) } : o))}
        />
      ))}
      {options.length > 0 && (
        <ul class="mention-pick" role="listbox" aria-label="People to mention">
          {options.map((p, i) => (
            <li
              key={p.who}
              role="option"
              aria-selected={i === pick % options.length}
              class={i === pick % options.length ? "picked" : ""}
              onMouseDown={(e) => {
                e.preventDefault();
                choose(p.token);
              }}
            >
              {p.who === "agent" ? <span class="msg-avatar agent">⚙</span> : <MsgAvatar who={p.who} name={p.name} pic={p.pic} />}
              <b>{p.name}</b>
              <span>@{p.token}</span>
            </li>
          ))}
        </ul>
      )}
      <div class="compose-box">
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
          rows={1}
          value={text}
          aria-label={`Message ${name}`}
          placeholder={placeholder ?? `Message ${name}`}
          onInput={(e) => {
            setMissed([]);
            setText((e.target as HTMLTextAreaElement).value);
          }}
          onKeyDown={(e) => {
            if (options.length && (e.key === "ArrowDown" || e.key === "ArrowUp")) {
              e.preventDefault();
              setPick((p) => (p + (e.key === "ArrowDown" ? 1 : options.length - 1)) % options.length);
              return;
            }
            if (options.length && (e.key === "Tab" || (e.key === "Enter" && !e.shiftKey && !whole))) {
              e.preventDefault();
              choose(options[pick % options.length].token);
              return;
            }
            if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
              e.preventDefault();
              void send();
            }
          }}
        />
        <button class="compose-send" type="submit" title="Send (Enter)" aria-label="Send" disabled={sending || (!text.trim() && !quote)}>
          <svg viewBox="0 0 16 16" width="16" height="16" aria-hidden="true">
            <path d="M2 2.5l12 5.5-12 5.5 1.8-5.5L2 2.5zm1.8 5.5h6" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linejoin="round" />
          </svg>
        </button>
      </div>
    </form>
  );
}

/** "Sam can't see this. Invite them?" (#297): one click shares the session
 *  with them as a viewer and opens them this thread, from this message on
 *  (the default) or all of it; no other thread opens to them. */
function InviteOffer({
  client,
  target,
  msg,
  person,
  named,
  done,
}: {
  client: Client;
  target: ThreadTarget;
  msg: ThreadMsg;
  person: Invitable;
  named: boolean;
  done: () => void;
}) {
  const [whole, setWhole] = useState(false);
  const [busy, setBusy] = useState(false);
  const [said, setSaid] = useState<{ ok: boolean; text: string } | null>(null);
  const n = person.name.split("@")[0];
  // Two people by one name, or one taken for another: say whom.
  const who = named || person.merged ? `${n} (${person.who})` : n;
  const invite = async () => {
    setBusy(true);
    try {
      setSaid({ ok: true, text: await client.inviteToThread(target, person.who, msg, whole) });
    } catch (e) {
      setSaid({ ok: false, text: (e as Error).message });
    } finally {
      setBusy(false);
    }
  };
  if (said?.ok) {
    return (
      <p class="thread-note invited" data-offer={person.who}>
        {said.text}
      </p>
    );
  }
  const name = `offer-${msg.id}-${person.who}`;
  return (
    <div class="thread-offer" data-offer={person.who}>
      <p>
        <strong>{who}</strong> can't see this. Invite them?
      </p>
      <label>
        <input type="radio" name={name} checked={!whole} onChange={() => setWhole(false)} /> This message and what follows
      </label>
      <label>
        <input type="radio" name={name} checked={whole} onChange={() => setWhole(true)} /> Share the whole thread
      </label>
      <p class="thread-offer-sees">
        {whole ? `${n} will see all of this thread, and no other` : `${n} will see this message and what follows in this thread`}
      </p>
      {said && <p class="thread-error">{said.text}</p>}
      <div class="thread-offer-buttons">
        <button type="button" class="primary" disabled={busy} onClick={() => void invite()}>
          Invite {n}
        </button>
        <button type="button" onClick={done}>
          Not now
        </button>
      </div>
    </div>
  );
}

function load(key: string): string {
  try {
    return sessionStorage.getItem(key) ?? "";
  } catch {
    return "";
  }
}

function save(key: string, text: string) {
  try {
    if (text) sessionStorage.setItem(key, text);
    else sessionStorage.removeItem(key);
  } catch {
    // storage refused: the draft lasts while the thread is open
  }
}

function day(at: number): string {
  return new Date(at).toDateString();
}

/** "Today", "Yesterday", or the date, for the line between days. */
function dayLabel(at: number): string {
  const d = new Date(at);
  const today = new Date();
  const yesterday = new Date(today.getFullYear(), today.getMonth(), today.getDate() - 1);
  if (d.toDateString() === today.toDateString()) return "Today";
  if (d.toDateString() === yesterday.toDateString()) return "Yesterday";
  return d.toLocaleDateString([], {
    weekday: "long",
    month: "long",
    day: "numeric",
    ...(d.getFullYear() === today.getFullYear() ? {} : { year: "numeric" }),
  });
}

function when(at: number): string {
  return new Date(at).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
}

/** A message's picture: theirs, or their initials on their color. */
export function MsgAvatar({ who, name, pic }: { who: string; name: string; pic?: string }) {
  const initials = name
    .split(/[\s@]+/)
    .filter(Boolean)
    .slice(0, 2)
    .map((w) => w[0]!.toUpperCase())
    .join("");
  return (
    <span class="msg-avatar" style={{ background: colorOf(who) }} aria-hidden="true">
      {pic ? <img src={pic} alt="" referrerpolicy="no-referrer" /> : initials}
    </span>
  );
}

function Message({
  client,
  m,
  prev,
  isMe,
  reveal,
  extras,
  mayPost,
  reply,
}: {
  client: Client;
  m: ThreadMsg;
  /** The message before it, when it's on the same day and no line between. */
  prev?: ThreadMsg;
  isMe: (token: string) => boolean;
  reveal: (q: Quote) => void;
  extras: ThreadExtras;
  mayPost: boolean;
  reply: (text: string) => void;
}) {
  // Runs of one person's messages share a heading.
  const head = !prev || prev.who !== m.who || m.at - prev.at > 5 * 60_000;
  const mine = m.who === client.me();
  const [copied, setCopied] = useState(false);
  const name = m.agent ? m.name : m.name.split("@")[0];
  const link = extras.link?.(m.id);
  return (
    <div class={`thread-msg${head ? " first" : ""}${mine ? " mine" : ""}${m.mentions?.includes(client.me()) ? " for-me" : ""}`} data-msg={m.id}>
      <div class="msg-gutter">
        {head ? (
          m.agent ? (
            <span class="msg-avatar agent" aria-hidden="true">
              ⚙
            </span>
          ) : (
            <MsgAvatar who={m.who} name={m.name} pic={m.pic} />
          )
        ) : (
          <time class="msg-hover-time" title={new Date(m.at).toLocaleString()}>
            {when(m.at)}
          </time>
        )}
      </div>
      <div class="msg-body">
        {head && (
          <div class="thread-head">
            <span class="thread-name" style={{ color: m.agent ? "var(--accent)" : undefined }}>
              {name}
            </span>
            {m.agent && <span class="msg-badge">Agent</span>}
            <time title={new Date(m.at).toLocaleString()}>{when(m.at)}</time>
          </div>
        )}
        {m.text && <Markup text={m.text} isMe={isMe} landed={reached(m, isMe, client.me())} />}
        {m.quote && (
          <button class="thread-quote" title="Show it in the pane" onClick={() => reveal(m.quote!)}>
            <span class="thread-quote-from">
              from %{m.quote.pane}
              {client.title(m.quote.pane) ? ` · ${client.title(m.quote.pane)}` : ""}
            </span>
            <pre>{m.quote.text}</pre>
          </button>
        )}
        {m.to_agent && <div class="thread-tag">sent to the pane's agent</div>}
      </div>
      <div class="msg-tools" role="toolbar" aria-label="Message actions">
        {mayPost && (
          <button title="Quote in reply" data-msg-reply onClick={() => reply(m.text || m.quote?.text || "")}>
            ❝
          </button>
        )}
        {link && (
          <button
            title={copied ? "Copied" : "Copy link"}
            data-msg-link
            onClick={() => {
              void navigator.clipboard?.writeText(link).then(
                () => {
                  setCopied(true);
                  setTimeout(() => setCopied(false), 1200);
                },
                () => {},
              );
            }}
          >
            {copied ? "✓" : "🔗"}
          </button>
        )}
        {extras.goTo && (
          <button title="Go to the pane" data-msg-go onClick={extras.goTo}>
            ↗
          </button>
        )}
      </div>
    </div>
  );
}

/** What to tell the poster about an `@` that went nowhere. Unknown names and
 *  names without access read the same: it must not say who exists. */
function unreachedNote(u: Unreached): string {
  switch (u.why) {
    case "agent_needs_pane":
      return `@${u.token} reaches an agent from its pane's thread`;
    case "may_not_drive":
      return `@${u.token} reaches the agent only from someone who can drive the pane`;
    default:
      return `Nobody here called ${u.token} can read this thread`;
  }
}

/** The @tokens of a message that reached someone (#296): the daemon's
 *  `landed`, and any that name the reader when it reached them (`mentions`
 *  says so). A message without `landed` (from a daemon before it was kept)
 *  marks nothing else: better plain than a highlight that promises a
 *  notification nobody got. */
function reached(m: ThreadMsg, isMe: (token: string) => boolean, me: string): (token: string) => boolean {
  const landed = m.landed ?? [];
  const forMe = !!m.mentions?.includes(me);
  return (token) => landed.includes(token) || (forMe && isMe(token));
}
