// Fountain blocks (M43): the person's Fountain agents as a catalog, read by
// the daemon with their own `fountain` login. A card per agent (what it
// runs, its skills and MCP servers, its environment and sandbox, where it
// comes from), in a grid (a list on the phone), with the filter bar on top:
// a search box and chips for source, runtime and sandbox provider. The
// filters are the block's (kept in its config), so every client and the
// phone see the same list. Each card: *Run on Fountain* (an agent block
// beside it), *Run here* (M44, the owner's: a Claude Code on this host
// wearing the agent, in a folder asked for; shown for claude agents, greyed
// with the reason for the ones written for Fountain only) and *Spec* (the
// agent-specs file, else Fountain's page). Everything drawn
// comes from the daemon's state; viewers get the list without the buttons.
//
// `view: runner` (M45b): this host as the account's Fountain runner. Its
// status (online, version against the installed CLI, last seen, how many
// sandboxes), the other runners, what wants you (the rail's line), and its
// sandboxes with their conversations: *Follow* (an agent block on the
// conversation), *Changes* (a diff per git checkout, read as `fountain`)
// and *Shell* (a terminal as `fountain` in the sandbox). Those three are
// the owner's: an editor sees the list.

import { render } from "preact";
import { useEffect, useRef, useState } from "preact/hooks";
import type { Client } from "../client";
import type { PaneId } from "../proto";
import { askText } from "../ui/menu";
import { registerBlock, type BlockView } from "./view";

type Source = "agent-specs" | "hand" | "app";
interface Card {
  id: string; name: string; description: string; runtime: string; model: string; skills: string[]; mcp: string[];
  environment: string | null; provider: string; mode: string | null; conversations: number; updated_at: string | null;
  source: Source | null; app: string | null; local: boolean; local_why: string | null;
}
interface Filter { query?: string; sources?: Source[]; runtimes?: string[]; providers?: string[] }
interface RunnerRow {
  id: string; name: string; online: boolean; version: string | null; os: string | null; arch: string | null;
  root: string | null; last_seen_at: string | null; last_seen_ms: number | null;
}
interface Conversation { id: string; status: string; mid_turn: boolean; runtime: string | null; title: string | null; inserted_at: string | null }
interface SandboxRow {
  id: string; name: string; status: string; parked: boolean; path: string | null; agent_id: string | null; agent: string | null;
  mode: string | null; inserted_at: string | null; conversations: Conversation[];
}
interface RunnerView {
  unit: { name: string; root: string | null; bin: string | null } | null; unit_active: boolean | null; local_version: string | null;
  this: RunnerRow | null; others: RunnerRow[]; sandboxes: SandboxRow[]; sandboxes_error: string | null; attention: string | null;
  offline_since_ms: number | null; note: string;
}
export interface FountainState {
  view: "catalog" | "runner";
  runner?: RunnerView; profile: string | null; profiles: string[]; base_url: string | null; key_from: "env" | "file" | null;
  loading: boolean; error: string | null; agents: Card[]; total: number; unreadable?: number; unreadable_note?: string | null; filter: Filter;
  counts: { source: Record<string, number>; runtime: Record<string, number>; provider: Record<string, number> };
  specs: string | null; specs_why: string | null; here?: string | null; updated_ms: number; polls: number; watching?: boolean; said: string | null;
}

/** "Fountain agents…" (or, M45b, "Fountain runner…"): the block beside
 * `split`, or in a new tab of `session`. */
export async function openFountain(client: Client, where: { split?: PaneId; session?: number }, view: "catalog" | "runner" = "catalog") {
  const place = where.split !== undefined ? { split: where.split, from_pane: where.split } : { session: where.session !== undefined ? String(where.session) : undefined };
  const failure = view === "runner" ? "couldn't open the Fountain runner" : "couldn't open the Fountain catalog";
  await client.openBlock({ type: "fountain", config: { view }, local: true, ...place }, failure);
}

const SOURCES: { key: Source; label: string }[] = [
  { key: "agent-specs", label: "agent-specs" },
  { key: "hand", label: "hand-made" },
  { key: "app", label: "app-made" },
];

function ago(iso: string | null): string {
  if (!iso) return "";
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return "";
  const s = Math.max(0, (Date.now() - t) / 1000);
  if (s < 3600) return `${Math.max(1, Math.round(s / 60))}m ago`;
  if (s < 86400) return `${Math.round(s / 3600)}h ago`;
  return `${Math.round(s / 86400)}d ago`;
}

function hostOf(u: string | null): string {
  if (!u) return "";
  try {
    return new URL(u).host;
  } catch {
    return u;
  }
}

function sourceLabel(c: Card): string {
  if (c.source === "app") return c.app ? `app: ${c.app}` : "app-made";
  if (c.source === "hand") return "hand-made";
  return c.source ?? "";
}

function seenAgo(ms: number | null): string {
  if (ms == null) return "";
  const s = Math.max(0, (Date.now() - ms) / 1000);
  if (s < 60) return `${Math.round(s)}s ago`;
  if (s < 3600) return `${Math.round(s / 60)}m ago`;
  if (s < 86400) return `${Math.round(s / 3600)}h ago`;
  return `${Math.round(s / 86400)}d ago`;
}

function versionLine(runner: string | null, local: string | null): string {
  if (runner && local) return runner === local ? `${runner} (as installed)` : `${runner} (installed fountain is ${local})`;
  if (runner) return runner;
  return local ? `version unknown (installed fountain is ${local})` : "version unknown";
}

function plainRunner(s: FountainState): string {
  const r = s.runner;
  const lines = [`Fountain runner${s.base_url ? ` on ${s.base_url}` : ""}`];
  if (s.error) lines.push(s.error);
  if (!r) return lines.join("\n");
  if (r.attention) lines.push(`! ${r.attention}`);
  if (r.this) lines.push(`${r.this.name}: ${r.this.online ? "online" : "offline"}, ${versionLine(r.this.version, r.local_version)}, ${r.sandboxes.length} sandboxes`);
  for (const o of r.others) lines.push(`${o.name}: ${o.online ? "online" : "offline"}`);
  for (const b of r.sandboxes) lines.push(`${b.name} ${b.parked ? "parked" : b.status} ${b.agent ?? ""} ${b.path ?? ""}`);
  return lines.join("\n");
}

function plain(s: FountainState | null): string {
  if (!s) return "";
  if (s.view === "runner") return plainRunner(s);
  const lines = [`Fountain agents${s.base_url ? ` on ${s.base_url}` : ""}`];
  if (s.error) lines.push(s.error);
  lines.push(`${s.agents.length} of ${s.total}`);
  for (const c of s.agents) lines.push(`${c.name} [${c.runtime}] ${sourceLabel(c)}${c.skills.length ? ` skills: ${c.skills.join(", ")}` : ""}`);
  return lines.join("\n");
}

function Chips({ kind, counts, chosen, labels, toggle, disabled }: {
  kind: string; counts: Record<string, number>; chosen: string[]; labels?: Record<string, string>; toggle: (k: string) => void; disabled: boolean;
}) {
  const keys = Object.keys(counts);
  if (keys.length < 2 && chosen.length === 0) return null;
  return (
    <span class="fountain-chips" data-chips={kind}>
      {keys.map((k) => (
        <button
          key={k}
          class={`fountain-chip${chosen.includes(k) ? " on" : ""}`}
          data-chip={k}
          aria-pressed={chosen.includes(k)}
          disabled={disabled}
          onClick={() => toggle(k)}
        >
          {labels?.[k] ?? k} <span class="dim">{counts[k]}</span>
        </button>
      ))}
    </span>
  );
}

function RunnerBlock({ client, id, s }: { client: Client; id: PaneId; s: FountainState }) {
  const [busy, setBusy] = useState<string | null>(null);
  const session = client.sessionOfTab(client.tabOfPane(id)?.id ?? -1) ?? null;
  const role = client.role(session);
  const mayAct = role !== "viewer";
  const mayOwn = role === "owner";
  const call = async (method: string, args: unknown, failure: string, key = method) => {
    setBusy(key);
    const ok = await client.api(`/api/blocks/${id}/call/${method}`, args, failure);
    setBusy(null);
    return ok;
  };
  const r = s.runner;
  const t = r?.this ?? null;
  return (
    <div class="review ws fountain fountain-runner" data-fountain-block={id} data-fountain-view="runner">
      <div class="review-bar">
        <span class="review-path" title={s.base_url ?? ""}>
          <b>Fountain runner</b> {t ? t.name : r?.unit?.name ?? ""} {hostOf(s.base_url)}
        </span>
        {t && (
          <span class={`ws-tag fountain-runner-state ${t.online ? "on" : "off"}`} data-runner-online={String(t.online)}>
            {t.online ? "online" : "offline"}
          </span>
        )}
        {mayOwn && (
          <button title="The agent catalog" data-fountain-view-catalog disabled={busy !== null} onClick={() => void call("view", { view: "catalog" }, "couldn't show the catalog")}>
            Agents
          </button>
        )}
        {mayAct && (
          <button title="Read it again" data-fountain-refresh disabled={busy !== null} onClick={() => void call("refresh", {}, "couldn't read the runner")}>
            {busy === "refresh" ? "…" : "↻"}
          </button>
        )}
        <span class={`review-live ${s.watching ? "on" : ""}`}>{s.watching ? "live" : "paused"}</span>
      </div>
      {s.error && (
        <div class="browser-card" data-fountain-error>
          <p>Can't read your Fountain runners</p>
          <p class="dim">{s.error}</p>
        </div>
      )}
      {r?.attention && (
        <p class="fountain-said fountain-attention" data-runner-attention>
          {r.attention}
        </p>
      )}
      {s.said && <p class="dim fountain-said">{s.said}</p>}
      <div class="review-body">
        {!r && !s.error && <p class="dim fountain-empty">Reading the runners…</p>}
        {r && (
          <div class="fountain-card fountain-this" data-runner-this={t?.name ?? ""}>
            {r.unit ? (
              <>
                <div class="fountain-card-head">
                  <b class="fountain-name">This host: {r.unit.name}</b>
                  <span class="ws-tag" data-unit-active={String(r.unit_active)}>
                    unit {r.unit_active === true ? "active" : r.unit_active === false ? "not active" : "unknown"}
                  </span>
                </div>
                {t ? (
                  <div class="dim fountain-meta">
                    {t.online ? "online" : "offline"} · {versionLine(t.version, r.local_version)}
                    {t.last_seen_ms != null && ` · last seen ${seenAgo(t.last_seen_ms)}`}
                    {` · ${r.sandboxes.length} sandbox${r.sandboxes.length === 1 ? "" : "es"}`}
                    {t.root && ` · ${t.root}`}
                  </div>
                ) : (
                  <div class="dim fountain-meta">Fountain doesn't list a runner named {r.unit.name}</div>
                )}
              </>
            ) : (
              <div class="dim fountain-meta">This host runs no fountain-runner unit (`arugula fountain runner install` makes it one).</div>
            )}
          </div>
        )}
        {r && r.others.length > 0 && (
          <div class="fountain-others">
            <b>Other runners</b>
            <ul>
              {r.others.map((o) => (
                <li key={o.id} data-runner-other={o.name} data-online={String(o.online)}>
                  {o.name} <span class="dim">{o.online ? "online" : "offline"}{o.version ? ` · ${o.version}` : ""}{o.last_seen_ms != null ? ` · last seen ${seenAgo(o.last_seen_ms)}` : ""}</span>
                </li>
              ))}
            </ul>
          </div>
        )}
        {r?.sandboxes_error && <p class="fountain-said">Sandboxes: {r.sandboxes_error}</p>}
        {r && t && (
          <>
            <p class="dim fountain-said" data-runner-note>{r.note}</p>
            {r.sandboxes.length === 0 && <p class="dim fountain-empty">No sandboxes on {t.name}</p>}
            <ul class="fountain-cards fountain-sandboxes">
              {r.sandboxes.map((b) => (
                <li key={b.id} class="fountain-card" data-sandbox={b.name} data-parked={String(b.parked)}>
                  <div class="fountain-card-head">
                    <b class="fountain-name" title={b.id}>{b.agent ?? "agent?"}</b>
                    <span class="ws-tag">{b.parked ? "parked" : b.status}</span>
                  </div>
                  <div class="dim fountain-meta" title={b.path ?? ""}>
                    {b.path ?? "no directory"}
                    {b.inserted_at && ` · ${ago(b.inserted_at)}`}
                  </div>
                  <ul class="fountain-convs">
                    {b.conversations.map((c) => (
                      <li key={c.id} data-conversation={c.id}>
                        <span>{c.title ?? c.id.slice(0, 8)}</span> <span class="dim">{c.status}{c.mid_turn ? " · mid-turn" : ""}</span>
                        {mayOwn && (
                          <button data-follow={c.id} disabled={busy !== null || !b.agent} title={b.agent ? "An agent block on this conversation (fountain acp, session/load)" : "Fountain didn't say its agent"} onClick={() => void call("follow", { conversation: c.id }, "couldn't follow it", `follow:${c.id}`)}>
                            Follow
                          </button>
                        )}
                      </li>
                    ))}
                  </ul>
                  {mayOwn && (
                    <div class="ws-actions fountain-actions">
                      <button data-changes={b.name} disabled={busy !== null || !b.path} title="A diff per git checkout in it, read as fountain" onClick={() => void call("changes", { sandbox: b.id }, "couldn't show its changes", `changes:${b.id}`)}>
                        Changes
                      </button>
                      <button data-shell={b.name} disabled={busy !== null || !b.path} title={`A shell as fountain in ${b.path ?? "it"}. ${r.note}`} onClick={() => void call("shell", { sandbox: b.id }, "couldn't open a shell there", `shell:${b.id}`)}>
                        Shell
                      </button>
                    </div>
                  )}
                </li>
              ))}
            </ul>
          </>
        )}
      </div>
    </div>
  );
}

function FountainBlock({ client, id, s }: { client: Client; id: PaneId; s: FountainState | null }) {
  if (s?.view === "runner") return <RunnerBlock client={client} id={id} s={s} />;
  return <CatalogBlock client={client} id={id} s={s} />;
}

function CatalogBlock({ client, id, s }: { client: Client; id: PaneId; s: FountainState | null }) {
  const [busy, setBusy] = useState<string | null>(null);
  const [query, setQuery] = useState(s?.filter.query ?? "");
  const typing = useRef<number | null>(null);
  const session = client.sessionOfTab(client.tabOfPane(id)?.id ?? -1) ?? null;
  const role = client.role(session);
  const mayAct = role !== "viewer";
  const mayOwn = role === "owner";
  // Someone else's filter (another client, the phone) shows here too,
  // unless this one is typing.
  useEffect(() => {
    if (typing.current === null) setQuery(s?.filter.query ?? "");
  }, [s?.filter.query]);

  const call = async (method: string, args: unknown, failure: string) => {
    setBusy(method);
    const ok = await client.api(`/api/blocks/${id}/call/${method}`, args, failure);
    setBusy(null);
    return ok;
  };
  if (!s || (s.loading && !s.updated_ms)) {
    return (
      <div class="review ws fountain" data-fountain-block={id}>
        <div class="browser-card dim">Reading your Fountain agents…</div>
      </div>
    );
  }
  const f = s.filter;
  const filter = (args: Record<string, unknown>) => void call("filter", args, "couldn't filter");
  const onQuery = (v: string) => {
    setQuery(v);
    if (typing.current !== null) clearTimeout(typing.current);
    typing.current = window.setTimeout(() => {
      typing.current = null;
      filter({ query: v });
    }, 200);
  };
  const toggle = (key: "sources" | "runtimes" | "providers", arg: string) => (k: string) => {
    const now = (f[key] ?? []) as string[];
    filter({ [arg]: now.includes(k) ? now.filter((x) => x !== k) : [...now, k] });
  };
  const runHere = async (name: string) => {
    const cwd = await askText(`Run ${name} here, in`, s.here ?? "~", "a folder on this machine (a worktree)");
    if (cwd?.trim()) await call("run_here", { agent: name, cwd: cwd.trim() }, `couldn't run ${name} here`);
  };
  const pickSpecs = async () => {
    const dir = await askText("Your agent-specs checkout", s.specs ?? "", "~/dev/…/agent-specs");
    if (dir?.trim()) await call("specs", { dir: dir.trim() }, "couldn't use that checkout");
  };
  const filtered = !!(f.query?.trim() || f.sources?.length || f.runtimes?.length || f.providers?.length);
  const sourceCounts = Object.fromEntries(SOURCES.filter((x) => s.counts.source[x.key]).map((x) => [x.key, s.counts.source[x.key]]));
  return (
    <div class="review ws fountain" data-fountain-block={id}>
      <div class="review-bar">
        <span class="review-path" title={s.base_url ?? ""}>
          <b>Fountain agents</b> {hostOf(s.base_url)}
          {s.profile && s.profile !== "default" ? ` (${s.profile})` : ""}
        </span>
        <span class="dim" data-fountain-count>
          {filtered ? `${s.agents.length} of ${s.total}` : `${s.total}`}
        </span>
        {mayOwn && s.profiles.length > 1 && (
          <select
            class="fountain-profile"
            title="Credentials profile"
            value={s.profile ?? "default"}
            disabled={busy !== null}
            onChange={(e) => void call("profile", { name: e.currentTarget.value }, "couldn't use that profile")}
          >
            {s.profiles.map((p) => (
              <option key={p} value={p}>
                {p}
              </option>
            ))}
          </select>
        )}
        {mayAct && (
          <button title="Read them again" data-fountain-refresh disabled={busy !== null} onClick={() => void call("refresh", {}, "couldn't read the agents")}>
            {busy === "refresh" ? "…" : "↻"}
          </button>
        )}
        <span class={`review-live ${s.watching ? "on" : ""}`}>{s.watching ? "live" : "paused"}</span>
      </div>
      <div class="fountain-filters">
        <input
          class="fountain-search"
          type="search"
          placeholder="Search names, descriptions, skills, MCP servers"
          value={query}
          disabled={!mayAct}
          autocomplete="off"
          spellcheck={false}
          onInput={(e) => onQuery(e.currentTarget.value)}
        />
        <Chips kind="source" counts={sourceCounts} chosen={f.sources ?? []} labels={Object.fromEntries(SOURCES.map((x) => [x.key, x.label]))} toggle={toggle("sources", "source")} disabled={!mayAct} />
        <Chips kind="runtime" counts={s.counts.runtime} chosen={f.runtimes ?? []} toggle={toggle("runtimes", "runtime")} disabled={!mayAct} />
        <Chips kind="provider" counts={s.counts.provider} chosen={f.providers ?? []} toggle={toggle("providers", "provider")} disabled={!mayAct} />
        {filtered && mayAct && (
          <button class="fountain-clear" data-fountain-clear onClick={() => { setQuery(""); filter({ clear: true }); }}>
            Clear
          </button>
        )}
      </div>
      {s.error && (
        <div class="browser-card" data-fountain-error>
          <p>Can't read your Fountain agents</p>
          <p class="dim">{s.error}</p>
          {mayAct && <button onClick={() => void call("refresh", {}, "couldn't read the agents")}>Try again</button>}
        </div>
      )}
      {s.unreadable_note && (
        <p class="fountain-said" data-fountain-unreadable>
          {s.unreadable_note}
        </p>
      )}
      {s.said && <p class="dim fountain-said">{s.said}</p>}
      {mayOwn && !s.specs && s.specs_why && (
        <p class="dim fountain-said" data-fountain-specs-why>
          {s.specs_why} <button class="fountain-link" onClick={() => void pickSpecs()}>Pick it…</button>
        </p>
      )}
      <div class="review-body">
        {s.agents.length === 0 && !s.error && <p class="dim fountain-empty">{filtered ? "Nothing matches" : "No agents on this account"}</p>}
        <ul class="fountain-cards">
          {s.agents.map((c) => (
            <li key={c.id} class="fountain-card" data-agent={c.name} data-source={c.source ?? ""}>
              <div class="fountain-card-head">
                <b class="fountain-name" title={c.id}>{c.name}</b>
                <span class={`ws-tag fountain-src ${c.source ?? ""}`}>{sourceLabel(c)}</span>
              </div>
              <div class="dim fountain-meta">
                {c.runtime}
                {c.model && ` · ${c.model.replace(/^anthropic\//, "")}`}
                {c.environment && ` · env ${c.environment}`}
                {` · ${c.provider}${c.mode ? ` ${c.mode}` : ""}`}
                {` · ${c.conversations} conversation${c.conversations === 1 ? "" : "s"}`}
                {c.updated_at && ` · ${ago(c.updated_at)}`}
              </div>
              {c.description && <p class="fountain-desc">{c.description}</p>}
              {(c.skills.length > 0 || c.mcp.length > 0) && (
                <div class="ws-tags fountain-tags">
                  {c.skills.map((k) => (
                    <span key={`s-${k}`} class="ws-tag" title="skill">
                      {k}
                    </span>
                  ))}
                  {c.mcp.map((m) => (
                    <span key={`m-${m}`} class="ws-tag fountain-mcp" title="MCP server">
                      ⚙ {m}
                    </span>
                  ))}
                </div>
              )}
              {mayAct && (
                <div class="ws-actions fountain-actions">
                  <button class="pri" data-run={c.name} disabled={busy !== null} onClick={() => void call("run", { agent: c.name }, `couldn't run ${c.name}`)}>
                    Run on Fountain
                  </button>
                  {c.runtime === "claude" && (
                    <button
                      data-run-here={c.name}
                      disabled={busy !== null || !c.local || !mayOwn}
                      title={c.local_why ?? (mayOwn ? "A Claude Code on this machine wearing this agent: its prompt, skills and MCP servers" : "Only the owner runs agents here")}
                      onClick={() => void runHere(c.name)}
                    >
                      Run here
                    </button>
                  )}
                  <button data-spec={c.name} disabled={busy !== null} onClick={() => void call("spec", { agent: c.name }, `couldn't open ${c.name}'s spec`)}>
                    Spec
                  </button>
                </div>
              )}
            </li>
          ))}
        </ul>
      </div>
    </div>
  );
}

registerBlock("fountain", (client, id): BlockView => {
  const host = document.createElement("div");
  host.className = "block block-fountain";
  let state: FountainState | null = null;
  const draw = () => render(<FountainBlock client={client} id={id} s={state} />, host);
  draw();
  const off = client.subscribe(draw);
  return {
    host,
    setVisible: (v) => (host.style.display = v ? "" : "none"),
    update: (s) => {
      state = s as FountainState;
      draw();
    },
    title: () => (state?.view === "runner" ? "Fountain runner" : "Fountain agents"),
    text: () => plain(state),
    focus: () => host.querySelector<HTMLElement>("input")?.focus(),
    dispose: () => {
      off();
      render(null, host);
      host.remove();
    },
  };
});
