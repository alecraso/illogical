// Fountain blocks (M43): the person's Fountain agents as a catalog, read by
// the daemon with their own `fountain` login. A card per agent (what it
// runs, its skills and MCP servers, its environment and sandbox, where it
// comes from), in a grid (a list on the phone), with the filter bar on top:
// a search box and chips for source, runtime and sandbox provider. The
// filters are the block's (kept in its config), so every client and the
// phone see the same list. Each card: *Run on Fountain* (an agent block
// beside it), *Run here* (M44; shown for claude agents, greyed until then)
// and *Spec* (the agent-specs file, else Fountain's page). Everything drawn
// comes from the daemon's state; viewers get the list without the buttons.

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
export interface FountainState {
  view: "catalog"; profile: string | null; profiles: string[]; base_url: string | null; key_from: "env" | "file" | null;
  loading: boolean; error: string | null; agents: Card[]; total: number; unreadable?: number; unreadable_note?: string | null; filter: Filter;
  counts: { source: Record<string, number>; runtime: Record<string, number>; provider: Record<string, number> };
  specs: string | null; specs_why: string | null; updated_ms: number; polls: number; watching?: boolean; said: string | null;
}

/** "Fountain agents…": the catalog beside `split`, or in a new tab of `session`. */
export async function openFountain(client: Client, where: { split?: PaneId; session?: number }) {
  const place = where.split !== undefined ? { split: where.split, from_pane: where.split } : { session: where.session !== undefined ? String(where.session) : undefined };
  await client.openBlock({ type: "fountain", config: { view: "catalog" }, local: true, ...place }, "couldn't open the Fountain catalog");
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

function plain(s: FountainState | null): string {
  if (!s) return "";
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

function FountainBlock({ client, id, s }: { client: Client; id: PaneId; s: FountainState | null }) {
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
                    <button data-run-here={c.name} disabled title={c.local_why ?? "Run here comes with M44: this agent, worn by a Claude Code on this machine"}>
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
    title: () => "Fountain agents",
    text: () => plain(state),
    focus: () => host.querySelector<HTMLElement>("input")?.focus(),
    dispose: () => {
      off();
      render(null, host);
      host.remove();
    },
  };
});
