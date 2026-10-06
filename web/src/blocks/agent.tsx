// Agent blocks (M6b): an agent run as messages, thoughts and tool-call
// cards, with its commands' output in a read-only terminal, permission
// requests as approve/deny cards, questions and forms as cards (M6c), and a
// composer, which takes images and files pasted, dropped or picked (M71).
// Works the same on a phone.

import { render, type ComponentChildren } from "preact";
import { useEffect, useLayoutEffect, useRef, useState } from "preact/hooks";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import type { Client } from "../client";
import type { PaneId } from "../proto";
import { theme } from "../theme";
import { AdapterHelp, installAdapter, type Adapter } from "../ui/adapter";
import { askText } from "../ui/menu";
import { answeredLine, mayAnswer } from "../ui/term-ask";
import { registerBlock, type BlockView } from "./view";
import { openFile } from "./diff";
import { AskCard, headline, type Ask, type Question } from "./ask";
import { pick, store } from "../upload";

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
  forgotten?: boolean;
}

/** #79: an opened conversation's entry off the branch Continue resumes. */
type Mark = { forgotten?: boolean };

export type Entry =
  | ({ type: "user"; text: string; images?: string[]; at_ms: number } & Mark)
  | ({ type: "agent"; text: string; id?: string } & Mark)
  | ({ type: "thought"; text: string; id?: string } & Mark)
  | ({ type: "note"; text: string; at_ms: number } & Mark)
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
  /** Its adapter isn't installed here, or has no Node (#111). */
  adapter: Adapter | null;
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
  /** M44: the Fountain agent it wears (by name), what came along, and
   *  what didn't. */
  as_fountain?: string | null;
  worn?: Worn | null;
  wearing?: boolean;
  /** A Claude Code conversation this block opened (M33). */
  import: {
    source: "terminal" | "desktop" | "other";
    path: string;
    title: string | null;
    /** Continued here: from now on it's this block's. */
    continued: boolean;
    /** Another process has it open now. */
    held: { pid: number; pane: PaneId | null; block: PaneId | null; status: string; place: string } | null;
  } | null;
  entries_from: number;
  entries: Entry[];
}

interface Worn {
  agent: string;
  model: string | null;
  plugin: string;
  skills: string[];
  skills_missing: string[];
  servers: { name: string; kind: string; vars: string[] }[];
  left_out: { name: string; why: string }[];
}

/** M44: what a worn Fountain agent brought (its skills and MCP servers),
 *  and what didn't carry over. */
function WornBar({ s }: { s: AgentState }) {
  if (!s.as_fountain) return null;
  const w = s.worn;
  if (!w) {
    return (
      <div class="agent-worn" data-worn={s.as_fountain}>
        <span class="agent-worn-as">as {s.as_fountain}</span> <span class="dim">{s.wearing ? "putting it on…" : ""}</span>
      </div>
    );
  }
  const missing = [...w.left_out.map((l) => `${l.name}: ${l.why}`), ...w.skills_missing.map((m) => `skill ${m}`)];
  return (
    <div class="agent-worn" data-worn={w.agent}>
      <span class="agent-worn-as" title={`Fountain's ${w.agent}, worn here${w.model ? ` (${w.model})` : ""}`}>as {w.agent}</span>
      <span class="ws-tags">
        {w.skills.map((k) => (
          <span key={`s-${k}`} class="ws-tag" title={`skill (${w.plugin}:${k})`} data-worn-skill={k}>
            {k}
          </span>
        ))}
        {w.servers.map((m) => (
          <span key={`m-${m.name}`} class="ws-tag fountain-mcp" title={m.vars.length ? m.vars.join("\n") : "MCP server"} data-worn-server={m.name}>
            ⚙ {m.name}
          </span>
        ))}
      </span>
      {missing.length > 0 && (
        <span class="agent-worn-missing" data-worn-missing title={missing.join("\n")}>
          didn't carry over: {[...w.left_out.map((l) => l.name), ...w.skills_missing.map((m) => m.split(" ")[0])].join(", ")}
        </span>
      )}
    </div>
  );
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

function ToolCard({ t, live, openAt }: { t: Tool; live: boolean; openAt?: (path: string) => void }) {
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
          {t.locations.length > 0 && (
            <div class="agent-tool-locs">
              {t.locations.map((l) => (
                <span key={l} class="agent-tool-loc">
                  {l}
                  {openAt && (
                    <button class="link" data-open-file={l} title={`Open ${l}`} onClick={() => openAt(l)}>
                      Open file
                    </button>
                  )}{" "}
                </span>
              ))}
            </div>
          )}
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

/** The first word of a command: what a standing rule allows by default
 * (#166). Anything else is the whole tool. */
function firstWord(p: Perm): string {
  return p.tool === "Bash" ? (p.command ?? p.title).trim().split(/\s+/)[0] ?? "" : "";
}

function PermCard({ client, id, p, cwd, owner }: { client: Client; id: PaneId; p: Perm; cwd: string | null; owner: boolean }) {
  const call = (method: string, args: unknown) => void client.api(`/api/blocks/${id}/call/${method}`, args, `couldn't ${method}`);
  const always = p.options.some((o) => o.kind === "allow_once");
  // #166: "From now on…" makes a standing rule the daemon keeps, for this
  // directory or every block (the owner's to make).
  const [standing, setStanding] = useState(false);
  const [scope, setScope] = useState<"cwd" | "everywhere">(cwd ? "cwd" : "everywhere");
  const [prefix, setPrefix] = useState(() => firstWord(p));
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
        {always && owner && !standing && (
          <button class="link" onClick={() => setStanding(true)}>
            From now on…
          </button>
        )}
      </div>
      {standing && (
        <form
          class="agent-perm-standing"
          aria-label="A standing rule"
          onSubmit={(e) => {
            e.preventDefault();
            call("approve", { id: p.id, option: "always", scope, prefix: prefix.trim() || undefined });
          }}
        >
          <label>
            Allow {p.tool}
            <input name="prefix" value={prefix} placeholder="any" onInput={(e) => setPrefix(e.currentTarget.value)} />
            <span class="hint">{prefix.trim() ? "commands starting with this" : "anything"}</span>
          </label>
          <label>
            <select name="scope" value={scope} onChange={(e) => setScope(e.currentTarget.value as "cwd" | "everywhere")}>
              {cwd && <option value="cwd">in {cwd} and below</option>}
              <option value="everywhere">in every agent block</option>
            </select>
          </label>
          <div class="agent-perm-buttons">
            <button class="primary" type="submit">
              Allow from now on
            </button>
            <button type="button" class="link" onClick={() => setStanding(false)}>
              Cancel
            </button>
          </div>
          <p class="hint">On this machine; forget it from the session menu, Permission rules…</p>
        </form>
      )}
    </div>
  );
}

/** Images the block keeps (M71), fetched once each: a URL to show. */
const kept = new Map<string, Promise<string | null>>();

function keptImage(client: Client, id: PaneId, name: string): Promise<string | null> {
  const key = `${id}/${name}`;
  let p = kept.get(key);
  if (!p) {
    p = client
      .request("POST", `/api/blocks/${id}/call/image`, { name })
      .then(async (res) => {
        if (!res.ok) return null;
        const { mime, data } = await res.json<{ mime: string; data: string }>();
        const bytes = Uint8Array.from(atob(data), (c) => c.charCodeAt(0));
        return URL.createObjectURL(new Blob([bytes], { type: mime }));
      })
      .catch(() => null);
    kept.set(key, p);
  }
  return p;
}

function KeptImage({ client, id, name }: { client: Client; id: PaneId; name: string }) {
  const [url, setUrl] = useState<string | null>(null);
  useEffect(() => {
    let live = true;
    void keptImage(client, id, name).then((u) => live && setUrl(u));
    return () => {
      live = false;
    };
  }, [id, name]);
  if (!url) return <span class="agent-image missing">image</span>;
  return (
    <a href={url} target="_blank" rel="noopener" class="agent-image">
      <img src={url} alt="image" />
    </a>
  );
}

/** A file waiting in the composer, with a preview if it's an image. */
function Attached({ file, remove }: { file: File; remove: () => void }) {
  const [url, setUrl] = useState<string | null>(null);
  useEffect(() => {
    if (!file.type.startsWith("image/")) return;
    const u = URL.createObjectURL(file);
    setUrl(u);
    return () => URL.revokeObjectURL(u);
  }, [file]);
  return (
    <span class="agent-attached-file" title={file.name}>
      {url ? <img src={url} alt={file.name} /> : <span class="agent-attached-name">{file.name || "file"}</span>}
      <button type="button" aria-label={`Remove ${file.name || "file"}`} onClick={remove}>
        ×
      </button>
    </span>
  );
}

/** Files in a paste or a drop. */
function filesOf(data: DataTransfer | null): File[] {
  return data ? [...data.files] : [];
}

function Composer({
  client,
  id,
  s,
  attached,
  setAttached,
}: {
  client: Client;
  id: PaneId;
  s: AgentState;
  attached: File[];
  setAttached: (f: File[]) => void;
}) {
  const [text, setText] = useState("");
  const [sending, setSending] = useState<string | null>(null);
  const area = useRef<HTMLTextAreaElement>(null);
  const send = async () => {
    const t = text.trim();
    if ((!t && !attached.length) || sending) return;
    let files: string[] = [];
    if (attached.length) {
      try {
        files = await store((m, p, b) => client.request(m, p, b), id, attached, setSending);
      } catch (e) {
        setSending(null);
        client.toast((e as Error).message);
        return;
      }
    }
    setSending(null);
    if (await client.api(`/api/blocks/${id}/call/send`, { text: t, files }, "couldn't send")) {
      setText("");
      setAttached([]);
    }
  };
  const busy = s.status === "working" || s.status === "remote" || (s.status === "starting" && s.queued.length > 0);
  const opened = s.import && !s.import.continued;
  return (
    <form
      class="agent-composer"
      onSubmit={(e) => {
        e.preventDefault();
        void send();
      }}
    >
      {(attached.length > 0 || sending) && (
        <div class="agent-attached">
          {attached.map((f, i) => (
            <Attached key={i} file={f} remove={() => setAttached(attached.filter((_, j) => j !== i))} />
          ))}
          {sending && <span class="agent-attached-note">{sending}</span>}
        </div>
      )}
      <button
        type="button"
        class="attach"
        aria-label="Attach files"
        title="Attach images or files"
        onClick={() => void pick().then((f) => f.length && setAttached([...attached, ...f]))}
      >
        📎
      </button>
      <textarea
        ref={area}
        rows={1}
        value={text}
        onPaste={(e) => {
          // A screenshot or a copied file: attached, not pasted as text.
          const files = filesOf(e.clipboardData);
          if (!files.length) return;
          e.preventDefault();
          setAttached([...attached, ...files]);
        }}
        placeholder={busy ? "Queue a message…" : opened ? (s.import?.held ? "Fork it to go on here" : "Continue the conversation…") : "Message the agent…"}
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
      <button type="submit" class="primary" disabled={(!text.trim() && !attached.length) || !!sending}>
        Send
      </button>
    </form>
  );
}

function AgentBlock({ client, id, s }: { client: Client; id: PaneId; s: AgentState | null }) {
  const scroller = useRef<HTMLDivElement>(null);
  const stick = useRef(true);
  // Files for the next prompt (M71), dropped anywhere on the block.
  const [attached, setAttached] = useState<File[]>([]);
  useLayoutEffect(() => {
    const el = scroller.current;
    if (el && stick.current) el.scrollTop = el.scrollHeight;
  });
  if (!s) return <div class="agent-empty">Starting…</div>;
  const tools = s.entries.filter((e) => e.type === "tool");
  const liveFrom = tools.length > LIVE_TERMINALS ? (tools[tools.length - LIVE_TERMINALS] as Tool).id : null;
  let live = liveFrom === null;
  const stopped = s.status === "stopped" || s.status === "exited";
  // M33: a conversation opened here, not continued yet.
  const opened = !!s.import && !s.import.continued;
  const open = s.asks.filter((a) => !a.accepted);
  const call = (method: string, args: unknown) => void client.api(`/api/blocks/${id}/call/${method}`, args, `couldn't ${method}`);
  // M29: who answered last, and whether this person may answer at all.
  const answered = client.info(id)?.answered ?? null;
  const can = mayAnswer(client, id);
  return (
    <div
      class="agent"
      onDragOver={(e) => {
        if (e.dataTransfer?.types.includes("Files")) e.preventDefault();
      }}
      onDrop={(e) => {
        const files = filesOf(e.dataTransfer);
        if (!files.length) return;
        e.preventDefault();
        setAttached([...attached, ...files]);
      }}
    >
      <div class="agent-bar">
        <span class={`agent-status ${s.status}`}>
          {s.pending.length || open.length ? "Needs you" : s.import && !s.import.continued ? (s.import.held ? "Open elsewhere" : "Conversation") : STATUS[s.status]}
        </span>
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
        {stopped && !opened && (
          <button onClick={() => void client.api(`/api/blocks/${id}/call/start`, {}, "couldn't start it")}>Resume</button>
        )}
        {opened && s.import?.held?.pane != null && client.info(s.import.held.pane) && (
          <button onClick={() => client.focusPane(s.import!.held!.pane!)}>Go to pane %{s.import.held.pane}</button>
        )}
        {opened && (
          <button
            data-continue
            disabled={!!s.import?.held}
            title={s.import?.held ? `It's ${s.import.held.place}: fork it, or continue once that's closed` : "Go on with it here"}
            onClick={() => void client.api(`/api/blocks/${id}/call/continue`, {}, "couldn't continue it")}
          >
            Continue
          </button>
        )}
        {(opened || (s.import && stopped)) && (
          <button data-fork title="A new session with its history; the original is left alone" onClick={() => void client.api(`/api/blocks/${id}/call/fork`, {}, "couldn't fork it")}>
            Fork
          </button>
        )}
      </div>
      <WornBar s={s} />
      {opened && (
        <div class="agent-import">
          A Claude Code conversation from {s.import!.source === "desktop" ? "the desktop app" : s.import!.source === "terminal" ? "a terminal" : "elsewhere"}
          {s.import!.held ? `, ${s.import!.held.place}: this follows it as it goes. Fork it to go on here.` : ". Continue it to go on here."}
        </div>
      )}
      <div
        class="agent-log"
        ref={scroller}
        onScroll={(e) => {
          const el = e.currentTarget as HTMLDivElement;
          stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
        }}
      >
        {s.entries_from > 0 && <div class="agent-note">{s.entries_from} earlier entries: `illogical capture %{id}`</div>}
        {rows(s.entries, (e, i) => {
          switch (e.type) {
            case "user":
              return (
                <div key={i} class="agent-user">
                  {e.text}
                  {e.images?.length ? (
                    <div class="agent-images">
                      {e.images.map((name) => (
                        <KeptImage key={name} client={client} id={id} name={name} />
                      ))}
                    </div>
                  ) : null}
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
              return <ToolCard key={e.id} t={e} live={live} openAt={client.state?.roles ? undefined : (path) => void openFile(client, id, path, null)} />;
            }
          }
        })}
        {s.status === "working" && !s.pending.length && !open.length && <div class="agent-working">{s.current_tool ? `Running ${s.current_tool.title}…` : "Working…"}</div>}
        {s.status === "remote" && <div class="agent-working">The turn is running on Fountain; it shows here when it ends.</div>}
        {s.queued.length > 0 && <div class="agent-note">Queued: {s.queued.join(" · ")}</div>}
      </div>
      {s.error && <div class="agent-error">{s.error}</div>}
      {s.adapter && stopped && (
        <AdapterHelp client={client} a={s.adapter} said then="Resume once it's done." install={() => void installAdapter(client, s.adapter!.kind, { split: id })} />
      )}
      {answered && !s.pending.length && !open.length && <div class="agent-answered">{answeredLine(answered)}</div>}
      {s.pending.map((p) =>
        can ? (
          <PermCard key={p.id} client={client} id={id} p={p} cwd={s.cwd} owner={!client.state?.roles} />
        ) : (
          <div key={p.id} class="agent-perm" role="alertdialog" aria-label={`Allow ${p.title}?`}>
            <div class="agent-perm-q">{p.tool} wants to run</div>
            <pre class="agent-perm-cmd">{p.command ?? p.title}</pre>
            <p class="ask-viewer">You're watching this session: an editor answers it.</p>
          </div>
        ),
      )}
      {s.asks.length > 0 && can && (
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
      <Composer client={client} id={id} s={s} attached={attached} setAttached={setAttached} />
    </div>
  );
}

/**
 * The entries, with each run the agent won't remember after Continue (#79)
 * folded under the note the daemon puts before it.
 */
function rows(entries: Entry[], show: (e: Entry, i: number) => ComponentChildren): ComponentChildren[] {
  const out: ComponentChildren[] = [];
  for (let i = 0; i < entries.length; i++) {
    const e = entries[i];
    const next = entries[i + 1];
    if (!e.forgotten && !(e.type === "note" && next?.forgotten)) {
      out.push(show(e, i));
      continue;
    }
    const label = e.forgotten ? "Not in what it remembers" : e.text;
    const run: ComponentChildren[] = [];
    let j = e.forgotten ? i : i + 1;
    for (; j < entries.length && entries[j].forgotten; j++) run.push(show(entries[j], j));
    out.push(
      <details key={`forgotten-${i}`} class="agent-forgotten">
        <summary>
          {label} <span class="agent-forgotten-n">({run.length === 1 ? "1 entry" : `${run.length} entries`})</span>
        </summary>
        {run}
      </details>,
    );
    i = j - 1;
  }
  return out;
}

/** Plain text of the transcript, roughly as `capture --text` gives it. */
function plain(s: AgentState): string {
  const out: string[] = [];
  for (const e of s.entries) {
    if (e.type === "tool") out.push(`[${e.status}] ${e.title}${e.output ? `\n${strip(e.output)}` : e.text ? `\n${e.text}` : ""}`);
    else if (e.type === "user") out.push(`> ${e.text}${(e.images ?? []).map((n) => ` [image ${n}]`).join("")}`);
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
