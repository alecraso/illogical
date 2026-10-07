// M38: a GitHub pull request as a block, against a fake GitHub served here
// (S23's recorded cli/cli#13788: three builds red, now by jhgaylor) and the
// stand-in `gh` the config puts on the daemon's PATH.
//
// The block shows the check runs and that they failed, with *Rerun checks*
// (GitHub can; Forgejo can't); clicking it posts `rerun-failed-jobs` for
// the red workflow run with gh's token, and the jobs queue again. Polls
// are conditional: the fake sees `If-None-Match` and answers 304. Nothing
// here reaches GitHub.

import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { createHash } from "node:crypto";
import { createServer, type Server, type ServerResponse } from "node:http";
import { expect, test } from "@playwright/test";
import { reset } from "./helpers";
import { listen } from "./ports";

const TOKEN = "e2e-github-token";
const REPO = "cli/cli";
const N = 13788;
const fixture = (f: string) =>
  JSON.parse(readFileSync(new URL(`../../crates/daemon/tests/fixtures/github/github-cli-cli-13788/${f}`, import.meta.url), "utf8"));

let origin = "";
let server: Server;
let item: Record<string, any>;
let runs: Record<string, any>[] = [];
const writes: { route: string; auth: string }[] = [];
let notModified = 0;

function answer(res: ServerResponse, inm: string | undefined, v: unknown) {
  const body = JSON.stringify(v);
  const etag = `"${createHash("sha1").update(body).digest("hex")}"`;
  const h = { etag, "x-ratelimit-limit": "5000", "x-ratelimit-remaining": "4900", "x-ratelimit-resource": "core" };
  if (inm === etag) {
    notModified += 1;
    return res.writeHead(304, h).end();
  }
  res.writeHead(200, { "content-type": "application/json", ...h }).end(body);
}

test.beforeAll(async () => {
  item = fixture("item.json");
  item.user.login = "jhgaylor";
  item.requested_reviewers = [];
  runs = fixture("check_runs.json").check_runs;
  server = createServer((req, res) => {
    const u = new URL(req.url!, "http://github");
    if (req.headers.authorization !== `Bearer ${TOKEN}`) return res.writeHead(401).end('{"message":"Bad credentials"}');
    const inm = req.headers["if-none-match"] as string | undefined;
    const p = u.pathname.replace(`/repos/${REPO}`, "");
    req.resume();
    req.on("end", () => {
      if (req.method === "POST") {
        writes.push({ route: p, auth: req.headers.authorization! });
        for (const c of runs) if (c.conclusion === "failure") Object.assign(c, { status: "queued", conclusion: null });
        return res.writeHead(201).end();
      }
      if (u.pathname === "/user") return answer(res, inm, { login: "jhgaylor" });
      if (u.pathname === "/user/teams") return answer(res, inm, []);
      if (p === `/pulls/${N}`) return answer(res, inm, item);
      if (p.endsWith("/check-runs")) return answer(res, inm, { total_count: runs.length, check_runs: runs });
      if (p.endsWith("/status")) return answer(res, inm, { state: "pending", total_count: 0, statuses: [] });
      if (p === `/pulls/${N}/reviews`) return answer(res, inm, fixture("reviews.json"));
      if (p === `/issues/${N}/timeline`) return answer(res, inm, fixture("timeline.json"));
      res.writeHead(404).end('{"message":"Not Found"}');
    });
  });
  origin = `http://127.0.0.1:${await listen(server)}`;
  // The stand-in gh has a login for it (and no other host).
  writeFileSync(join(process.env.ARUGULA_E2E_TEA_DIR!, "gh-hosts"), `${origin.replace("http://", "")}\n`);
});

test.afterAll(() => {
  server.close();
  writeFileSync(join(process.env.ARUGULA_E2E_TEA_DIR!, "gh-hosts"), "");
});

test("red check runs on your GitHub PR rerun from the block", async ({ page }) => {
  await reset(page);
  const block = await page.evaluate(
    ([api, repo, number]) =>
      window.__arugula.client
        .request("POST", "/api/blocks", { type: "forge", config: { provider: "github", api, repo, number }, local: true })
        .then((r) => r.json<{ block: number }>())
        .then((r) => r.block),
    [origin, REPO, N] as const,
  );
  const el = page.locator(`[data-forge-block="${block}"]`);
  await expect(el.locator(".review-path")).toContainText(`${REPO}#${N} fix: preserve percent-encoded path`);
  await expect(el.locator("[data-check]")).toHaveCount(11);
  await expect(el.locator('[data-want="failed"]')).toContainText("3 checks failed: build (ubuntu-latest)");
  await expect(el.locator(".forge-meta")).toContainText("as jhgaylor (gh on");
  // On the rail too, with a rerun.
  await expect
    .poll(() => page.evaluate((b) => window.__arugula.client.info(b)?.reason?.actions ?? [], block))
    .toEqual(["rerun", "dismiss"]);
  // Polls are conditional: unchanged, they're 304s.
  await expect.poll(() => notModified).toBeGreaterThan(3);

  await el.locator("[data-rerun]").click();
  await expect.poll(() => writes.length).toBe(1);
  expect(writes[0]).toEqual({ route: "/actions/runs/28656994029/rerun-failed-jobs", auth: `Bearer ${TOKEN}` });
  // The jobs queue again: no longer failed.
  await expect(el.locator('[data-want="failed"]')).toHaveCount(0);
  await expect(el.locator('[data-check="queued"]')).toHaveCount(3);
});
