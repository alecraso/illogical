// M40: *Live updates* on a Forgejo PR block, against a fake Forgejo served
// here (S23's #84) and the stand-in `tea` the config puts on the daemon's
// PATH. The button makes a repository webhook pointed at the daemon with a
// secret made there; a delivery signed with it (as Forgejo signs) makes the
// block read at once and say it's live; an unsigned one is refused; the
// button again removes the webhook. Nothing here reaches a real forge.

import { createHmac } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import { createServer, type Server, type ServerResponse } from "node:http";
import { join } from "node:path";
import { expect, test } from "@playwright/test";
import { menu, paneEl, panes, reset } from "./helpers";
import { listen } from "./ports";

const TOKEN = "e2e-forge-token";
const REPO = "jhgaylor/illogical";
const fixture = (f: string) =>
  JSON.parse(readFileSync(new URL(`../../crates/daemon/tests/fixtures/forgejo/forgejo-illogical-84/${f}`, import.meta.url), "utf8"));

let origin = "";
let server: Server;
let pulls = 0;
const hooks: { method: string; path: string; body: any }[] = [];

function json(res: ServerResponse, status: number, v: unknown) {
  res.writeHead(status, { "content-type": "application/json" }).end(JSON.stringify(v));
}

test.beforeAll(async () => {
  const item = fixture("item.json");
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
        if (p === "/pulls/84") {
          pulls += 1;
          return json(res, 200, { ...item, state: "open", merged: false, merged_at: null, html_url: `${origin}/${REPO}/pulls/84` });
        }
        if (p === "/pulls/84/reviews" || p === "/issues/84/timeline") return json(res, 200, []);
        if (p.startsWith("/commits/")) return json(res, 200, { state: "success", total_count: 0, statuses: [] });
        return json(res, 404, { message: "not here" });
      }
      hooks.push({ method: req.method!, path: p, body: JSON.parse(body || "{}") });
      if (req.method === "POST" && p === "/hooks") return json(res, 201, { id: 31, type: "forgejo", active: true });
      if (req.method === "DELETE" && p === "/hooks/31") return res.writeHead(204).end();
      json(res, 404, { message: "not here" });
    });
  });
  origin = `http://127.0.0.1:${await listen(server)}`;
  writeFileSync(join(process.env.ARUGULA_E2E_TEA_DIR!, "logins.json"), JSON.stringify([{ name: "e2e", url: origin, ssh_host: "", user: "jhgaylor", default: "false" }]));
});

test.afterAll(() => {
  server.close();
  writeFileSync(join(process.env.ARUGULA_E2E_TEA_DIR!, "logins.json"), "[]");
});

test("Live updates makes a signed webhook; a delivery reads the PR at once", async ({ page, baseURL }) => {
  await reset(page);
  await menu(page, paneEl(page, (await panes(page))[0]), "Open pull request…");
  await page.locator(".prompt input").fill(`${origin}/${REPO}/pulls/84`);
  await page.keyboard.press("Enter");
  await expect.poll(async () => (await panes(page)).length).toBe(2);
  const block = await page.evaluate(() => window.__arugula.client.state!.panes.find((p) => p.type === "forge")!.id);
  const el = page.locator(`[data-forge-block="${block}"]`);
  await expect(el.locator("[data-forge-live-state]")).toContainText("polling");
  await el.locator("[data-forge-live]").click();
  await expect.poll(() => hooks.length).toBe(1);
  const made = hooks[0].body;
  expect(made.type).toBe("forgejo");
  expect(made.config.url).toMatch(new RegExp(`^${baseURL}/api/forge/hooks/forgejo\\?k=`));
  await expect(el.locator("[data-forge-live]")).toHaveText("Stop live updates");
  await expect(el.locator("[data-forge-live-state]")).toContainText("live (webhook)");

  const payload = JSON.stringify({ action: "reviewed", repository: { full_name: REPO }, pull_request: { number: 84 } });
  const deliver = (sig?: string) =>
    fetch(made.config.url, {
      method: "POST",
      headers: { "content-type": "application/json", "x-forgejo-event": "pull_request_review_approved", ...(sig ? { "x-forgejo-signature": sig } : {}) },
      body: payload,
    }).then((r) => r.status);
  expect(await deliver()).toBe(401);
  expect(await deliver(createHmac("sha256", "not the secret").update(payload).digest("hex"))).toBe(401);
  const before = pulls;
  expect(await deliver(createHmac("sha256", made.config.secret).update(payload).digest("hex"))).toBe(200);
  await expect.poll(() => pulls, { timeout: 2000 }).toBeGreaterThan(before);
  await expect.poll(() => page.evaluate((b) => window.__arugula.client.request("GET", `/api/blocks/${b}`).then((r) => r.json<any>()).then((v) => v.state.pokes), block)).toBe(1);

  await el.locator("[data-forge-live]").click();
  await expect.poll(() => hooks.map((h) => `${h.method} ${h.path}`)).toEqual(["POST /hooks", "DELETE /hooks/31"]);
  await expect(el.locator("[data-forge-live]")).toHaveText("Live updates");
});
