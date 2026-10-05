// M37: an issue as a block, against a fake Forgejo served here (Codeberg's
// recorded #14556, moved onto this repo and changed as each test needs),
// the stand-in `tea` the config puts on the daemon's PATH (this spec
// writes its logins), and the stand-in Claude Code (the config's fake ACP
// agent). Nothing here reaches a real forge or a real agent.
//
// Opened from a pane's menu ("Open issue…"), the block shows the issue,
// what waits on you (it was given to you) and the PRs that refer to it. An
// agent's new issue is a draft on its own block, edited and sent from the
// card. *Agent on this* makes a branch and worktree from main in a clone,
// starts the agent there with the issue as its prompt, puts the two in a
// tab of their own, and when the fake lists a PR from that branch, its PR
// block joins them.

import { execFileSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createServer, type Server, type ServerResponse } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test, type Page } from "@playwright/test";
import { menu, paneEl, panes, reset } from "./helpers";
import { listen } from "./ports";

const TOKEN = "e2e-forge-token";
const REPO = "jhgaylor/illogical";
const N = 14556;
const fixture = (dir: string, f: string) =>
  JSON.parse(readFileSync(new URL(`../../crates/daemon/tests/fixtures/forgejo/${dir}/${f}`, import.meta.url), "utf8"));

let origin = "";
let server: Server;
let dir = "";
let issues: Record<number, Record<string, any>> = {};
let timelines: Record<number, Record<string, unknown>[]> = {};
let pulls: Record<string, any>[] = [];
const writes: { route: string; body: any; auth: string }[] = [];

function json(res: ServerResponse, status: number, v: unknown, headers: Record<string, string> = {}) {
  res.writeHead(status, { "content-type": "application/json", ...headers }).end(JSON.stringify(v));
}

/** #14556 on this repo: open, given to jhgaylor by sam, a linked PR. */
function fresh() {
  const it = fixture("codeberg-forgejo-14556", "item.json");
  Object.assign(it, { title: "Add a frobnicator to the CLI", body: "The CLI needs a `frobnicate` command.", html_url: `${origin}/${REPO}/issues/${N}`, assignees: [{ login: "jhgaylor" }] });
  issues = { [N]: it };
  const tl = (fixture("codeberg-forgejo-14556", "timeline.json") as Record<string, any>[]).filter((e) => e.type !== "comment");
  for (const e of tl) if (e.type === "assignees") Object.assign(e, { user: { login: "sam" }, assignee: { login: "jhgaylor" } });
  timelines = { [N]: tl };
  pulls = [];
  writes.length = 0;
}

const git = (cwd: string, ...args: string[]) => execFileSync("git", args, { cwd, encoding: "utf8" }).trim();

test.describe.configure({ mode: "serial" });

test.beforeAll(async () => {
  server = createServer((req, res) => {
    const u = new URL(req.url!, "http://forge");
    if (req.headers.authorization !== `token ${TOKEN}`) return json(res, 401, { message: "token is required" });
    const p = u.pathname.replace(`/api/v1/repos/${REPO}`, "");
    let body = "";
    req.on("data", (d) => (body += d));
    req.on("end", () => {
      let m: RegExpMatchArray | null;
      if (req.method === "GET") {
        if (u.pathname === "/api/v1/user") return json(res, 200, { id: 1, login: "jhgaylor" });
        if (u.pathname === "/api/v1/user/teams") return json(res, 200, []);
        if (p === "") return json(res, 200, { full_name: REPO, default_branch: "main", ssh_url: `ssh://git@git.example/${REPO}.git` });
        if ((m = p.match(/^\/issues\/(\d+)$/))) return issues[+m[1]] ? json(res, 200, issues[+m[1]]) : json(res, 404, { message: "no issue" });
        if ((m = p.match(/^\/issues\/(\d+)\/timeline$/))) {
          const tl = timelines[+m[1]] ?? [];
          return json(res, 200, tl, { "x-total-count": String(tl.length) });
        }
        if (p === "/pulls") return json(res, 200, pulls);
        if ((m = p.match(/^\/pulls\/(\d+)$/))) {
          const pr = pulls.find((x) => x.number === +m![1]);
          return pr ? json(res, 200, pr) : json(res, 404, { message: "no pull" });
        }
        if (p.match(/^\/pulls\/\d+\/reviews$/)) return json(res, 200, []);
        if (p.startsWith("/commits/") && p.endsWith("/status")) return json(res, 200, { state: "pending", total_count: 0, statuses: [] });
        return json(res, 404, { message: "not here" });
      }
      const b = JSON.parse(body || "{}");
      writes.push({ route: p, body: b, auth: req.headers.authorization! });
      if (p === "/issues") {
        const n = 15000 + writes.length;
        const it = { ...fixture("codeberg-forgejo-14556", "item.json"), number: n, title: b.title, body: b.body, assignees: [], labels: [], comments: 0, html_url: `${origin}/${REPO}/issues/${n}` };
        it.user = { login: "jhgaylor" };
        issues[n] = it;
        return json(res, 201, { number: n, html_url: it.html_url });
      }
      json(res, 404, { message: "not here" });
    });
  });
  origin = `http://127.0.0.1:${await listen(server)}`;
  const tea = process.env.ILLOGICAL_E2E_TEA_DIR!;
  writeFileSync(join(tea, "logins.json"), JSON.stringify([{ name: "e2e", url: origin, ssh_host: "", user: "jhgaylor", default: "false" }]));
  // main on a remote, and the person's clone of it.
  dir = mkdtempSync(join(tmpdir(), "ilg-e2e-issue-"));
  const work = join(dir, "work");
  execFileSync("git", ["init", "-q", "-b", "main", work]);
  git(work, "config", "user.email", "t@example.com");
  git(work, "config", "user.name", "t");
  writeFileSync(join(work, "a.txt"), "one\n");
  git(work, "add", ".");
  git(work, "commit", "-qm", "base");
  git(dir, "clone", "-q", "--bare", work, join(dir, "remote.git"));
  git(dir, "clone", "-q", join(dir, "remote.git"), join(dir, "clone"));
});

test.afterAll(() => {
  server.close();
  writeFileSync(join(process.env.ILLOGICAL_E2E_TEA_DIR!, "logins.json"), "[]");
  if (dir) rmSync(dir, { recursive: true, force: true });
});

const info = (page: Page, id: number) => page.evaluate((b) => window.__illogical.client.state!.panes.find((p) => p.id === b) ?? null, id);
const tabOf = (page: Page, id: number) => page.evaluate((b) => window.__illogical.client.tabOfPane(b)?.id ?? null, id);

test("an issue opens from the menu, given to you; an agent's new issue is a draft sent from its card", async ({ page }) => {
  fresh();
  await reset(page);
  const term = (await panes(page))[0];
  await menu(page, paneEl(page, term), "Open issue…");
  await page.locator(".prompt input").fill(`${origin}/${REPO}/issues/${N}`);
  await page.keyboard.press("Enter");
  await expect.poll(async () => (await panes(page)).length).toBe(2);
  const block = await page.evaluate(() => window.__illogical.client.state!.panes.find((p) => p.type === "forge")!.id);
  const el = page.locator(`[data-forge-block="${block}"]`);
  await expect(el).toHaveAttribute("data-forge-kind", "issue");
  await expect(el.locator(".review-path")).toContainText(`${REPO}#${N} Add a frobnicator to the CLI`);
  await expect(el.locator("[data-issue-state]")).toHaveText("open");
  // Looking at it, what's there is seen; then sam gives it to you.
  await expect(el.locator('[data-want="assigned"]')).toHaveCount(0);
  timelines[N].push({ id: 99001, type: "assignees", created_at: "2099-01-01T00:00:00Z", user: { login: "sam" }, assignee: { login: "jhgaylor" }, removed_assignee: false });
  issues[N].updated_at = "2026-10-03T05:00:00Z";
  await expect(el.locator('[data-want="assigned"]')).toContainText("assigned to you by sam");
  await expect.poll(async () => (await info(page, block))?.reason?.kind).toBe("input");
  await expect(el.locator('[data-linked="14571"]')).toContainText("enh: add org members");
  // Opened from a link with no clone of ours: the owner still gets the button.
  await expect(el.locator("[data-agent-on]")).toBeVisible();

  // An agent's new issue: a draft on a block of its own, on the card.
  const r = await page.evaluate(
    ([repo, t]) =>
      window.__illogical.client
        .request("POST", "/api/blocks", { type: "forge", config: { issue: "new", repo, title: "Frobs leak", body: "Memory grows.", agent: true }, split: t, local: true })
        .then((r) => r.json<{ block: number }>()),
    [REPO, term] as const,
  );
  const draft = page.locator(`[data-forge-block="${r.block}"]`);
  await expect(draft).toHaveAttribute("data-forge-new", "waiting");
  const card = page.locator('.pane-ask .ask[data-ask="new"]');
  await expect(card).toContainText(`an agent drafted a new issue on ${REPO}`);
  await expect(card.locator('input[name="title"]')).toHaveValue("Frobs leak");
  await expect(card.locator('textarea[name="body"]')).toHaveValue("Memory grows.");
  expect(writes).toEqual([]);
  await card.locator('input[name="title"]').fill("Frobs leak memory");
  await card.locator("[data-ask-submit]").click();
  await expect.poll(() => writes.length).toBe(1);
  expect(writes[0]).toEqual({ route: "/issues", body: { title: "Frobs leak memory", body: "Memory grows." }, auth: `token ${TOKEN}` });
  // The block is the issue now.
  await expect(draft).toHaveAttribute("data-forge-kind", "issue");
  await expect(draft.locator(".review-path")).toContainText("Frobs leak memory");
  await expect(card).toHaveCount(0);
});

test("Agent on this: the issue and its agent in a tab, on a branch of their own, and its PR joins them", async ({ page }) => {
  fresh();
  await reset(page);
  const clone = join(dir, "clone");
  const block = (await page.evaluate(
    ([repo, n, clone, api]) =>
      window.__illogical.client.openBlock({ type: "forge", config: { repo, number: n, kind: "issue", dir: clone, api, login: "e2e" }, local: true }),
    [REPO, N, clone, `${origin}/api/v1`] as const,
  ))!;
  const el = page.locator(`[data-forge-block="${block}"]`);
  await expect(el.locator("[data-issue-state]")).toHaveText("open");
  await el.locator("[data-agent-on]").click();

  const branch = "i14556-add-a-frobnicator-to-the";
  await expect(el.locator("[data-agent-link]")).toContainText(branch);
  const agent = await page.evaluate(() => window.__illogical.client.state!.panes.find((p) => p.type === "agent")?.id ?? null);
  expect(agent).not.toBeNull();
  // One tab, named for the issue.
  expect(await tabOf(page, agent!)).toBe(await tabOf(page, block));
  await expect
    .poll(() => page.evaluate((b) => window.__illogical.client.tabOfPane(b)?.name ?? null, block))
    .toBe(`#${N}`);
  // The worktree, on its branch from main, and the agent there with the issue.
  const wt = join(clone, ".illogical/worktrees", branch);
  expect(existsSync(join(wt, ".git"))).toBe(true);
  expect(git(wt, "rev-parse", "--abbrev-ref", "HEAD")).toBe(branch);
  await expect
    .poll(() =>
      page.evaluate(
        (a) =>
          window.__illogical.client
            .request("GET", `/api/blocks/${a}`)
            .then((r) => r.json<{ state: { entries: { type: string; text?: string }[] } }>())
            .then((v) => v.state.entries.find((e) => e.type === "user")?.text ?? ""),
        agent!,
      ),
    )
    .toContain(`<issue-text>\nTitle: Add a frobnicator to the CLI`);
  await expect(el.locator("[data-agent-pr-waiting]")).toBeVisible();

  // The agent's PR appears on the forge: its block joins the tab.
  const pr = fixture("forgejo-illogical-84", "item.json");
  Object.assign(pr, { number: 91, state: "open", merged: false, merged_at: null, html_url: `${origin}/${REPO}/pulls/91`, title: "Add a frobnicator" });
  pr.head.ref = branch;
  pr.head.repo.full_name = REPO;
  pulls = [pr];
  await expect(el.locator('[data-agent-pr="91"]')).toBeVisible({ timeout: 15_000 });
  const prBlock = await page.evaluate(
    (b) => window.__illogical.client.state!.panes.find((p) => p.type === "forge" && p.id !== b)?.id ?? null,
    block,
  );
  expect(prBlock).not.toBeNull();
  expect(await tabOf(page, prBlock!)).toBe(await tabOf(page, block));
  await expect(page.locator(`[data-forge-block="${prBlock}"] .review-path`)).toContainText(`${REPO}#91 Add a frobnicator`);
  expect(writes).toEqual([]);
});
