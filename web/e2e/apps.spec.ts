// M35: a studio box as a block, framed from another site.
//
// A fake studio (its token, the app list, minting entry links) and a fake
// box: a page behind a door that lets in whoever follows an entry link
// (a partitioned cookie, as studio's door hands out), and hud's routes
// (tabs, the chat stream with its queue frames, answers). The box is on
// `localhost`, Arugula's page on 127.0.0.1: another site, so the frame is
// third party as it is for real. Picked from a pane's menu, the box opens
// in the frame; its agent's question is a card on the block ("hud asks"),
// answered here and sent back to hud; answered in hud, the card goes.
// hud's pages open in the frame from the block's Records menu. No entry
// link is kept: not in the frame's src, the page's storage or the daemon's
// state directory.

import { readdirSync, readFileSync, statSync } from "node:fs";
import { createServer, type IncomingMessage, type Server, type ServerResponse } from "node:http";
import { join } from "node:path";
import { expect, test, type Page } from "@playwright/test";
import { menu, paneEl, panes, reset, closeContexts } from "./helpers";
import { listen } from "./ports";

test.afterAll(closeContexts);

const TOKEN = "e2e-studio-token";
let studio = "";
let box = "";
const servers: Server[] = [];
const keys = new Set<string>();
const sessions = new Set<string>();
let question: Record<string, unknown> | null = null;
const answers: Record<string, unknown>[] = [];
const streams = new Set<ServerResponse>();
const feeds = new Set<ServerResponse>();
let gates: Record<string, unknown>[] = [];
const approvals: Record<string, unknown>[] = [];
let failNext: string | null = null;
/** Reads of hud's work board (each one a `chant workspace status` in a real box). */
let boardReads = 0;
/** Requests a hung box took and hasn't answered, to answer when it's back. */
let hung: (() => void)[] | null = null;

function moved() {
  for (const s of feeds) s.write("event: live-state\ndata: {}\n\n");
}

function cookieOf(req: IncomingMessage): string | undefined {
  return /(?:^|;\s*)hud_session=(\w+)/.exec(req.headers.cookie ?? "")?.[1];
}

function frameOf(): string {
  return `data: ${JSON.stringify({ type: "hud-chat-queue", chatKey: "c1", waiting: [], ...(question ? { question } : {}) })}\n\n`;
}

function changed() {
  for (const s of streams) s.write(frameOf());
}

function ask(id: string) {
  const now = Date.now();
  question = {
    requestId: id,
    summary: "Which colour should the header be?",
    askedAt: now,
    expiresAt: now + 300_000,
    options: [
      { optionId: "o0", name: "Green", description: "Matches arugula" },
      { optionId: "o1", name: "Blue", description: "Calmer" },
    ],
  };
  changed();
}

function json(res: ServerResponse, status: number, v: unknown) {
  res.writeHead(status, { "content-type": "application/json" }).end(JSON.stringify(v));
}

test.describe.configure({ mode: "serial" });

test.beforeAll(async () => {
  const boxServer = createServer(function serve(req, res) {
    if (hung) return void hung.push(() => serve(req, res));
    const u = new URL(req.url!, "http://box");
    const authed = sessions.has(cookieOf(req) ?? "");
    if (u.pathname === "/__enter") {
      if (!keys.has(u.searchParams.get("k") ?? "")) return void res.writeHead(403).end("Not an entry link");
      const s = `s${sessions.size + 1}x${Date.now()}`;
      sessions.add(s);
      const to = u.searchParams.get("to");
      res.writeHead(302, {
        location: to && /^\/__hud\/(work|decisions|intent|sessions)$/.test(to) ? to : "/",
        "set-cookie": `hud_session=${s}; Path=/; HttpOnly; SameSite=None; Secure; Partitioned`,
        "cache-control": "no-store",
      });
      return void res.end();
    }
    if (u.pathname.startsWith("/__hud/api/")) {
      if (!authed) return json(res, 401, { error: "no session" });
      if (u.pathname === "/__hud/api/tabs") return json(res, 200, { tabs: [{ chatKey: "c1" }] });
      if (u.pathname === "/__hud/api/chat/stream") {
        res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache" });
        res.write(frameOf());
        streams.add(res);
        req.on("close", () => streams.delete(res));
        return;
      }
      if (u.pathname === "/__hud/api/work") {
        boardReads++;
        return json(res, 200, { v: 1, groups: [{ id: "approve", items: gates.map((gate) => ({ key: `gate:${gate.name}`, gate })) }] });
      }
      if (u.pathname === "/__hud/api/live/stream") {
        res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache" });
        res.write("event: live-snapshot\ndata: {}\n\n");
        feeds.add(res);
        req.on("close", () => feeds.delete(res));
        return;
      }
      if (u.pathname === "/__hud/api/work/gates/approve" && req.method === "POST") {
        let body = "";
        req.on("data", (d) => (body += d));
        req.on("end", () => {
          if (req.headers.origin !== box) return json(res, 403, { error: "cross-site" });
          if (failNext) {
            const error = failNext;
            failNext = null;
            return json(res, 422, { code: "chant-refused", error });
          }
          approvals.push(JSON.parse(body));
          gates = [];
          moved();
          json(res, 200, { v: 1, approval: { command: "chant approve", exitCode: 0 } });
        });
        return;
      }
      if (u.pathname === "/__hud/api/chat/answer" && req.method === "POST") {
        let body = "";
        req.on("data", (d) => (body += d));
        req.on("end", () => {
          if (req.headers.origin !== box) return json(res, 403, { error: "cross-site" });
          answers.push(JSON.parse(body));
          question = null;
          changed();
          json(res, 200, { answered: true });
        });
        return;
      }
      return json(res, 404, {});
    }
    if (!authed) return void res.writeHead(401, { "content-type": "text/html" }).end("<p id=door>You need a link to get in</p>");
    const page = u.pathname === "/__hud/work" ? "<h1 id=work>Work board</h1>" : "<p id=app>Hello from the box</p>";
    res.writeHead(200, { "content-type": "text/html" }).end(`<title>Pinboard</title>${page}`);
  });
  box = `http://localhost:${await listen(boxServer)}`;
  servers.push(boxServer);

  const studioServer = createServer((req, res) => {
    if (req.headers.authorization !== `Bearer ${TOKEN}`) return json(res, 401, { error: "who are you?" });
    // studio#292's shape.
    if (req.url === "/api/apps") return json(res, 200, { apps: [{ name: "pinboard", title: "Pinboard", url: box, createdAt: 1 }] });
    if (req.url === "/api/apps/pinboard/open" && req.method === "POST") {
      const k = `k${keys.size + 1}x${Date.now()}`;
      keys.add(k);
      return json(res, 200, { url: `${box}/__enter?e=${Date.now() + 600_000}&k=${k}` });
    }
    json(res, 404, {});
  });
  studio = `http://127.0.0.1:${await listen(studioServer)}`;
  servers.push(studioServer);
});

test.afterAll(async ({ browser }) => {
  const page = await browser.newPage();
  await page.goto("/");
  await page.evaluate(() => window.__arugula?.client.request("DELETE", "/api/studio")).catch(() => {});
  await page.close();
  for (const s of streams) s.end();
  for (const s of feeds) s.end();
  for (const s of servers) s.close();
});

/** Every file under the daemon's state directory. */
function files(dir: string): string[] {
  const out: string[] = [];
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    const st = statSync(p, { throwIfNoEntry: false });
    if (!st) continue;
    if (st.isDirectory()) out.push(...files(p));
    else if (st.isFile()) out.push(p);
  }
  return out;
}

async function blockOf(page: Page): Promise<number> {
  await expect.poll(async () => (await panes(page)).length).toBe(2);
  return page.evaluate(() => window.__arugula.client.state!.panes.find((p) => p.type === "app")!.id);
}

test("a studio app opens framed from another site; its agent's question is answered here", async ({ page }) => {
  await reset(page);
  const term = (await panes(page))[0];
  // Not linked to a studio: the menus don't offer one (#180).
  await paneEl(page, term).click({ button: "right", position: { x: 60, y: 60 } });
  await expect(page.getByRole("menuitem", { name: "Split right" })).toBeVisible();
  await expect(page.getByRole("menuitem", { name: "Open a studio app…" })).toHaveCount(0);
  await page.keyboard.press("Escape");
  const r = await page.evaluate(([url, token]) => window.__arugula.client.request("POST", "/api/studio", { url, token }).then((r) => r.status), [studio, TOKEN]);
  expect(r).toBe(200);

  await menu(page, paneEl(page, term), "Open a studio app…");
  await page.locator('.picker.apps [data-app="pinboard"]').click();
  const b = await blockOf(page);
  const el = paneEl(page, b);
  await expect(el.locator(".app-name")).toHaveText("Pinboard");

  // In: the door's partitioned cookie works in the third-party frame.
  const frame = page.frameLocator(`[data-pane="${b}"] iframe`);
  await expect(frame.locator("#app")).toHaveText("Hello from the box");
  await expect(el.locator("[data-follower=following]")).toBeVisible();
  // The link went to the frame and nowhere else.
  expect(await el.locator("iframe").getAttribute("src")).toBe(`${box}/`);
  const kept = await page.evaluate(() => JSON.stringify({ ...localStorage }) + JSON.stringify({ ...sessionStorage }) + location.href);
  expect(kept).not.toContain("__enter");

  // The box's agent asks: a card on the block.
  ask("r1");
  const card = el.getByRole("dialog", { name: "Which colour should the header be?" });
  await expect(card).toBeVisible();
  await expect(el.locator(".pane-ask-bar")).toContainText("hud asks");
  await expect(card.getByRole("button", { name: "Answer in terminal" })).toHaveCount(0);
  await card.getByRole("radio", { name: /^Blue/ }).click();
  await card.getByRole("button", { name: "Submit" }).click();
  await expect.poll(() => answers.length).toBe(1);
  expect(answers[0]).toEqual({ chatKey: "c1", requestId: "r1", optionId: "o1" });
  await expect(card).toHaveCount(0);

  // Answered in hud's own panel: the card goes.
  ask("r2");
  await expect(card).toBeVisible();
  question = null;
  changed();
  await expect(card).toHaveCount(0);

  // A release waits at ship: a gate on the block and the phone's sheet's
  // reason. hud refusing puts its error on the card; then it's approved.
  // The live feed moving all through a turn doesn't read the board each
  // time: reads are paced (at least 5s apart), so the gate may wait that.
  const before = boardReads;
  gates = [{ member: "delivery", component: "release", name: "ship", env: "prod", needed: 1, approvals: 0, approve: "chant approve release ship --env prod" }];
  for (let i = 0; i < 15; i++) {
    moved();
    await page.waitForTimeout(200);
  }
  const gate = el.locator('[data-gate="delivery/release/ship"]');
  await expect(gate).toContainText("release waits at gate ship in prod", { timeout: 15_000 });
  expect(boardReads - before).toBeLessThanOrEqual(2);
  await expect.poll(() => page.evaluate((p) => window.__arugula.client.info(p)?.reason?.kind, b)).toBe("gate");
  failNext = "chant approve exited 1";
  await gate.getByRole("button", { name: "Approve" }).click();
  await expect(gate.locator(".app-gate-error")).toHaveText("hud: chant approve exited 1");
  await expect.poll(() => page.evaluate((p) => window.__arugula.client.info(p)?.reason?.headline, b)).toContain("approving failed");
  await gate.getByRole("button", { name: "Approve" }).click();
  await expect(gate).toHaveCount(0);
  expect(approvals).toEqual([{ member: "delivery", component: "release", gate: "ship", env: "prod" }]);
  await expect.poll(() => page.evaluate((p) => window.__arugula.client.info(p)?.reason ?? null, b)).toBeNull();

  // hud's pages, in the frame.
  await el.getByRole("button", { name: "Records ▾" }).click();
  await page.getByRole("menuitem", { name: "Work", exact: true }).click();
  await expect(frame.locator("#work")).toHaveText("Work board");

  // Back after a reload of the page: it enters again.
  await page.reload();
  await expect.poll(() => page.evaluate(() => window.__arugula?.client.connected)).toBe(true);
  await expect(page.frameLocator(`[data-pane="${b}"] iframe`).locator("#app")).toHaveText("Hello from the box");

  // Nothing the daemon keeps holds an entry link.
  const state = process.env.ARUGULA_E2E_STATE;
  if (state) {
    for (const f of files(state)) {
      const text = readFileSync(f).toString("latin1");
      expect(text.includes("__enter"), f).toBe(false);
      for (const k of keys) expect(text.includes(k), f).toBe(false);
    }
  }
});

test("a box that takes the connection and never answers says so, then comes in", async ({ page }) => {
  test.setTimeout(60_000);
  await reset(page);
  const term = (await panes(page))[0];
  await page.evaluate(([url, token]) => window.__arugula.client.request("POST", "/api/studio", { url, token }), [studio, TOKEN]);
  hung = [];
  await menu(page, paneEl(page, term), "Open a studio app…");
  await page.locator('.picker.apps [data-app="pinboard"]').click();
  const b = await blockOf(page);
  const el = paneEl(page, b);

  // Not a blank white frame: a card that says why.
  await expect(el.getByText("Opening Pinboard…")).toBeVisible();
  const stuck = el.locator("[data-app-stuck]");
  await expect(stuck).toContainText("Pinboard isn't answering", { timeout: 25_000 });
  await expect(el.locator("iframe")).toBeHidden();

  // The box answers: the frame comes in.
  const held = hung;
  hung = null;
  for (const go of held) go();
  await expect(page.frameLocator(`[data-pane="${b}"] iframe`).locator("#app")).toHaveText("Hello from the box");
  await expect(stuck).toHaveCount(0);
  await expect(el.locator("iframe")).toBeVisible();
});
