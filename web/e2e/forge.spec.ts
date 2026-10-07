// M36: a pull request as a block, against a fake Forgejo served here (S23's
// recorded #84, changed as each test needs) and the stand-in `tea` the
// config puts on the daemon's PATH (this spec writes its logins).
//
// Opened from a pane's menu ("Open pull request…"), the block shows the PR;
// a review asked of you waits first and is approved there, reaching the
// forge as a review with your login's token. An agent's comment is a
// draft: a card on the block with the text in a box to edit, sent from
// here (the edited text is what's posted), and the block says who sent it.
// Nothing here reaches a real forge.

import { readFileSync, writeFileSync } from "node:fs";
import { createServer, type Server, type ServerResponse } from "node:http";
import { join } from "node:path";
import { expect, test, type Page } from "@playwright/test";
import { menu, paneEl, panes, reset } from "./helpers";
import { listen } from "./ports";

const TOKEN = "e2e-forge-token";
const REPO = "jhgaylor/illogical";
const fixture = (f: string) =>
  JSON.parse(readFileSync(new URL(`../../crates/daemon/tests/fixtures/forgejo/forgejo-illogical-84/${f}`, import.meta.url), "utf8"));

let origin = "";
let server: Server;
let item: Record<string, any>;
let reviews: Record<string, unknown>[] = [];
let timeline: Record<string, unknown>[] = [];
const writes: { route: string; body: any; auth: string }[] = [];
let n = 0;

function json(res: ServerResponse, status: number, v: unknown, headers: Record<string, string> = {}) {
  res.writeHead(status, { "content-type": "application/json", ...headers }).end(JSON.stringify(v));
}

function touch() {
  n += 1;
  item.updated_at = `2026-10-03T03:${String(Math.floor(n / 60) % 60).padStart(2, "0")}:${String(n % 60).padStart(2, "0")}Z`;
}

/** #84, open again, by someone else, a review asked of jhgaylor. */
function fresh() {
  item = fixture("item.json");
  Object.assign(item, { state: "open", merged: false, merged_at: null, html_url: `${origin}/${REPO}/pulls/84` });
  item.user.login = "sam";
  item.requested_reviewers = [{ login: "jhgaylor" }];
  reviews = [{ id: 1, state: "REQUEST_REVIEW", user: { login: "jhgaylor" }, submitted_at: "2026-10-02T23:40:00Z" }];
  timeline = fixture("timeline.json").slice(0, 1);
  writes.length = 0;
  touch();
}

test.describe.configure({ mode: "serial" });

test.beforeAll(async () => {
  server = createServer((req, res) => {
    const u = new URL(req.url!, "http://forge");
    if (req.headers.authorization !== `token ${TOKEN}`) return json(res, 401, { message: "token is required" });
    const p = u.pathname.replace(`/api/v1/repos/${REPO}`, "");
    let body = "";
    req.on("data", (d) => (body += d));
    req.on("end", () => {
      if (req.method === "GET") {
        if (u.pathname === "/api/v1/user") return json(res, 200, { id: 1, login: "jhgaylor" });
        if (u.pathname === "/api/v1/user/teams") return json(res, 200, []);
        if (p === "/pulls/84") return json(res, 200, item);
        if (p === "/pulls/84/reviews") return json(res, 200, reviews);
        if (p.startsWith("/commits/") && p.endsWith("/status")) return json(res, 200, { state: "success", total_count: 2, statuses: fixture("statuses.json").statuses });
        if (p === "/issues/84/timeline") return json(res, 200, timeline, { "x-total-count": String(timeline.length) });
        return json(res, 404, { message: "not here" });
      }
      const b = JSON.parse(body || "{}");
      writes.push({ route: p, body: b, auth: req.headers.authorization! });
      n += 1;
      if (p === "/pulls/84/reviews") {
        reviews.push({ id: 100 + n, state: b.event, user: { login: "jhgaylor" }, body: b.body, submitted_at: "2026-10-03T02:00:00Z" });
        touch();
        return json(res, 200, { id: 100 + n, html_url: `${origin}/${REPO}/pulls/84#issuecomment-${100 + n}` });
      }
      if (p === "/issues/84/comments") {
        timeline.push({ id: 7000 + n, type: "comment", created_at: "2026-10-03T02:00:00Z", user: { login: "jhgaylor" }, body: b.body });
        item.comments += 1;
        touch();
        return json(res, 201, { id: 7000 + n, html_url: `${origin}/${REPO}/pulls/84#issuecomment-${7000 + n}` });
      }
      json(res, 404, { message: "not here" });
    });
  });
  origin = `http://127.0.0.1:${await listen(server)}`;
  const tea = process.env.ARUGULA_E2E_TEA_DIR!;
  writeFileSync(join(tea, "logins.json"), JSON.stringify([{ name: "e2e", url: origin, ssh_host: "", user: "jhgaylor", default: "false" }]));
});

test.afterAll(() => {
  server.close();
  writeFileSync(join(process.env.ARUGULA_E2E_TEA_DIR!, "logins.json"), "[]");
});

async function openPr(page: Page): Promise<number> {
  await reset(page);
  const term = (await panes(page))[0];
  await menu(page, paneEl(page, term), "Open pull request…");
  await page.locator(".prompt input").fill(`${origin}/${REPO}/pulls/84`);
  await page.keyboard.press("Enter");
  await expect.poll(async () => (await panes(page)).length).toBe(2);
  return page.evaluate(() => window.__arugula.client.state!.panes.find((p) => p.type === "forge")!.id);
}

test("a pull request opens from the menu; the review asked of you is approved there", async ({ page }) => {
  fresh();
  const block = await openPr(page);
  const el = page.locator(`[data-forge-block="${block}"]`);
  await expect(el.locator(".review-path")).toContainText(`${REPO}#84 CI: delete the kept build`);
  await expect(el.locator("[data-pr-state]")).toHaveText("open");
  await expect(el.locator("[data-check]")).toHaveCount(2);
  // What waits on you comes first, and it's on the rail too.
  await expect(el.locator('[data-want="review"]')).toContainText("review requested from you");
  await expect.poll(() => page.evaluate((b) => window.__arugula.client.info(b)?.reason?.kind, block)).toBe("gate");
  await el.locator("[data-approve-review]").click();
  await expect.poll(() => writes.length).toBe(1);
  expect(writes[0]).toEqual({ route: "/pulls/84/reviews", body: { event: "APPROVED", body: "" }, auth: `token ${TOKEN}` });
  // Read back: approved, and no longer asked.
  await expect(el.locator('[data-review="approved"]')).toBeVisible();
  await expect(el.locator('[data-want="review"]')).toHaveCount(0);
  await expect.poll(() => page.evaluate((b) => window.__arugula.client.info(b)?.reason ?? null, block)).toBeNull();
});

test("an agent's comment waits as a draft until a person edits and sends it", async ({ page }) => {
  fresh();
  const block = await openPr(page);
  const el = page.locator(`[data-forge-block="${block}"]`);
  await expect(el.locator(".review-path")).toContainText("#84");
  // An agent's write (the CLI under Claude Code sends agent: true).
  const r = await page.evaluate(
    (b) => window.__arugula.client.request("POST", `/api/blocks/${b}/call/comment`, { body: "LGTM, one nit", agent: true }).then((r) => r.json<{ draft: string }>()),
    block,
  );
  expect(r.draft).toBeTruthy();
  expect(writes).toEqual([]);
  // The card: who drafted what, the text in a box to edit.
  const card = page.locator(`.pane-ask .ask[data-ask="${r.draft}"]`);
  await expect(card).toContainText("an agent drafted a comment on jhgaylor/illogical#84");
  const box = card.locator('textarea[name="body"]');
  await expect(box).toHaveValue("LGTM, one nit");
  await expect(el.locator("[data-forge-drafts-waiting]")).toContainText("A draft waits");
  await box.fill("LGTM, one small nit.\n\nThanks!");
  await card.locator("[data-ask-submit]").click();
  await expect.poll(() => writes.length).toBe(1);
  expect(writes[0].route).toBe("/issues/84/comments");
  expect(writes[0].body).toEqual({ body: "LGTM, one small nit.\n\nThanks!" });
  expect(writes[0].auth).toBe(`token ${TOKEN}`);
  // The block says who sent it, and the comment's in its timeline.
  await expect(el.locator(`[data-draft="${r.draft}"]`)).toHaveAttribute("data-draft-status", "sent");
  await expect(el.locator(`[data-draft="${r.draft}"]`)).toContainText("sent by");
  await expect(el.locator("[data-forge-timeline]")).toContainText("LGTM, one small nit.");
  await expect(card).toHaveCount(0);

  // Another, dropped: nothing goes.
  const d = await page.evaluate(
    (b) => window.__arugula.client.request("POST", `/api/blocks/${b}/call/merge`, { agent: true }).then((r) => r.json<{ draft: string }>()),
    block,
  );
  const merge = page.locator(`.pane-ask .ask[data-ask="${d.draft}"]`);
  await expect(merge).toContainText("drafted a merge");
  await merge.locator("[data-ask-decline]").click();
  await expect(el.locator(`[data-draft="${d.draft}"]`)).toHaveAttribute("data-draft-status", "dropped");
  expect(writes.length).toBe(1);
});
