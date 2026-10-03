// Forge blocks (M36): a pull request on the person's Forgejo, read by the
// daemon with their own `tea` login; on GitHub with their `gh` login (M38);
// or a GitLab merge request with their `glab` login (M39; anonymously and
// read-only when glab has none). What waits on you comes first (a
// review asked of you, red checks, changes asked for, a mention), then an
// agent's drafts, the checks, the reviews and the timeline. An agent's
// draft is answered on the card over the block (the daemon's ask): edit
// the text and Send, or Drop. Everything drawn comes from the daemon's
// state, so a shared session's viewers see the same, without the buttons.

import { render } from "preact";
import { useState } from "preact/hooks";
import type { Client } from "../client";
import type { PaneId } from "../proto";
import { askText } from "../ui/menu";
import { registerBlock, type BlockView } from "./view";

interface Branch { repo: string | null; branch: string; sha: string }
interface Item {
  number: number; url: string; title: string; body: string; author: string; state: "open" | "closed" | "merged"; draft: boolean;
  labels: string[]; assignees: string[]; base: Branch; head: Branch; head_ref: string; merge_base: string | null;
  mergeable: boolean | null; requested: ({ user: string } | { team: string })[]; updated_at: number; merged_at: number | null; merged_by: string | null;
}
interface Review { id: string; author: string | null; state: string; commit: string | null; stale: boolean; at: number | null; body: string | null; url: string | null }
interface Check { name: string; source: string; state: string; url: string | null; description: string | null }
interface Event { id: string; at: number; actor: string | null; kind: string; what?: string; target?: { user: string } | { team: string }; body?: string; commits?: number; force?: boolean }
interface Pr { item: Item; reviews: Review[]; checks: Check[]; rollup: string | null; events: Event[] }
interface Draft {
  id: string; method: "comment" | "review" | "merge" | "rerun_checks"; body?: string; event?: string; style?: string; by: string; at_ms: number;
  status: "waiting" | "sent" | "dropped"; settled_by?: string; settled_ms?: number; url?: string; error?: string;
}
export interface ForgeState {
  provider: string; repo: string; number: number; api: string | null; login: string | null; host: string | null; dir: string | null;
  loading: boolean; error: string | null; read_only?: string | null; logins: { name: string; url: string; user: string }[]; me: string | null; pr: Pr | null;
  wants: { kind: "review" | "failed" | "changes" | "mention" | "done"; why: string }[];
  rerun: { api: boolean; url: string | null; note: string; pipeline?: string | null; runs?: number } | null; drafts: Draft[];
  /** M38: GitHub's rate limit, and why it's backing off. */
  rate?: { remaining: number | null; limit: number | null; backoff: string | null } | null;
  updated_ms: number; polls: number; reads: number; watching?: boolean; said: string | null;
}

/** "Open pull request…": a link, OWNER/REPO#N, or N in `dir`'s repository,
 * beside `split` or in a new tab of `session`. */
export async function openPr(client: Client, where: { split?: PaneId; session?: number; dir?: string | null }) {
  const pr = await askText("Open a pull request", "", "a link, OWNER/REPO#N, or N in this repository");
  if (!pr?.trim()) return;
  const place = where.split !== undefined ? { split: where.split, from_pane: where.split } : { session: where.session !== undefined ? String(where.session) : undefined };
  await client.openBlock({ type: "forge", config: { pr: pr.trim(), dir: where.dir ?? undefined }, local: true, ...place }, "couldn't open the pull request");
}

function ago(ms: number | null | undefined): string {
  if (!ms) return "";
  const s = Math.max(0, (Date.now() - ms) / 1000);
  if (s < 60) return "just now";
  if (s < 3600) return `${Math.round(s / 60)}m ago`;
  if (s < 86400) return `${Math.round(s / 3600)}h ago`;
  return `${Math.round(s / 86400)}d ago`;
}

const words = (s: string) => s.replace(/_/g, " ");

function eventLine(e: Event): string {
  switch (e.kind) {
    case "commented":
      return "commented";
    case "review_comment":
      return "commented on the code";
    case "review_requested":
      return `asked ${e.target && "user" in e.target ? e.target.user : e.target && "team" in e.target ? e.target.team : "someone"} for a review`;
    case "pushed":
      return e.commits ? `pushed ${e.commits} commit${e.commits === 1 ? "" : "s"}${e.force ? " (forced)" : ""}` : "pushed";
    case "other":
      return e.what ?? "did something";
    default:
      return words(e.kind);
  }
}

function draftWhat(d: Draft): string {
  if (d.method === "comment") return "a comment";
  if (d.method === "merge") return `a merge (${d.style ?? "merge"})`;
  if (d.method === "rerun_checks") return "a rerun of the checks";
  return d.event === "approve" ? "an approval" : d.event === "request_changes" ? "a review asking for changes" : "a review";
}

function ForgeBlock({ client, id, s }: { client: Client; id: PaneId; s: ForgeState | null }) {
  const [busy, setBusy] = useState<string | null>(null);
  const session = client.sessionOfTab(client.tabOfPane(id)?.id ?? -1) ?? null;
  const role = client.role(session);
  // Writes are the owner's and editors' (they go out with the owner's
  // login, naming who sent them); the clone and the login are the owner's.
  const mayWrite = role !== "viewer" && !s?.read_only;
  const mayOwn = role === "owner";
  const call = async (method: string, args: unknown, failure: string) => {
    setBusy(method);
    const ok = await client.api(`/api/blocks/${id}/call/${method}`, args, failure);
    setBusy(null);
    return ok;
  };
  const comment = async () => {
    const body = await askText("Comment", "", "markdown");
    if (body?.trim()) await call("comment", { body }, "couldn't comment");
  };
  const review = async (event: "approve" | "request_changes" | "comment") => {
    let body: string | null = "";
    if (event !== "approve") {
      body = await askText(event === "request_changes" ? "Request changes" : "Review", "", "what to change");
      if (!body?.trim()) return;
    }
    await call("review", { event, body: body || undefined }, "couldn't review");
  };
  const gitlab = s?.provider === "gitlab";
  const merge = async () => {
    const style = await askText("Merge: how?", "merge", gitlab ? "merge or squash" : s?.provider === "github" ? "merge, squash or rebase" : "merge, rebase, rebase-merge, squash or fast-forward-only");
    if (style?.trim()) await call("merge", { style: style.trim() }, "couldn't merge");
  };
  const refresh = () => void call("refresh", {}, "couldn't read the pull request");
  const viewer = role === "viewer";

  if (!s || (s.loading && !s.updated_ms)) {
    return (
      <div class="review ws forge">
        <div class="browser-card dim">Reading the pull request…</div>
      </div>
    );
  }
  const pr = s.pr;
  const it = pr?.item;
  const state = it ? (it.state === "open" && it.draft ? "draft" : it.state) : null;
  const waiting = s.drafts.filter((d) => d.status === "waiting");
  const settled = s.drafts.filter((d) => d.status !== "waiting").slice(-5).reverse();
  const asked = s.wants.some((w) => w.kind === "review");
  return (
    <div class="review ws forge" data-forge-block={id}>
      <div class="review-bar">
        <span class="review-path" title={it?.url ?? s.repo}>
          <b>
            {s.repo}#{s.number}
          </b>{" "}
          {it?.title}
        </span>
        {state && <span class={`ws-tag forge-state ${state}`} data-pr-state={state}>{state}</span>}
        {it && (
          <a class="dim forge-link" href={it.url} target="_blank" rel="noopener">
            open ↗
          </a>
        )}
        {!viewer && (
          <button title="Read it again" disabled={busy !== null} onClick={refresh}>
            {busy === "refresh" ? "…" : "↻"}
          </button>
        )}
        <span class={`review-live ${s.watching ? "on" : ""}`}>{s.watching ? "live" : "paused"}</span>
      </div>
      {s.rate?.backoff && (
        <p class="dim ws-note" data-forge-rate>
          {s.rate.backoff}
        </p>
      )}
      {s.error && (
        <div class="browser-card" data-forge-error>
          <p>Can't read this pull request</p>
          <p class="dim">{s.error}</p>
          {mayOwn && s.logins.length > 0 && (
            <div class="ws-actions" data-forge-logins>
              {s.logins.map((l) => (
                <button key={l.name} onClick={() => void call("login", { name: l.name }, "couldn't use that login")}>
                  Use login {l.name} ({l.url})
                </button>
              ))}
            </div>
          )}
          {!viewer && <button onClick={refresh}>Try again</button>}
        </div>
      )}
      {s.read_only && (
        <p class="dim ws-note" data-forge-read-only>
          {s.read_only}
        </p>
      )}
      {pr && it && (
        <div class="review-body ws-body">
          <div class="dim forge-meta">
            {it.author} wants to merge <code>{it.head.repo && it.head.repo !== s.repo ? `${it.head.repo}:` : ""}{it.head.branch}</code> into <code>{it.base.branch}</code>
            {" · "}
            {s.login && `as ${s.me ?? "?"} (${gitlab ? s.login.replace(/^glab:/, "glab, ") : s.provider === "github" ? `gh on ${s.login}` : `tea login ${s.login}`})`}
            {!s.login && s.read_only && "anonymously, read-only"}
            {it.labels.length > 0 && " · "}
            {it.labels.map((l) => (
              <span key={l} class="ws-tag">
                {l}
              </span>
            ))}
          </div>
          {(s.wants.length > 0 || waiting.length > 0) && (
            <section class="ws-gates" data-forge-wants>
              <h4>Waiting on you</h4>
              {waiting.length > 0 && (
                <p class="forge-want" data-forge-drafts-waiting>
                  {waiting.length === 1 ? "A draft waits" : `${waiting.length} drafts wait`} for you to send it: it's on the card
                </p>
              )}
              {s.wants.map((w) => (
                <div class="ws-gate" key={w.kind} data-want={w.kind}>
                  <div class="ws-gate-what">{w.why}</div>
                  <div class="ws-actions">
                    {w.kind === "review" && mayWrite && (
                      <>
                        <button class="pri" data-approve-review disabled={busy !== null} onClick={() => void review("approve")}>
                          {busy === "review" ? "Approving…" : "Approve"}
                        </button>
                        <button disabled={busy !== null} onClick={() => void review("request_changes")}>
                          Request changes…
                        </button>
                      </>
                    )}
                    {w.kind === "failed" && s.rerun?.api && mayWrite && (
                      <button class="pri" data-rerun disabled={busy !== null} title={s.rerun.note} onClick={() => void call("rerun_checks", {}, "couldn't rerun the checks")}>
                        {busy === "rerun_checks" ? "Retrying…" : "Rerun"}
                      </button>
                    )}
                    {w.kind === "failed" && s.rerun?.url && (
                      <a class="button" href={s.rerun.url} target="_blank" rel="noopener" title={s.rerun.note} data-rerun-link>
                        Open the run ↗
                      </a>
                    )}
                  </div>
                </div>
              ))}
              {s.rerun && !s.rerun.api && <p class="dim ws-note">{s.rerun.note}</p>}
              {viewer && <p class="dim ws-note">You're watching this session: the owner or an editor answers.</p>}
            </section>
          )}
          {s.said && <p class="ws-said" data-forge-said>{s.said}</p>}
          {s.drafts.length > 0 && (
            <section data-forge-drafts>
              <h4>Drafts</h4>
              {[...waiting, ...settled].map((d) => (
                <div key={d.id} class={`forge-draft ${d.status}`} data-draft={d.id} data-draft-status={d.status}>
                  <div>
                    <b>{d.by.replace(/^mcp:/, "")}</b> drafted {draftWhat(d)} <span class="dim">{ago(d.at_ms)}</span>
                    {d.status === "sent" && (
                      <span class="ws-tag">
                        sent by {d.settled_by}
                        {d.url && (
                          <>
                            {" "}
                            <a href={d.url} target="_blank" rel="noopener">
                              ↗
                            </a>
                          </>
                        )}
                      </span>
                    )}
                    {d.status === "dropped" && <span class="ws-tag">dropped by {d.settled_by}</span>}
                    {d.error && <span class="ws-tag bad">{d.error}</span>}
                  </div>
                  {d.body && <div class="forge-body">{d.body}</div>}
                </div>
              ))}
            </section>
          )}
          <section data-forge-checks>
            <h4>
              Checks {pr.rollup ? <span class={`ws-tag forge-check ${pr.rollup}`}>{words(pr.rollup)}</span> : <span class="dim">none</span>}
            </h4>
            {pr.checks.map((c) => (
              <div key={c.name} class="forge-row" data-check={c.state}>
                <span class={`forge-dot ${c.state}`} title={words(c.state)} />
                {c.url ? (
                  <a href={c.url} target="_blank" rel="noopener">
                    {c.name}
                  </a>
                ) : (
                  c.name
                )}
                {c.description && <span class="dim"> · {c.description}</span>}
              </div>
            ))}
          </section>
          <section data-forge-reviews>
            <h4>Reviews ({pr.reviews.length})</h4>
            {it.requested.length > 0 && (
              <p class="dim">
                Asked: {it.requested.map((r) => ("user" in r ? r.user : r.team)).join(", ")}
                {asked && " (you)"}
              </p>
            )}
            {pr.reviews.map((r) => (
              <div key={r.id} class="forge-row" data-review={r.state}>
                <b>{r.author ?? "a team"}</b> <span class={`ws-tag forge-review ${r.state}`}>{words(r.state)}</span>
                {r.stale && <span class="dim"> (an older head)</span>}
                {r.body && <div class="forge-body">{r.body}</div>}
              </div>
            ))}
          </section>
          {it.body.trim() && (
            <section>
              <h4>Description</h4>
              <div class="forge-body">{it.body}</div>
            </section>
          )}
          <section data-forge-timeline>
            <h4>Timeline</h4>
            {[...pr.events].reverse().map((e) => (
              <div key={e.id} class="forge-row forge-event">
                <span class="dim">{ago(e.at)}</span> <b>{e.actor ?? "someone"}</b> {eventLine(e)}
                {e.body && <div class="forge-body">{e.body}</div>}
              </div>
            ))}
          </section>
          {(mayWrite || mayOwn) && (
            <div class="ws-actions forge-actions">
              {mayWrite && it.state === "open" && <button onClick={() => void comment()}>Comment…</button>}
              {mayWrite && it.state === "open" && !asked && <button onClick={() => void review("comment")}>Review…</button>}
              {mayWrite && it.state === "open" && <button onClick={() => void merge()}>Merge…</button>}
              {mayOwn && (
                <button data-forge-diff disabled={busy !== null} onClick={() => void call("diff", {}, "couldn't show the changes")}>
                  Diff
                </button>
              )}
              {mayOwn && (
                <button data-forge-checkout disabled={busy !== null} onClick={() => void call("checkout", {}, "couldn't check it out")}>
                  Checkout
                </button>
              )}
            </div>
          )}
          <p class="dim ws-note">
            {s.polls} polls, {s.reads} full reads · read {ago(s.updated_ms)}
          </p>
        </div>
      )}
    </div>
  );
}

function plain(s: ForgeState | null): string {
  if (!s) return "";
  const it = s.pr?.item;
  const lines = [`${s.repo}#${s.number} ${it?.title ?? ""}`.trim()];
  if (s.error) lines.push(s.error);
  for (const w of s.wants) lines.push(`waiting on you: ${w.why}`);
  for (const d of s.drafts) lines.push(`draft ${d.id} ${d.status}: ${d.body ?? d.method}`);
  for (const c of s.pr?.checks ?? []) lines.push(`${c.state} ${c.name}`);
  for (const r of s.pr?.reviews ?? []) lines.push(`${r.author ?? "?"} ${r.state}`);
  return lines.join("\n");
}

registerBlock("forge", (client, id): BlockView => {
  const host = document.createElement("div");
  host.className = "block block-forge";
  let state: ForgeState | null = null;
  const draw = () => render(<ForgeBlock client={client} id={id} s={state} />, host);
  draw();
  const off = client.subscribe(draw);
  return {
    host,
    setVisible: (v) => (host.style.display = v ? "" : "none"),
    update: (s) => {
      state = s as ForgeState;
      draw();
    },
    title: () => `${state?.repo ?? "pull request"}#${state?.number ?? ""}`,
    text: () => plain(state),
    focus: () => host.querySelector<HTMLElement>("button")?.focus(),
    dispose: () => {
      off();
      render(null, host);
      host.remove();
    },
  };
});
