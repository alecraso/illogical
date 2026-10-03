#!/usr/bin/env node
// S23: one full read of a PR, then unchanged polls, through a forge's CLI or plain HTTP.
//
//   node read.mjs github  cli/cli 14519            --via cli|http [--polls 3] [--save DIR]
//   node read.mjs forgejo jhgaylor/illogical 84    --via cli|http --host git.inevitable.fyi --login forgejo
//   node read.mjs forgejo forgejo/forgejo 14667    --via http --host codeberg.org     (no login: anonymous)
//   node read.mjs gitlab  gitlab-org/cli 3941      --via http                          (anonymous)
//
// Tokens are only ever held in memory (gh auth token / tea's config.yml) and never printed or saved.
// --save writes response bodies and a short list of response headers (no auth, no cookies).
// Read-only: every request is a GET.

import { execFileSync, spawn } from "node:child_process";
import { readFileSync, mkdirSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

const [provider, repo, numArg, ...rest] = process.argv.slice(2);
const opt = (k, d) => { const i = rest.indexOf(k); return i >= 0 ? rest[i + 1] : d; };
const via = opt("--via", "http");
const polls = Number(opt("--polls", "2"));
const save = opt("--save", null);
const login = opt("--login", null);
const host = opt("--host", provider === "github" ? "api.github.com" : provider === "gitlab" ? "gitlab.com" : null);
const n = Number(numArg);

const KEEP = /^(etag|last-modified|x-ratelimit-.*|ratelimit-.*|x-total-count|x-total|x-total-pages|x-next-page|link|cache-control|x-poll-interval|x-github-media-type)$/i;

function token() {
  if (provider === "github") return execFileSync("gh", ["auth", "token"], { encoding: "utf8" }).trim();
  if (provider === "forgejo" && login) {
    // tea keeps logins in ~/.config/tea/config.yml; the daemon would parse the YAML properly.
    const y = readFileSync(join(process.env.XDG_CONFIG_HOME || join(homedir(), ".config"), "tea/config.yml"), "utf8");
    const block = y.split(/\n\s*- name: /).find((b) => b.startsWith(login + "\n"));
    const m = block && block.match(/\n\s*token: (\S+)/);
    if (!m) throw new Error(`no token for tea login ${login}`);
    return m[1];
  }
  return null; // anonymous (Codeberg, gitlab.com)
}
const TOKEN = via === "http" ? token() : null;

function base() {
  if (provider === "github") return "https://api.github.com/";
  if (provider === "forgejo") return `https://${host}/api/v1/`;
  return `https://${host}/api/v4/`;
}

// One request. Returns {status, ms, bytes, headers, body}.
async function get(path, etag) {
  const t0 = performance.now();
  if (via === "http") {
    const h = { "User-Agent": "illogical-s23-spike", Accept: provider === "github" ? "application/vnd.github+json" : "application/json" };
    if (TOKEN) h.Authorization = provider === "gitlab" ? `Bearer ${TOKEN}` : `token ${TOKEN}`;
    if (etag) h["If-None-Match"] = etag;
    const r = await fetch(base() + path, { headers: h });
    const body = await r.text();
    const headers = {};
    for (const [k, v] of r.headers) if (KEEP.test(k)) headers[k.toLowerCase()] = v;
    return { status: r.status, ms: performance.now() - t0, bytes: body.length, headers, body };
  }
  // via the CLI's own passthrough
  const args = provider === "github"
    ? ["api", "-i", ...(etag ? ["-H", `If-None-Match: ${etag}`] : []), path]
    : ["api", "-i", ...(login ? ["-l", login] : []), ...(etag ? ["-H", `If-None-Match:${etag}`] : []), path];
  const bin = provider === "github" ? "gh" : provider === "forgejo" ? "tea" : "glab";
  const { out, err } = await new Promise((res) => {
    const p = spawn(bin, args, { stdio: ["ignore", "pipe", "pipe"] });
    let out = "", err = "";
    p.stdout.on("data", (d) => (out += d)); p.stderr.on("data", (d) => (err += d));
    p.on("close", () => res({ out, err }));
  });
  // gh prints status + headers on stdout before the body; tea prints them on stderr.
  const head = provider === "github" ? out.slice(0, out.search(/\r?\n\r?\n/)) : err;
  const body = provider === "github" ? out.slice(head.length).trimStart() : out;
  const status = Number((head.match(/HTTP\/[\d.]+ (\d+)/) || [])[1] || 0);
  const headers = {};
  for (const line of head.split(/\r?\n/)) {
    const m = line.match(/^([\w-]+):\s*(.*)$/);
    if (m && KEEP.test(m[1])) headers[m[1].toLowerCase()] = m[2];
  }
  return { status, ms: performance.now() - t0, bytes: body.length, headers, body };
}

const enc = encodeURIComponent;
// The full read: the item first (for the head sha), then the rest in parallel.
function plan(item) {
  if (provider === "github") {
    const sha = item.head.sha;
    return {
      reviews: `repos/${repo}/pulls/${n}/reviews?per_page=100`,
      review_comments: `repos/${repo}/pulls/${n}/comments?per_page=100`,
      check_runs: `repos/${repo}/commits/${sha}/check-runs?per_page=100`,
      statuses: `repos/${repo}/commits/${sha}/status`,
      timeline: `repos/${repo}/issues/${n}/timeline?per_page=100`,
      files: `repos/${repo}/pulls/${n}/files?per_page=100`,
    };
  }
  if (provider === "forgejo") {
    const sha = item.head.sha;
    return {
      reviews: `repos/${repo}/pulls/${n}/reviews?limit=50`,
      statuses: `repos/${repo}/commits/${sha}/status`,
      timeline: `repos/${repo}/issues/${n}/timeline?limit=50`,
      files: `repos/${repo}/pulls/${n}/files?limit=50`,
      action_runs: `repos/${repo}/actions/runs?head_sha=${sha}`,
    };
  }
  const p = `projects/${enc(repo)}/merge_requests/${n}`;
  return {
    approvals: `${p}/approvals`,
    pipelines: `${p}/pipelines`,
    discussions: `${p}/discussions?per_page=100`,
    diffs: `${p}/diffs?per_page=100`,
  };
}
const itemPath = provider === "gitlab" ? `projects/${enc(repo)}/merge_requests/${n}` : `repos/${repo}/pulls/${n}`;

async function rate() {
  if (provider !== "github") return null;
  const r = await get("rate_limit");
  try { return JSON.parse(r.body).resources.core.used; } catch { return null; }
}

function saveAll(tag, results) {
  if (!save) return;
  mkdirSync(save, { recursive: true });
  const meta = {};
  for (const [name, r] of Object.entries(results)) {
    meta[name] = { status: r.status, ms: Math.round(r.ms), bytes: r.bytes, headers: r.headers };
    if (r.status === 200 && tag === "full") {
      let body = r.body;
      try { body = JSON.stringify(JSON.parse(body), null, 1); } catch {}
      writeFileSync(join(save, `${name}.json`), body);
    }
  }
  writeFileSync(join(save, `_${tag}.headers.json`), JSON.stringify(meta, null, 1));
}

const summary = { provider, repo, n, via, host, runs: [] };
const used0 = await rate();
let t0 = performance.now();
const item = await get(itemPath);
if (item.status !== 200) { console.error("item", item.status, item.body.slice(0, 300)); process.exit(1); }
const parsed = JSON.parse(item.body);
const paths = plan(parsed);
const names = Object.keys(paths);
const rs = await Promise.all(names.map((k) => get(paths[k])));
const full = { item, ...Object.fromEntries(names.map((k, i) => [k, rs[i]])) };
const fullMs = performance.now() - t0;
const used1 = await rate();
summary.runs.push({
  kind: "full", wall_ms: Math.round(fullMs), requests: 1 + names.length,
  bytes: Object.values(full).reduce((a, r) => a + r.bytes, 0),
  statuses: Object.fromEntries(Object.entries(full).map(([k, r]) => [k, r.status])),
  etags: Object.values(full).filter((r) => r.headers.etag).length,
  rate_used: used1 != null ? used1 - used0 : undefined,
  per_request_ms: Object.fromEntries(Object.entries(full).map(([k, r]) => [k, Math.round(r.ms)])),
});
saveAll("full", full);

// Unchanged polls: every request again, with If-None-Match where we got an ETag.
const all = { item: itemPath, ...paths };
for (let i = 0; i < polls; i++) {
  const u0 = await rate();
  t0 = performance.now();
  const keys = Object.keys(all);
  const pr = await Promise.all(keys.map((k) => get(all[k], full[k].headers.etag)));
  const ms = performance.now() - t0;
  const u1 = await rate();
  const res = Object.fromEntries(keys.map((k, j) => [k, pr[j]]));
  summary.runs.push({
    kind: "poll", wall_ms: Math.round(ms), requests: keys.length,
    bytes: pr.reduce((a, r) => a + r.bytes, 0),
    statuses: Object.fromEntries(keys.map((k, j) => [k, pr[j].status])),
    rate_used: u1 != null ? u1 - u0 : undefined,
  });
  if (i === 0) saveAll("poll", res);
  // The cheapest change check: the item alone, conditional.
  const c0 = await rate();
  const one = await get(itemPath, full.item.headers.etag);
  const c1 = await rate();
  summary.runs.push({ kind: "poll-item-only", wall_ms: Math.round(one.ms), requests: 1, status: one.status, rate_used: c1 != null ? c1 - c0 : undefined });
}
console.log(JSON.stringify(summary, null, 1));
