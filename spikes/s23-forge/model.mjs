// S23: the normalized model (item / review / check / event) and one adapter per forge, as a JS
// stand-in for M36's Rust (field names are the proposed struct fields), plus the attention rules.
//
//   node model.mjs fixtures/forgejo-illogical-84 forgejo [--me jhgaylor] [--teams a,b]
//   node model.mjs --all            every fixture, with the "me" each one exercises
//
// Pure: reads a fixture directory (what read.mjs --save wrote), prints the model and attention.

import { readFileSync, existsSync } from "node:fs";
import { join } from "node:path";

const j = (dir, f) => (existsSync(join(dir, f)) ? JSON.parse(readFileSync(join(dir, f), "utf8")) : null);
const t = (s) => (s ? Date.parse(s) : null);
const unmapped = new Set();

// ---------------------------------------------------------------- states
// CheckState: queued | running | success | failure | cancelled | skipped | neutral | action_required | manual
const GH_CONCLUSION = { success: "success", failure: "failure", timed_out: "failure", startup_failure: "failure",
  cancelled: "cancelled", skipped: "skipped", neutral: "neutral", stale: "neutral", action_required: "action_required" };
const STATUS = { success: "success", failure: "failure", error: "failure", pending: "running", warning: "neutral", skipped: "skipped" };
const GL_JOB = { success: "success", failed: "failure", canceled: "cancelled", skipped: "skipped", manual: "manual",
  created: "queued", pending: "queued", waiting_for_resource: "queued", preparing: "queued", scheduled: "queued",
  running: "running" };
// ReviewState: approved | changes_requested | commented | dismissed | requested | pending
const REVIEW = { APPROVED: "approved", CHANGES_REQUESTED: "changes_requested", REQUEST_CHANGES: "changes_requested",
  COMMENTED: "commented", COMMENT: "commented", DISMISSED: "dismissed", REQUEST_REVIEW: "requested", PENDING: "pending" };

/** The rollup over checks: failure if any (counted) failed, running if any not done, else success; none → null. */
export function rollup(checks) {
  const counted = checks.filter((c) => !c.allow_failure && !["skipped", "neutral", "manual"].includes(c.state));
  if (counted.length === 0) return null;
  if (counted.some((c) => ["failure", "cancelled", "action_required"].includes(c.state))) return "failure";
  if (counted.some((c) => ["queued", "running"].includes(c.state))) return "running";
  return "success";
}

// ---------------------------------------------------------------- Forgejo
function forgejo(dir) {
  const it = j(dir, "item.json"), reviews = j(dir, "reviews.json") || [], st = j(dir, "statuses.json"),
    tl = j(dir, "timeline.json") || [];
  const [owner, name] = it.base.repo.full_name.split("/");
  // Forgejo's requested_reviewers lists everyone asked *or who reviewed*; a request is pending only while
  // that reviewer's (or team's) latest review entry is REQUEST_REVIEW.
  const latest = new Map();
  for (const r of reviews) latest.set(r.user ? `u:${r.user.login}` : `t:${r.team?.name}`, r);
  const requested = [...latest.entries()].filter(([, r]) => r.state === "REQUEST_REVIEW")
    .map(([k]) => (k.startsWith("u:") ? { user: k.slice(2) } : { team: k.slice(2) }));
  const item = {
    provider: "forgejo", host: new URL(it.html_url).host, repo: `${owner}/${name}`, number: it.number, kind: "pr",
    url: it.html_url, title: it.title, body: it.body, author: it.user.login,
    state: it.merged ? "merged" : it.state, draft: it.draft, labels: it.labels.map((l) => l.name),
    assignees: (it.assignees || []).map((a) => a.login),
    base: { repo: it.base.repo.full_name, branch: it.base.ref, sha: it.base.sha },
    head: { repo: it.head.repo?.full_name ?? null, branch: it.head.ref, sha: it.head.sha },
    head_ref: `refs/pull/${it.number}/head`, merge_base: it.merge_base, mergeable: it.mergeable,
    requested, updated_at: t(it.updated_at), merged_at: t(it.merged_at),
  };
  const out = reviews.filter((r) => r.state !== "REQUEST_REVIEW").map((r) => ({
    id: String(r.id), author: r.user?.login ?? null, team: r.team?.name ?? null,
    state: r.dismissed ? "dismissed" : REVIEW[r.state] ?? (unmapped.add(`forgejo review ${r.state}`), "commented"),
    commit: r.commit_id, at: t(r.submitted_at), body: r.body, stale: r.stale, comments: r.comments_count,
  }));
  // Forgejo Actions report as commit statuses (target_url .../actions/runs/<index>/jobs/<n>).
  const checks = (st?.statuses || []).map((s) => {
    const run = (s.target_url || "").match(/actions\/runs\/(\d+)\/jobs\/(\d+)/);
    return { name: s.context, source: run ? "action" : "status",
      state: STATUS[s.status] ?? (unmapped.add(`forgejo status ${s.status}`), "neutral"),
      allow_failure: false, url: s.target_url, commit: it.head.sha, description: s.description,
      run: run ? { id: run[1], job: run[2] } : null };
  });
  const FJ = { comment: "commented", review: "reviewed", review_request: "review_requested", pull_push: "pushed",
    label: "labeled", merge_pull: "merged", close: "closed", reopen: "reopened", commit_ref: "referenced",
    issue_ref: "referenced", pull_ref: "referenced", comment_ref: "referenced", milestone: "milestone", assignees: "assigned",
    delete_branch: "branch_deleted", change_title: "renamed", dismiss_review: "review_dismissed", code: "review_comment" };
  const events = tl.map((e) => {
    const kind = FJ[e.type] ?? (unmapped.add(`forgejo event ${e.type}`), e.type);
    let target = null, extra = {};
    if (e.type === "review_request") target = e.assignee ? { user: e.assignee.login } : { team: e.assignee_team?.name };
    if (e.type === "pull_push") { try { const p = JSON.parse(e.body); extra = { force: p.is_force_push, commits: p.commit_ids.length }; } catch {} }
    return { id: `fj-${e.id}`, at: t(e.created_at), actor: e.user?.login ?? null, kind, target,
      body: ["comment", "code"].includes(e.type) ? e.body : null, ...extra };
  });
  return { item, reviews: out, checks, rollup: rollup(checks), forge_rollup: st?.state ?? null, events };
}

// ---------------------------------------------------------------- GitHub
function github(dir) {
  const it = j(dir, "item.json"), reviews = j(dir, "reviews.json") || [], cr = j(dir, "check_runs.json"),
    st = j(dir, "statuses.json"), tl = j(dir, "timeline.json") || [];
  const item = {
    provider: "github", host: "github.com", repo: it.base.repo.full_name, number: it.number, kind: "pr",
    url: it.html_url, title: it.title, body: it.body, author: it.user.login,
    state: it.merged ? "merged" : it.state, draft: it.draft, labels: it.labels.map((l) => l.name),
    assignees: (it.assignees || []).map((a) => a.login),
    base: { repo: it.base.repo.full_name, branch: it.base.ref, sha: it.base.sha },
    head: { repo: it.head.repo?.full_name ?? null, branch: it.head.ref, sha: it.head.sha },
    head_ref: `refs/pull/${it.number}/head`, merge_base: null, // compute: git merge-base base.sha head.sha
    mergeable: it.mergeable, // null while GitHub computes it
    blocked: it.mergeable_state === "blocked", // branch protection: a required review or check is missing
    // GitHub drops a request once that reviewer reviews, so these are pending ones.
    requested: [...it.requested_reviewers.map((u) => ({ user: u.login })), ...it.requested_teams.map((x) => ({ team: x.slug }))],
    updated_at: t(it.updated_at), merged_at: t(it.merged_at),
  };
  const out = reviews.map((r) => ({ id: String(r.id), author: r.user?.login ?? null, team: null,
    state: REVIEW[r.state] ?? (unmapped.add(`github review ${r.state}`), "commented"), commit: r.commit_id,
    at: t(r.submitted_at), body: r.body, stale: r.commit_id !== it.head.sha, comments: null }));
  // Both feed checks: check runs (Actions, apps) and commit statuses (older CI). An empty combined status
  // says "pending" with total_count 0: that's "no statuses", not pending.
  const checks = [
    ...(cr?.check_runs || []).map((c) => ({ name: c.name, source: "check_run",
      state: c.status !== "completed" ? (c.status === "queued" ? "queued" : "running")
        : GH_CONCLUSION[c.conclusion] ?? (unmapped.add(`github conclusion ${c.conclusion}`), "neutral"),
      allow_failure: false, url: c.html_url, commit: c.head_sha, description: c.output?.title ?? null,
      run: { id: String(c.id), suite: String(c.check_suite?.id), app: c.app?.slug } })),
    ...(st?.statuses || []).map((s) => ({ name: s.context, source: "status", state: STATUS[s.state] ?? "neutral",
      allow_failure: false, url: s.target_url, commit: it.head.sha, description: s.description, run: null })),
  ];
  const GH = { commented: "commented", reviewed: "reviewed", review_requested: "review_requested",
    review_request_removed: "review_request_removed", committed: "pushed", head_ref_force_pushed: "pushed",
    labeled: "labeled", unlabeled: "unlabeled", merged: "merged", closed: "closed", reopened: "reopened",
    referenced: "referenced", "cross-referenced": "referenced", mentioned: "mentioned", subscribed: null,
    unsubscribed: null, head_ref_deleted: "branch_deleted", renamed: "renamed", review_dismissed: "review_dismissed",
    ready_for_review: "ready_for_review", convert_to_draft: "converted_to_draft", assigned: "assigned",
    copilot_work_started: null, copilot_work_finished: null };
  const events = tl.flatMap((e) => {
    const kind = e.event in GH ? GH[e.event] : (unmapped.add(`github event ${e.event}`), e.event);
    if (kind === null) return [];
    // A `mentioned` event's actor is who was mentioned, not who wrote the mention.
    const actor = e.event === "mentioned" ? null : e.actor?.login ?? e.user?.login ?? e.author?.name ?? null;
    const target = e.event === "review_requested" || e.event === "review_request_removed"
      ? (e.requested_reviewer ? { user: e.requested_reviewer.login } : { team: e.requested_team?.slug })
      : e.event === "mentioned" ? { user: e.actor?.login } : null; // GitHub: a mention's actor is who was mentioned
    return [{ id: `gh-${e.id ?? e.sha ?? e.node_id}`, at: t(e.created_at ?? e.submitted_at ?? e.committer?.date),
      actor, kind, target, body: e.event === "commented" || e.event === "reviewed" ? e.body ?? null : null,
      force: e.event === "head_ref_force_pushed" || undefined, state: e.state ?? undefined }];
  });
  return { item, reviews: out, checks, rollup: rollup(checks), forge_rollup: null, events };
}

// ---------------------------------------------------------------- GitLab
function gitlab(dir) {
  const it = j(dir, "item.json"), appr = j(dir, "approvals.json"), rv = j(dir, "reviewers.json") || [],
    pipes = j(dir, "pipelines.json") || [], jobs = j(dir, "pipeline_jobs.json") || [], disc = j(dir, "discussions.json");
  const repo = it.references.full.replace(/!\d+$/, "");
  const item = {
    provider: "gitlab", host: new URL(it.web_url).host, repo, number: it.iid, kind: "pr",
    url: it.web_url, title: it.title, body: it.description, author: it.author.username,
    state: it.state === "opened" ? "open" : it.state, // opened | closed | merged | locked
    draft: it.draft, labels: it.labels, assignees: it.assignees.map((a) => a.username),
    base: { repo, branch: it.target_branch, sha: it.diff_refs?.start_sha },
    head: { repo: it.source_project_id === it.target_project_id ? repo : `project:${it.source_project_id}`,
      branch: it.source_branch, sha: it.sha },
    head_ref: `refs/merge-requests/${it.iid}/head`, merge_base: it.diff_refs?.base_sha, // not start_sha
    mergeable: it.detailed_merge_status === "mergeable" ? true : it.state === "opened" ? false : null,
    // GitLab reviewers carry their own state (unreviewed | review_started | reviewed | requested_changes | approved).
    requested: rv.filter((r) => ["unreviewed", "review_started"].includes(r.state)).map((r) => ({ user: r.user.username })),
    updated_at: t(it.updated_at), merged_at: t(it.merged_at),
  };
  const GL_RV = { approved: "approved", requested_changes: "changes_requested", reviewed: "commented" };
  const reviews = [
    ...rv.filter((r) => GL_RV[r.state]).map((r) => ({ id: `rv-${r.user.id}`, author: r.user.username, team: null,
      state: GL_RV[r.state], commit: null, at: t(r.created_at), body: null, stale: false, comments: null })),
    // Approvals are separate from reviewer state; someone can approve without being a reviewer.
    ...(appr?.approved_by || []).filter((a) => !rv.some((r) => r.user.username === a.user.username && r.state === "approved"))
      .map((a) => ({ id: `ap-${a.user.id}`, author: a.user.username, team: null, state: "approved", commit: null,
        at: null, body: null, stale: false, comments: null })),
  ];
  // Checks: the head pipeline's jobs. With merged-results pipelines the head pipeline runs on a merge
  // commit (its sha isn't the MR's), so "checks on head.sha" means "the MR's head_pipeline".
  const checks = jobs.map((x) => ({ name: x.name, source: "pipeline_job",
    state: GL_JOB[x.status] ?? (unmapped.add(`gitlab job ${x.status}`), "neutral"), allow_failure: x.allow_failure,
    url: x.web_url, commit: x.commit?.id ?? null, description: x.stage,
    run: { id: String(x.id), pipeline: String(x.pipeline?.id ?? it.head_pipeline?.id) } }));
  // The timeline: discussions (notes, threads; system notes are GitLab's events). Anonymous reads get 401.
  const events = (disc || []).flatMap((d) => d.notes.map((n) => ({ id: `gl-${n.id}`, at: t(n.created_at),
    actor: n.author.username, kind: n.system ? "system" : n.position ? "review_comment" : "commented",
    thread: d.individual_note ? null : d.id, resolved: n.resolvable ? n.resolved : undefined, body: n.body, target: null })));
  return { item, reviews, checks, rollup: rollup(checks), forge_rollup: it.head_pipeline?.status ?? null, events,
    approvals: appr ? { required: appr.approvals_required, left: appr.approvals_left } : null,
    pipelines: pipes.map((p) => ({ id: p.id, sha: p.sha, status: p.status })) };
}

export const adapters = { forgejo, github, gitlab };

// ---------------------------------------------------------------- attention
/**
 * What the PR wants of `me` (M24 reasons): [{reason, why}].
 *   ask    a review requested from me, directly or through one of `teams`
 *   failed my open PR's checks are red
 *   input  changes requested on my open PR; a mention of me after `seen`
 *   done   my PR merged; or open, green, approved, nothing requested of me
 */
export function attention(pr, me, teams = [], seen = 0) {
  const out = [], it = pr.item, mine = it.author === me, open = it.state === "open";
  const req = it.requested.find((r) => r.user === me || (r.team && teams.includes(r.team)));
  if (open && req) out.push({ reason: "ask", why: req.user ? "review requested from you" : `review requested from ${req.team}` });
  if (mine && open && pr.rollup === "failure") {
    const red = pr.checks.filter((c) => ["failure", "cancelled", "action_required"].includes(c.state) && !c.allow_failure);
    out.push({ reason: "failed", why: `${red.length} check${red.length === 1 ? "" : "s"} failed: ${red.slice(0, 3).map((c) => c.name).join(", ")}` });
  }
  // Each reviewer's latest decisive review (approve / changes) counts; comments don't change it.
  const decisive = new Map();
  for (const r of [...pr.reviews].sort((a, b) => (a.at ?? 0) - (b.at ?? 0)))
    if (["approved", "changes_requested", "dismissed"].includes(r.state)) decisive.set(r.author, r);
  const changes = [...decisive.values()].filter((r) => r.state === "changes_requested");
  if (mine && open && changes.length) out.push({ reason: "input", why: `changes requested by ${changes.map((r) => r.author).join(", ")}` });
  const at = new RegExp(`(^|[^\\w])@${me.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}(?![\\w-])`, "i");
  // A mention: GitHub says so (`mentioned`, no author); elsewhere, @me in a comment's body.
  const mention = pr.events.filter((e) => (e.at ?? 0) > seen && e.actor !== me &&
    ((e.kind === "mentioned" && e.target?.user === me) || (e.body && at.test(e.body)))).pop();
  if (mention) {
    const by = mention.actor ?? pr.events.filter((e) => e.body && at.test(e.body) && Math.abs((e.at ?? 0) - (mention.at ?? 0)) < 5000)[0]?.actor;
    out.push({ reason: "input", why: `mentioned${by ? ` by ${by}` : ""}` });
  }
  // Green: checks passed, nothing asks for changes, the forge doesn't block it (GitHub's mergeable_state).
  const approved = [...decisive.values()].some((r) => r.state === "approved");
  if (mine && it.state === "merged") out.push({ reason: "done", why: "merged" });
  else if (mine && open && pr.rollup === "success" && !changes.length && !it.draft && it.mergeable !== false && !it.blocked)
    out.push({ reason: "done", why: approved ? "approved and checks green: ready to merge" : "checks green: ready to merge" });
  return out;
}

export function summary(pr) {
  const count = (xs, k) => xs.reduce((m, x) => ((m[x[k]] = (m[x[k]] || 0) + 1), m), {});
  return {
    item: { ...pr.item, body: pr.item.body ? `${pr.item.body.length} chars` : null },
    reviews: count(pr.reviews, "state"), checks: count(pr.checks, "state"), check_sources: count(pr.checks, "source"),
    rollup: pr.rollup, forge_rollup: pr.forge_rollup, events: count(pr.events, "kind"),
    ...(pr.approvals ? { approvals: pr.approvals } : {}),
  };
}

// ---------------------------------------------------------------- CLI
if (import.meta.url === `file://${process.argv[1]}`) {
  const args = process.argv.slice(2);
  const opt = (k, d) => { const i = args.indexOf(k); return i >= 0 ? args[i + 1] : d; };
  const here = new URL(".", import.meta.url).pathname;
  const runs = args[0] === "--all"
    ? [ // each fixture, as the person it exercises a rule for (they're other people's PRs: "me" is hypothetical)
        ["forgejo-illogical-84", "forgejo", "jhgaylor", []],
        ["codeberg-forgejo-14667", "forgejo", "wetneb", []],
        ["codeberg-forgejo-14667", "forgejo", "mfenniak", []],
        ["codeberg-forgejo-14606", "forgejo", "Gusted", ["Reviewers"]],
        ["codeberg-forgejo-14606", "forgejo", "viceice-bot", []],
        ["github-cli-cli-14519", "github", "waldyrious", []],
        ["github-cli-cli-14519", "github", "williammartin", []],
        ["github-cli-cli-14519", "github", "BagToad", ["code-reviewers"]],
        ["codeberg-forgejo-14665", "forgejo", "Gusted", ["Reviewers"]],
        ["codeberg-forgejo-14665", "forgejo", "viceice-bot", []],
        ["codeberg-forgejo-14657", "forgejo", "Gusted", []],
        ["codeberg-forgejo-14657", "forgejo", "n0toose", []],
        ["github-cli-cli-13788", "github", "babakks", []],
        ["github-cli-cli-13788", "github", "happysnaker", []],
        ["github-cli-cli-13899", "github", "BagToad", []],
        ["github-cli-cli-13899", "github", "imkp1", []],
        ["gitlab-cli-3941", "gitlab", "Kxrma47", []],
        ["gitlab-cli-3941", "gitlab", "jhebden", []],
      ].map(([d, p, me, teams]) => [join(here, "fixtures", d), p, me, teams])
    : [[args[0], args[1], opt("--me", "jhgaylor"), (opt("--teams", "") || "").split(",").filter(Boolean)]];
  const seenDirs = new Set();
  for (const [dir, provider, me, teams] of runs) {
    const pr = adapters[provider](dir);
    if (!seenDirs.has(dir)) { seenDirs.add(dir); console.log(`\n# ${dir.split("/").pop()}`); console.log(JSON.stringify(summary(pr))); }
    console.log(`  as ${me}${teams.length ? ` (teams ${teams})` : ""}: ${JSON.stringify(attention(pr, me, teams))}`);
  }
  if (unmapped.size) console.log(`\nunmapped: ${[...unmapped].join("; ")}`);
}
