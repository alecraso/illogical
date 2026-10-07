// M43: the Fountain agent catalog, against a fake Fountain served here
// (synthetic agents shaped like S24's) and the credentials file the config
// points the daemon at (this spec writes it). *Run on Fountain* runs the
// config's stand-in `fountain`, the fake ACP agent. Nothing here reaches a
// real Fountain.
//
// Opened from a pane's menu ("Fountain agents…"), the block lists every
// agent; the agent-specs chip leaves the curated ones, a search finds an
// agent by its skill, and *Run on Fountain* opens an agent block beside the
// catalog that answers a prompt.

import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createServer, type Server, type ServerResponse } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test, type Page } from "@playwright/test";
import { menu, paneEl, panes, reset } from "./helpers";
import { listen } from "./ports";

const KEY = "e2e-fountain-key";
const fixture = (f: string) => JSON.parse(readFileSync(new URL(`../../crates/daemon/tests/fixtures/fountain/${f}`, import.meta.url), "utf8"));

let origin = "";
let server: Server;
const seen: { path: string; ua: string }[] = [];

function json(res: ServerResponse, status: number, v: unknown) {
  res.writeHead(status, { "content-type": "application/json" }).end(JSON.stringify(v));
}

test.describe.configure({ mode: "serial" });

test.beforeAll(async () => {
  const agents = fixture("agents.json");
  const envs = fixture("environments.json");
  server = createServer((req, res) => {
    const u = new URL(req.url!, "http://fountain");
    seen.push({ path: u.pathname, ua: String(req.headers["user-agent"] ?? "") });
    if (req.headers.authorization !== `Bearer ${KEY}`) return json(res, 401, { error: { message: "invalid api key" } });
    if (u.pathname === "/api/agents") return json(res, 200, agents);
    if (u.pathname === "/api/environments") return json(res, 200, envs);
    json(res, 404, { error: "not here" });
  });
  origin = `http://127.0.0.1:${await listen(server)}`;
  writeFileSync(process.env.ARUGULA_FOUNTAIN_CREDENTIALS!, `[default]\napi_key = "${KEY}"\nbase_url = "${origin}"\n`);
});

test.afterAll(() => {
  server.close();
  rmSync(process.env.ARUGULA_FOUNTAIN_CREDENTIALS!, { force: true });
});

async function openCatalog(page: Page): Promise<number> {
  await reset(page);
  const term = (await panes(page))[0];
  await menu(page, paneEl(page, term), "Fountain agents…");
  await expect.poll(async () => (await panes(page)).length).toBe(2);
  return page.evaluate(() => window.__arugula.client.state!.panes.find((p) => p.type === "fountain")!.id);
}

test("the catalog filters, and Run on Fountain opens an agent block beside it", async ({ page }) => {
  const block = await openCatalog(page);
  const el = page.locator(`[data-fountain-block="${block}"]`);
  const cards = el.locator(".fountain-card");
  // Every agent, app-made ones too.
  await expect(cards).toHaveCount(108);
  await expect(el.locator("[data-fountain-count]")).toHaveText("108");
  expect(seen.length).toBeGreaterThan(0);
  expect(seen.every((s) => s.ua.startsWith("arugula/"))).toBe(true);
  await expect(el.locator('.fountain-card[data-source="app"]').first()).toBeVisible();

  // The agent-specs chip leaves the curated ones.
  await el.locator('[data-chips="source"] [data-chip="agent-specs"]').click();
  await expect(cards).toHaveCount(23);
  await expect(el.locator("[data-fountain-count]")).toHaveText("23 of 108");
  await expect(el.locator('[data-chips="source"] [data-chip="agent-specs"]')).toHaveAttribute("aria-pressed", "true");
  // A skill finds its agent.
  await el.locator(".fountain-search").fill("frontend-design");
  await expect(cards).toHaveCount(1);
  await expect(cards.first()).toHaveAttribute("data-agent", "designer");
  // The filter is the block's: kept in its state (and config).
  const blockState = (b: number) =>
    page.evaluate((b) => window.__arugula.client.request("GET", `/api/blocks/${b}`).then((r) => r.json<{ state: Record<string, any> }>()), b).then((v) => v.state);
  await expect.poll(() => blockState(block).then((s) => s.filter.query)).toBe("frontend-design");
  // Run here (M44) and Spec are offered.
  const designer = el.locator('.fountain-card[data-agent="designer"]');
  await expect(designer.locator("[data-run-here]")).toBeEnabled();
  await expect(designer.locator("[data-spec]")).toBeEnabled();

  // Clear, then find games and run it on Fountain.
  await el.locator("[data-fountain-clear]").click();
  await expect(cards).toHaveCount(108);
  await el.locator(".fountain-search").fill("games");
  await expect(el.locator('.fountain-card[data-agent="games"]')).toBeVisible();
  await el.locator('.fountain-card[data-agent="games"] [data-run]').click();
  await expect.poll(async () => (await panes(page)).length).toBe(3);
  const agent = await page.evaluate(() => window.__arugula.client.state!.panes.find((p) => p.type === "agent")!.id);
  expect(await panes(page)).toContain(agent);
  // It's today's Fountain agent block (here the fake ACP agent), and works.
  expect((await blockState(agent)).label).toBe("Fountain games");
  const a = paneEl(page, agent);
  await a.locator(".agent-composer textarea").fill("hello");
  await a.locator(".agent-composer textarea").press("Enter");
  await expect(a.locator(".agent-msg").last()).toHaveText("Hello! I am fake.");
});

// M44: *Run here* asks for a folder and opens a Claude Code block beside
// the catalog wearing the agent (here Claude Code's adapter is the fake ACP
// agent): its header says what it wears. games has inline skills only and
// no MCP servers, so nothing here reaches Infisical, gh or GitHub.
test("Run here wears the agent in a Claude Code block", async ({ page }) => {
  const block = await openCatalog(page);
  const el = page.locator(`[data-fountain-block="${block}"]`);
  await el.locator(".fountain-search").fill("games");
  const games = el.locator('.fountain-card[data-agent="games"]');
  await expect(games).toBeVisible();
  await games.locator("[data-run-here]").click();
  const dir = mkdtempSync(join(tmpdir(), "arugula-e2e-wear-"));
  try {
    await page.locator(".prompt input").fill(dir);
    await page.locator(".prompt input").press("Enter");
    await expect.poll(async () => (await panes(page)).length).toBe(3);
    const agent = await page.evaluate(() => window.__arugula.client.state!.panes.find((p) => p.type === "agent")!.id);
    const a = paneEl(page, agent);
    const worn = a.locator('[data-worn="games"]');
    await expect(worn).toContainText("as games", { timeout: 15_000 });
    for (const s of ["love2d", "pixijs", "screenshots-in-prs"]) await expect(worn.locator(`[data-worn-skill="${s}"]`)).toBeVisible();
    await expect(a.locator(".agent-name")).toHaveText("Claude Code as games");
    await a.locator(".agent-composer textarea").fill("hello");
    await a.locator(".agent-composer textarea").press("Enter");
    await expect(a.locator(".agent-msg").last()).toHaveText("Hello! I am fake.");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
