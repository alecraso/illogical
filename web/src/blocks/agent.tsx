// Agent blocks (M6b): an agent run as messages, thoughts and tool-call
// cards, with its commands' output in a read-only terminal, permission
// requests as approve/deny cards, questions and forms as cards (M6c), and a
// composer. Works the same on a phone.

import { render } from "preact";
import { useEffect, useLayoutEffect, useRef, useState } from "preact/hooks";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import type { Client } from "../client";
import type { PaneId } from "../proto";
import { theme } from "../theme";
import { askText } from "../ui/menu";
import { registerBlock, type BlockView } from "./view";
import { AskCard, headline, type Ask, type Question } from "./ask";

export interface Tool {
  type: "tool";
  id: string;
  /** The agent's name for it (AskUserQuestion), when it says. */
  name?: string;
  title: string;
  kind: string;
  status: string;
  command?: string;
  output: string;
  exit?: number;
  text: string;
  locations: string[];
  questions?: Question[];
  started_ms: number;
  ended_ms?: number;
}

export type Entry =
  | { type: "user"; text: string; at_ms: number }
  | { type: "agent"; text: string; id?: string }
  | { type: "thought"; text: string; id?: string }
  | { type: "note"; text: string; at_ms: number }
  | Tool;

export interface Perm {
  id: string;
  tool_call_id: string;
  tool: string;
  title: string;
  kind: string;
  command?: string;
  options: { id: string; name: string; kind: string }[];
  at_ms: number;
}

export interface AgentState {
  agent: "claude" | "codex" | "fountain" | "acp";
  label: string;
  title: string | null;
  cwd: string | null;
  vm: boolean;
  session_id: string | null;
  server: { name?: string; title?: string; version?: string } | null;
  status: "starting" | "ready" | "working" | "remote" | "stopped" | "exited";
  attention: string;
  error: string | null;
  last_stop: string | null;
  current_tool: { id: string; title: string; kind: string } | null;
  pending: Perm[];
  /** Open questions and forms. */
  asks: Ask[];
  queued: string[];
  cost: { total: number; currency: string | null; last_turn: number | null } | null;
  tokens: { total: number; last_turn: Record<string, number> | null };
  turns: number;
  allow: { tool: string; title?: string }[];
  entries_from: number;
  entries: Entry[];
}

const STATUS: Record<AgentState["status"], string> = {
  starting: "Starting",
  ready: "Ready",
  working: "Working",
  remote: "Running on Fountain",
  stopped: "Stopped",
  exited: "Stopped",
};

const strip = (s: string) => s.replace(/\x1b\[[0-9;?]*[ -/]*[@-~]/g, "").replace(/\x1b\][^\x07\x1b]*(\x07|\x1b\\)/g, "");

/** Tool cards older than this many show plain text instead of a terminal. */
const LIVE_TERMINALS = 30;

function money(n: number, currency: string | null) {
  const sym = !currency || currency === "USD" ? "$" : `${currency} `;
  return `${sym}${n < 0.01 ? n.toFixed(4) : n.toFixed(2)}`;
}

/** A command's output, drawn by a read-only terminal (ANSI and all). */
function Output({ data }: { data: string }) {
  const host = useRef<HTMLDivElement>(null);
  const term = useRef<{ t: Terminal; fit: FitAddon; written: string } | null>(null);
  const lines = Math.min(Math.max(data.split("\n").length, 1), 16);
  useLayoutEffect(() => {
    const t = new Terminal({
      theme: { ...theme, background: "#11111b" },
      disableStdin: true,
      convertEol: true,
      cursorStyle: "underline",
      cursorInactiveStyle: "none",
      fontSize: 12,
      fontFamily: "ui-monospace, 'JetBrains Mono', Menlo, monospace",
      rows: lines,
      cols: 80,
      scrollback: 2000,
    });
    const fit = new FitAddon();
    t.loadAddon(fit);
    t.open(host.current!);
    term.current = { t, fit, written: "" };
    const resize = () => {
      const d = fit.proposeDimensions();
      if (d && d.cols > 0) t.resize(d.cols, t.rows);
    };
    resize();
    const ro = new ResizeObserver(resize);
    ro.observe(host.current!);
    return () => {
      ro.disconnect();
      t.dispose();
      term.current = null;
    };
  }, []);
  useEffect(() => {
    const cur = term.current;
    if (!cur) return;
    if (cur.t.rows !== lines) cur.t.resize(cur.t.cols, lines);
    if (data.startsWith(cur.written)) {
      cur.t.write(data.slice(cur.written.length));
    } else {
      cur.t.reset();
      cur.t.write(data);
    }
    cur.written = data;
  }, [data, lines]);
  return <div class="agent-output" ref={host} />;
}

function ToolCard({ t, live }: { t: Tool; live: boolean }) {
  const [open, setOpen] = useState(true);
  const icon = t.name === "AskUserQuestion" ? "?" : { execute: "$", edit: "✎", read: "◱", delete: "✕", move: "→", search: "⌕", fetch: "↓", think: "…" }[t.kind] ?? "⚙";
  const body = t.output || t.text;
  return (
    <div class={`agent-tool ${t.status}`} data-tool={t.id}>
      <button class="agent-tool-head" onClick={() => setOpen(!open)} aria-expanded={open}>
        <span class="agent-tool-icon">{icon}</span>
        <span class="agent-tool-title">{t.title || t.kind || "tool"}</span>
        <span class={`agent-tool-status ${t.status}`}>
          {t.status === "in_progress" ? "running" : t.status}
          {t.exit != null && t.exit !== 0 ? ` (exit ${t.exit})` : ""}
        </span>
      </button>
      {open && (
        <>
          {t.command && t.command !== t.title && <pre class="agent-tool-cmd">$ {t.command}</pre>}
          {t.locations.length > 0 && <div class="agent-tool-locs">{t.locations.join("  ")}</div>}
          {t.output ? (
            live ? <Output data={t.output} /> : <pre class="agent-tool-text">{strip(t.output)}</pre>
          ) : body ? (
            <pre class="agent-tool-text">{body}</pre>
          ) : null}
        </>
      )}
    </div>
  );
}

function PermCard({ client, id, p }: { client: Client; id: PaneId; p: Perm }) {
  const call = (method: string, args: unknown) => void client.api(`/api/blocks/${id}/call/${method}`, args, `couldn't ${method}`);
  const always = p.options.some((o) => o.kind === "allow_once");
  return (
    <div class="agent-perm" role="alertdialog" aria-label={`Allow ${p.title}?`}>
      <div class="agent-perm-q">
        {p.tool} wants to run
      </div>
      <pre class="agent-perm-cmd">{p.command ?? p.title}</pre>
      <div class="agent-perm-buttons">
        <button class="primary" onClick={() => call("approve", { id: p.id })}>
          Approve
        </button>
        {always && (
          <button title={`Allow ${p.title} from now on, in this block`} onClick={() => call("approve", { id: p.id, option: "always" })}>
            Always
          </button>
        )}
        <button class="danger" onClick={() => call("deny", { id: p.id })}>
          Deny
        </button>
        <button
          class="link"
          onClick={async () => {
            const reason = await askText("Deny, and say why", "", "the reason");
            if (reason !== null) call("deny", { id: p.id, reason });
          }}
        >
          Deny with reason…
        </button>
      </div>
    </div>
  );
}

function Composer({ client, id, s }: { client: Client; id: PaneId; s: AgentState }) {
  const [text, setText] = useState("");
  const area = useRef<HTMLTextAreaElement>(null);
  const send = async () => {
    const t = text.trim();
    if (!t) return;
    if (await client.api(`/api/blocks/${id}/call/send`, { text: t }, "couldn't send")) setText("");
  };
  const busy = s.status === "working" || s.status === "remote" || (s.status === "starting" && s.queued.length > 0);
  return (
    <form
      class="agent-composer"
      onSubmit={(e) => {
        e.preventDefault();
        void send();
      }}
    >
      <textarea
        ref={area}
        rows={1}
        value={text}
        placeholder={busy ? "Queue a message…" : "Message the agent…"}
        onInput={(e) => setText((e.currentTarget as HTMLTextAreaElement).value)}
        onKeyDown={(e) => {
          // Enter sends on a keyboard; on a phone, the Send button does.
          if (e.key === "Enter" && !e.shiftKey && !matchMedia("(pointer: coarse)").matches) {
            e.preventDefault();
            void send();
          }
        }}
      />
      {busy ? (
        <button type="button" class="danger" onClick={() => void client.api(`/api/blocks/${id}/call/cancel`, {}, "couldn't stop it")}>
          Stop
        </button>
      ) : null}
      <button type="submit" class="primary" disabled={!text.trim()}>
        Send
      </button>
    </form>
  );
}

function AgentBlock({ client, id, s }: { client: Client; id: PaneId; s: AgentState | null }) {
  const scroller = useRef<HTMLDivElement>(null);
  const stick = useRef(true);
  useLayoutEffect(() => {
    const el = scroller.current;
    if (el && stick.current) el.scrollTop = el.scrollHeight;
  });
  if (!s) return <div class="agent-empty">Starting…</div>;
  const tools = s.entries.filter((e) => e.type === "tool");
  const liveFrom = tools.length > LIVE_TERMINALS ? (tools[tools.length - LIVE_TERMINALS] as Tool).id : null;
  let live = liveFrom === null;
  const stopped = s.status === "stopped" || s.status === "exited";
  const open = s.asks.filter((a) => !a.accepted);
  const call = (method: string, args: unknown) => void client.api(`/api/blocks/${id}/call/${method}`, args, `couldn't ${method}`);
  return (
    <div class="agent">
      <div class="agent-bar">
        <span class={`agent-status ${s.status}`}>{s.pending.length || open.length ? "Needs you" : STATUS[s.status]}</span>
        <span class="agent-name" title={s.server?.name ? `${s.server.name} ${s.server.version ?? ""}` : undefined}>
          {s.title ?? s.label}
        </span>
        {s.vm && <span class="host-tag">VM</span>}
        <span class="agent-spacer" />
        {s.cost && (
          <span class="agent-cost" title={`${s.turns} turns, ${s.tokens.total} tokens`}>
            {money(s.cost.total, s.cost.currency)}
            {s.cost.last_turn != null && s.turns > 1 ? ` (last ${money(s.cost.last_turn, s.cost.currency)})` : ""}
          </span>
        )}
        {stopped && (
          <button onClick={() => void client.api(`/api/blocks/${id}/call/start`, {}, "couldn't start it")}>Resume</button>
        )}
      </div>
      <div
        class="agent-log"
        ref={scroller}
        onScroll={(e) => {
          const el = e.currentTarget as HTMLDivElement;
          stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
        }}
      >
        {s.entries_from > 0 && <div class="agent-note">{s.entries_from} earlier entries: `illogical capture %{id}`</div>}
        {s.entries.map((e, i) => {
          switch (e.type) {
            case "user":
              return (
                <div key={i} class="agent-user">
                  {e.text}
                </div>
              );
            case "agent":
              return (
                <div key={i} class="agent-msg">
                  {e.text}
                </div>
              );
            case "thought":
              return (
                <details key={i} class="agent-thought">
                  <summary>Thinking</summary>
                  {e.text}
                </details>
              );
            case "note":
              return (
                <div key={i} class="agent-note">
                  {e.text}
                </div>
              );
            case "tool": {
              if (e.id === liveFrom) live = true;
              return <ToolCard key={e.id} t={e} live={live} />;
            }
          }
        })}
        {s.status === "working" && !s.pending.length && !open.length && <div class="agent-working">{s.current_tool ? `Running ${s.current_tool.title}…` : "Working…"}</div>}
        {s.status === "remote" && <div class="agent-working">The turn is running on Fountain; it shows here when it ends.</div>}
        {s.queued.length > 0 && <div class="agent-note">Queued: {s.queued.join(" · ")}</div>}
      </div>
      {s.error && <div class="agent-error">{s.error}</div>}
      {s.pending.map((p) => (
        <PermCard key={p.id} client={client} id={id} p={p} />
      ))}
      {s.asks.length > 0 && (
        <div class="agent-asks">
          {s.asks.map((a) => (
            <AskCard
              key={a.id}
              ask={a}
              actions={{
                answer: (content) => call("answer", { id: a.id, content }),
                decline: () => call("decline", { id: a.id }),
                stop: a.kind === "url" ? undefined : () => call("cancel", {}),
              }}
            />
          ))}
        </div>
      )}
      <Composer client={client} id={id} s={s} />
    </div>
  );
}

/** Plain text of the transcript, roughly as `capture --text` gives it. */
function plain(s: AgentState): string {
  const out: string[] = [];
  for (const e of s.entries) {
    if (e.type === "tool") out.push(`[${e.status}] ${e.title}${e.output ? `\n${strip(e.output)}` : e.text ? `\n${e.text}` : ""}`);
    else if (e.type === "user") out.push(`> ${e.text}`);
    else out.push(e.text);
  }
  for (const p of s.pending) out.push(`(waiting for approval: ${p.title})`);
  for (const a of s.asks) if (!a.accepted) out.push(`(waiting for your answer: ${headline(a)})`);
  return out.join("\n");
}

registerBlock("agent", (client, id): BlockView => {
  const host = document.createElement("div");
  host.className = "block block-agent";
  let state: AgentState | null = null;
  const draw = () => render(<AgentBlock client={client} id={id} s={state} />, host);
  draw();
  return {
    host,
    setVisible: (v) => (host.style.display = v ? "" : "none"),
    update: (s) => {
      state = s as AgentState;
      draw();
    },
    title: () => state?.title ?? state?.label ?? "agent",
    text: () => (state ? plain(state) : ""),
    focus: () => host.querySelector<HTMLElement>(".agent-composer textarea")?.focus(),
    dispose: () => {
      render(null, host);
      host.remove();
    },
  };
});
