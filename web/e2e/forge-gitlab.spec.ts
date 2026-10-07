// M39: a GitLab merge request as a block, against a fake GitLab served here
// (S23's recorded gitlab-org/cli!3941, moved into a subgroup and opened
// again) and the stand-in `glab` the config puts on the daemon's PATH (this
// spec writes the host it knows).
//
// Opened from a pane's menu with the MR's link, its pipeline failed: the
// block offers Rerun, which retries the pipeline with glab's token. With no
// glab login the same MR reads anonymously: the block says it's read-only
// and offers no writes. Nothing here reaches a real forge.

import { readFileSync, rmSync, writeFileSync } from "node:fs";
import { createServer, type Server, type ServerResponse } from "node:http";
import { join } from "node:path";
import { expect, test, type Page } from "@playwright/test";
import { menu, paneEl, panes, reset } from "./helpers";
import { listen } from "./ports";

const TOKEN = "e2e-gitlab-token";
const REPO = "group/sub/proj";
const fixture = (f: string) =>
  JSON.parse(readFileSync(new URL(`../../crates/daemon/tests/fixtures/gitlab/gitlab-cli-3941/${f}`, import.meta.url), "utf8"));

let origin = "";
let server: Server;
let item: Record<string, any>;
let jobs: Record<string, any>[] = [];
const writes: { route: string; auth: string }[] = [];
const glabHost = () => join(process.env.ARUGULA_E2E_TEA_DIR!, "glab-host");

function json(res: ServerResponse, status: number, v: unknown) {
  res.writeHead(status, { "content-type": "application/json" }).end(JSON.stringify(v));
}

/** !3941, open again, Jake's, its integration tests red. */
function fresh() {
  item = fixture("item.json");
  Object.assign(item, { state: "opened", merged_at: null, merged_by: null, detailed_merge_status: "mergeable" });
  item.author.username = "jhgaylor";
  item.references.full = `${REPO}!3941`;
  item.web_url = `${origin}/${REPO}/-/merge_requests/3941`;
  item.head_pipeline.status = "failed";
  jobs = fixture("pipeline_jobs.json");
  jobs[8].status = "failed";
  writes.length = 0;
}

test.describe.configure({ mode: "serial" });

test.beforeAll(async () => {
  server = createServer((req, res) => {
    const u = new URL(req.url!, "http://forge");
    const auth = req.headers.authorization ?? "";
    const authed = auth === `Bearer ${TOKEN}`;
    const mr = `/api/v4/projects/${encodeURIComponent(REPO)}/merge_requests/3941`;
    const p = u.pathname;
    req.resume();
    req.on("end", () => {
      if (req.method === "GET") {
        if (p === "/api/v4/user") return authed ? json(res, 200, { id: 1, username: "jhgaylor" }) : json(res, 401, { message: "401 Unauthorized" });
        if (p === mr) return json(res, 200, item);
        if (p === `${mr}/reviewers`) return json(res, 200, []);
        if (p === `${mr}/approvals`) return json(res, 200, { approved_by: [] });
        if (p === `${mr}/discussions`) return authed ? json(res, 200, []) : json(res, 401, { message: "401 Unauthorized" });
        if (p.endsWith("/pipelines/2892163626/jobs")) return json(res, 200, jobs);
        return json(res, 404, { message: "404 Not Found" });
      }
      if (!authed) return json(res, 401, { message: "401 Unauthorized" });
      writes.push({ route: p, auth });
      if (p.endsWith("/pipelines/2892163626/retry")) {
        for (const j of jobs) if (j.status === "failed") j.status = "pending";
        item.head_pipeline.status = "running";
        item.head_pipeline.updated_at = "2026-10-03T04:00:00.000Z";
        return json(res, 201, { id: 2892163626, status: "running", web_url: `${origin}/${REPO}/-/pipelines/2892163626` });
      }
      json(res, 404, { message: "404 Not Found" });
    });
  });
  origin = `http://127.0.0.1:${await listen(server)}`;
});

test.afterAll(() => {
  server.close();
  rmSync(glabHost(), { force: true });
});

async function openMr(page: Page): Promise<number> {
  await reset(page);
  const term = (await panes(page))[0];
  await menu(page, paneEl(page, term), "Open pull request…");
  await page.locator(".prompt input").fill(`${origin}/${REPO}/-/merge_requests/3941`);
  await page.keyboard.press("Enter");
  await expect.poll(async () => (await panes(page)).length).toBe(2);
  return page.evaluate(() => window.__arugula.client.state!.panes.find((p) => p.type === "forge")!.id);
}

test("a merge request's failed pipeline is rerun from the block", async ({ page }) => {
  fresh();
  writeFileSync(glabHost(), origin.replace("http://", ""));
  const block = await openMr(page);
  const el = page.locator(`[data-forge-block="${block}"]`);
  await expect(el.locator(".review-path")).toContainText(`${REPO}#3941 fix(issue): reject invalid list output flags`);
  await expect(el.locator("[data-pr-state]")).toHaveText("open");
  await expect(el.locator("[data-check]")).toHaveCount(23);
  await expect(el.locator('[data-check="manual"]')).toHaveCount(3);
  await expect(el.locator(".forge-meta")).toContainText("as jhgaylor (glab, 127.0.0.1:");
  await expect(el.locator('[data-want="failed"]')).toContainText("1 check failed: tests:integration");
  await expect(el.locator("[data-forge-read-only]")).toHaveCount(0);
  await el.locator("[data-rerun]").click();
  await expect.poll(() => writes.length).toBe(1);
  expect(writes[0]).toEqual({ route: "/api/v4/projects/34675721/pipelines/2892163626/retry", auth: `Bearer ${TOKEN}` });
  // Running again: nothing waits on you.
  await expect(el.locator('[data-want="failed"]')).toHaveCount(0);
  await expect(el.locator(".forge-check.running")).toBeVisible();
});

test("with no glab login it reads anonymously and offers no writes", async ({ page }) => {
  fresh();
  rmSync(glabHost(), { force: true });
  const block = await openMr(page);
  const el = page.locator(`[data-forge-block="${block}"]`);
  await expect(el.locator(".review-path")).toContainText(`${REPO}#3941`);
  await expect(el.locator("[data-forge-read-only]")).toContainText("read-only: no glab login for 127.0.0.1:");
  await expect(el.locator(".forge-meta")).toContainText("anonymously, read-only");
  await expect(el.locator("[data-check]")).toHaveCount(23);
  await expect(el.locator("[data-rerun]")).toHaveCount(0);
  await expect(el.locator(".forge-actions button", { hasText: "Comment" })).toHaveCount(0);
  expect(writes).toEqual([]);
});
