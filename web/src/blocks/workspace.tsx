// Workspace blocks (M34): a chant workspace, read by the daemon through
// chant's read contract. Gates waiting for a person come first (approved
// here by the owner and editors), then a card per member to open a shell,
// an agent, its changes or a nested workspace on, then the records.
// Everything drawn comes from the daemon's state, so a shared session's
// viewers see the same, without the buttons.

import { render } from "preact";
import { useEffect, useState } from "preact/hooks";
import type { Client } from "../client";
import { gateKey, type Gate, type PaneId, type RunRequest } from "../proto";
import { registerBlock, type BlockView } from "./view";

interface Diagnostic { rule: string; severity: string; message: string; file: string | null; line: number | null }
interface Member {
  name: string; dir: string; path: string; kind: string; because: string | null; roles: string[]; nested: boolean;
  unreadable: string | null; errors: number; warnings: number; diagnostics: Diagnostic[]; releases: number; gates: number;
}
interface Rec { kind: string; id: string; title: string | null; state: string | null; ready: boolean | null; blocked_by: string[]; warnings: string[]; valid: boolean }
interface Read { name: string; ms: number; code: number; ok: boolean; note: string | null }
export interface WorkspaceState {
  root: string; name: string | null; chant: string | null; how: string | null; version: string | null; env: string;
  members: Member[]; records: Rec[]; records_note: string | null; gates: Gate[]; diagnostics: Diagnostic[];
  reads: Read[]; ms: number; error: string | null; headline: string | null; loading: boolean; updated_ms: number; watching?: boolean;
}

/** Directories known to hold a `chant.workspace.json`, or not, by the
 * pane whose host they're on. */
const known = new Map<string, boolean>();

/** Whether `dir`, where `pane` runs (its machine, or this host), is a chant
 * workspace: it holds a `chant.workspace.json`. */
export async function isWorkspace(client: Client, pane: PaneId, dir: string): Promise<boolean> {
  const key = `${client.machine(pane)?.id ?? "here"}:${dir}`;
  const had = known.get(key);
  if (had !== undefined) return had;
  try {
    const path = `${dir.replace(/\/$/, "")}/chant.workspace.json`;
    const res = await client.request("GET", `/api/fs/stat?pane=${pane}&path=${encodeURIComponent(path)}`);
    known.set(key, res.ok);
    return res.ok;
  } catch {
    return false;
  }
}

/** Whether the directory `pane` is in is a chant workspace, looked up when
 * it changes (the owner's only: files are theirs), for menus to offer
 * "Open as workspace". */
export function useWorkspaceDir(client: Client, pane: PaneId, dir: string | null | undefined): boolean {
  const [yes, setYes] = useState(false);
  useEffect(() => {
    setYes(false);
    if (!dir || client.state?.roles) return;
    let live = true;
    void isWorkspace(client, pane, dir).then((v) => live && setYes(v));
    return () => {
      live = false;
    };
  }, [client, pane, dir]);
  return yes;
}

/** A workspace block for `root`, beside `from` (on its host), or in a new
 * tab here. */
export function openWorkspace(client: Client, root: string, from?: PaneId, env = "local") {
  void client.openBlock(
    { type: "workspace", config: { root, env }, ...(from !== undefined ? { split: from, from_pane: from } : { local: true }) },
    "couldn't open the workspace",
  );
}

/** "3m ago", "2h ago" from an RFC 3339 time. */
function ago(t: string | undefined): string {
  if (!t) return "";
  const s = Math.max(0, (Date.now() - Date.parse(t)) / 1000);
  if (s < 60) return "just now";
  if (s < 3600) return `${Math.round(s / 60)}m ago`;
  if (s < 86400) return `${Math.round(s / 3600)}h ago`;
  return `${Math.round(s / 86400)}d ago`;
}

function hint(error: string, root: string): string | null {
  if (error.startsWith("no chant here")) return `Run npm install in ${root}, or set CHANT for the daemon.`;
  if (error.startsWith("no chant.workspace.json")) return "This directory isn't a chant workspace.";
  return null;
}

function WorkspaceBlock({ client, id, s }: { client: Client; id: PaneId; s: WorkspaceState | null }) {
  const [busy, setBusy] = useState<string | null>(null);
  const [said, setSaid] = useState<string | null>(null);
  const session = client.sessionOfTab(client.tabOfPane(id)?.id ?? -1) ?? null;
  const role = client.role(session);
  // Approving is for the owner and editors (#75); opening panes and blocks
  // on the host is the owner's.
  const mayApprove = role !== "viewer";
  const mayOpen = role === "owner";
  const beside = { split: id, from_pane: id };
  const shell = (cwd: string) => void client.make("/api/run", { ...beside, cwd } satisfies RunRequest).then((e) => e && client.toast(e));
  const agent = (m: Member) => void client.openBlock({ type: "agent", config: { agent: "claude", cwd: m.path }, ...beside }, "couldn't start the agent");
  const changes = (m: Member) => void client.openBlock({ type: "diff", config: { repo: m.path }, ...beside }, "couldn't show the changes");
  const nested = (m: Member) => openWorkspace(client, m.path, id, s?.env ?? "local");
  const runOp = (g: Gate) =>
    g.source.kind === "chant" &&
    void client.make("/api/run", { ...beside, cwd: g.source.dir, command: `${s?.chant ?? "chant"} run ${g.op}` } satisfies RunRequest).then((e) => e && client.toast(e));
  const approve = async (g: Gate) => {
    setBusy(gateKey(g));
    setSaid(null);
    const ok = await client.api(`/api/blocks/${id}/call/approve`, { key: gateKey(g) }, "couldn't approve it");
    setBusy(null);
    if (ok) setSaid(`Approved ${g.gate}. Run ${g.op} again to walk through it.`);
  };
  const refresh = () => void client.api(`/api/blocks/${id}/call/refresh`, {}, "couldn't read the workspace");

  if (!s || (s.loading && !s.updated_ms)) {
    return (
      <div class="review ws">
        <div class="browser-card dim">Reading the workspace…</div>
      </div>
    );
  }
  const help = s.error ? hint(s.error, s.root) : null;
  return (
    <div class="review ws" data-workspace-block={id}>
      <div class="review-bar">
        <span class="review-path" title={s.root}>
          <b>{s.name ?? "workspace"}</b> {s.root}
        </span>
        <span class="dim ws-meta" title={s.chant ?? ""}>
          {s.env} · chant {s.version ?? "?"}
        </span>
        {mayOpen && (
          <button title="Read it again" disabled={s.loading} onClick={refresh}>
            {s.loading ? "…" : "↻"}
          </button>
        )}
        <span class={`review-live ${s.watching ? "on" : ""}`}>{s.watching ? "live" : "paused"}</span>
      </div>
      {s.error ? (
        <div class="browser-card" data-ws-error>
          <p>Can't show this workspace</p>
          <p class="dim">{s.error}</p>
          {help && <p class="dim">{help}</p>}
          {mayOpen && <button onClick={refresh}>Try again</button>}
        </div>
      ) : (
        <div class="review-body ws-body">
          {s.gates.length > 0 && (
            <section class="ws-gates" data-ws-gates>
              <h4>Waiting on you</h4>
              {s.gates.map((g) => (
                <div class="ws-gate" key={gateKey(g)} data-gate={gateKey(g)}>
                  <div class="ws-gate-what">
                    <b>{g.member}</b>: {g.op} waits at gate <b>{g.gate}</b>
                    <div class="dim ws-gate-when">
                      {g.approvals}/{g.needed} approvals
                      {g.since && ` · ${ago(g.since)}`}
                      {g.expires && ` · expires ${new Date(g.expires).toLocaleString()}`}
                    </div>
                  </div>
                  <div class="ws-actions">
                    {mayApprove && (
                      <button class="pri" data-approve disabled={busy !== null} onClick={() => void approve(g)}>
                        {busy === gateKey(g) ? "Approving…" : "Approve"}
                      </button>
                    )}
                    {mayOpen && <button onClick={() => runOp(g)}>Run {g.op}</button>}
                  </div>
                </div>
              ))}
              {!mayApprove && <p class="dim ws-note">You're watching this session: the owner or an editor approves.</p>}
            </section>
          )}
          {said && <p class="ws-said" data-ws-said>{said}</p>}
          {s.diagnostics.length > 0 && (
            <section>
              {s.diagnostics.map((d, i) => (
                <p key={i} class={`ws-diag ${d.severity}`}>
                  {d.rule}: {d.message}
                </p>
              ))}
            </section>
          )}
          <section>
            <h4>Members ({s.members.length})</h4>
            <div class="ws-members">
              {s.members.map((m) => {
                const problems = m.diagnostics.map((d) => `${d.rule}: ${d.message}`).join("\n");
                return (
                  <div class={`ws-card${m.errors || m.unreadable ? " bad" : ""}${m.gates ? " waits" : ""}`} key={m.name} data-member={m.name}>
                    <div class="ws-card-head">
                      <b title={m.name}>{m.name}</b>
                      <span class="ws-kind" title={m.because ?? ""}>
                        {m.kind}
                      </span>
                    </div>
                    <div class="dim ws-dir" title={m.path}>
                      {m.dir}
                    </div>
                    <div class="ws-tags" title={problems}>
                      {m.gates > 0 && <span class="ws-tag waits">{m.gates === 1 ? "gate waits" : `${m.gates} gates wait`}</span>}
                      {m.errors > 0 && <span class="ws-tag bad">{m.errors} errors</span>}
                      {m.warnings > 0 && <span class="ws-tag">{m.warnings} warnings</span>}
                      {m.releases > 0 && <span class="ws-tag">{m.releases} releases</span>}
                      {m.unreadable && <span class="ws-tag bad">{m.unreadable}</span>}
                      {m.roles.map((r) => (
                        <span key={r} class="ws-tag">
                          {r}
                        </span>
                      ))}
                    </div>
                    {mayOpen && (
                      <div class="ws-actions">
                        {m.nested ? (
                          <button data-open-nested onClick={() => nested(m)}>
                            Open
                          </button>
                        ) : (
                          <>
                            <button onClick={() => shell(m.path)}>Shell</button>
                            <button onClick={() => agent(m)}>Agent</button>
                            <button onClick={() => changes(m)}>Changes</button>
                          </>
                        )}
                      </div>
                    )}
                  </div>
                );
              })}
            </div>
          </section>
          {s.records.length > 0 ? (
            <section class="ws-records">
              <h4>Records ({s.records.length})</h4>
              {s.records.map((r) => (
                <div class="ws-record" key={`${r.kind}/${r.id}`}>
                  <b>{r.id}</b> <span class="ws-kind">{r.state ?? "—"}</span> {r.title}
                  {r.blocked_by.length > 0 && <span class="dim"> · blocked by {r.blocked_by.join(", ")}</span>}
                  {r.warnings.map((w, i) => (
                    <div key={i} class="ws-diag warning">
                      {w}
                    </div>
                  ))}
                </div>
              ))}
            </section>
          ) : (
            s.records_note && <p class="dim ws-note">Records: {s.records_note}</p>
          )}
          <p class="dim ws-note">
            {s.reads.map((r) => `${r.name} ${r.ms} ms${r.ok ? "" : " (failed)"}`).join(" · ")} · read {ago(new Date(s.updated_ms).toISOString())}
          </p>
        </div>
      )}
    </div>
  );
}

function plain(s: WorkspaceState | null): string {
  if (!s) return "";
  const lines = [`${s.name ?? "workspace"} ${s.root}`];
  if (s.error) lines.push(s.error);
  for (const g of s.gates) lines.push(`waiting: ${g.member}: ${g.op} at gate ${g.gate}`);
  for (const m of s.members) lines.push(`${m.name} ${m.kind} ${m.dir}`);
  for (const r of s.records) lines.push(`${r.id} ${r.state ?? ""} ${r.title ?? ""}`);
  return lines.join("\n");
}

registerBlock("workspace", (client, id): BlockView => {
  const host = document.createElement("div");
  host.className = "block block-workspace";
  let state: WorkspaceState | null = null;
  const draw = () => render(<WorkspaceBlock client={client} id={id} s={state} />, host);
  draw();
  // Roles can change (a share made view-only) without the block changing.
  const off = client.subscribe(draw);
  return {
    host,
    setVisible: (v) => (host.style.display = v ? "" : "none"),
    update: (s) => {
      state = s as WorkspaceState;
      draw();
    },
    title: () => `${state?.name ?? "workspace"} (chant)`,
    text: () => plain(state),
    focus: () => host.querySelector<HTMLElement>("button")?.focus(),
    dispose: () => {
      off();
      render(null, host);
      host.remove();
    },
  };
});
