// M34's "done when", against the real chant: a chant workspace (the fixture
// in e2e/fixtures/chant-workspace, chant 0.87.0 pinned in its lock file) as
// a block. `illogical workspace` shows its members; a gated op (`chant run
// ship` exits 3) shows as attention while the block is drawn, within a few
// seconds; the owner approves on the desktop, an editor on a phone from the
// sheet's gates-first list (chant's ledger names each), a viewer sees the
// gate with no Approve; the next `chant run` walks through. The pane menu
// and the picker offer "Open as workspace" in a workspace's directory.
//
// The first run installs the fixture's chant (`npm ci`, a few seconds).

import { execFileSync, spawn, spawnSync, type ChildProcess } from "node:child_process";
import { cpSync, existsSync, mkdtempSync, realpathSync, rmSync, symlinkSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { devices, expect, test, type Page } from "@playwright/test";
import { menu, open, paneEl, closeContexts } from "./helpers";
import type { PaneId, Reason } from "../src/proto";
import { ANY, daemonPort } from "./ports";

test.afterAll(closeContexts);

const OWNER = "me@example.com";
const FRIEND = "friend@example.com";
const FIXTURE = fileURLToPath(new URL("./fixtures/chant-workspace", import.meta.url));
const phone = (() => {
  const { viewport, userAgent, deviceScaleFactor, isMobile, hasTouch } = devices["Pixel 7"];
  return { viewport, userAgent, deviceScaleFactor, isMobile, hasTouch };
})();

let PORT = 0;
let dir = "";
let ws = "";
let daemon: ChildProcess | null = null;

const base = () => `http://127.0.0.1:${PORT}`;
const chant = (cwd: string, ...args: string[]) =>
  spawnSync(join(FIXTURE, "node_modules/.bin/chant"), args, { cwd, encoding: "utf8", env: { ...process.env, NO_COLOR: "1" } });
const git = (...args: string[]) => execFileSync("git", ["-C", ws, "-c", "user.email=t@example.com", "-c", "user.name=t", ...args]);
const cli = (...args: string[]) => execFileSync("../target/debug/illogical", ["--socket", join(dir, "state/sock"), ...args], { encoding: "utf8" });
const post = (path: string, body: unknown, headers: Record<string, string> = {}) =>
  fetch(base() + path, { method: "POST", headers: { "content-type": "application/json", ...headers }, body: JSON.stringify(body) });
const panesOf = (page: Page) => page.evaluate(() => window.__illogical.client.state!.panes);
const reasonOf = async (page: Page, id: PaneId): Promise<Reason | null> => (await panesOf(page)).find((p) => p.id === id)?.reason ?? null;
/** Who approved delivery's gates, by chant's own `status`. */
const approvers = () => {
  const st = JSON.parse(chant(ws, "workspace", "status", "local", "--json").stdout);
  const delivery = st.members.find((m: { name: string }) => m.name === "delivery");
  return delivery.gates.flatMap((g: { approvals: { principal: string }[] }) => g.approvals.map((a) => a.principal));
};
/** `chant run <op>` in delivery: 3 at the gate, 0 through it. */
const run = (op = "ship") => chant(join(ws, "delivery"), "run", op).status;

test.describe.configure({ mode: "serial" });
test.use({ baseURL: async ({}, use) => use(base()) });

let block: PaneId = 0;

test.beforeAll(async () => {
  test.setTimeout(180_000);
  if (!existsSync(join(FIXTURE, "node_modules/.bin/chant"))) {
    execFileSync("npm", ["ci", "--no-audit", "--no-fund"], { cwd: FIXTURE, stdio: "ignore" });
  }
  // Resolved, as the daemon reports paths (macOS's temp dir is behind a symlink).
  dir = realpathSync(mkdtempSync(join(tmpdir(), "ilg-e2e-workspace-")));
  ws = join(dir, "toy");
  cpSync(FIXTURE, ws, { recursive: true, filter: (src) => !src.includes("node_modules") });
  symlinkSync(join(FIXTURE, "node_modules"), join(ws, "node_modules"));
  git("init", "-q", "-b", "main");
  git("add", ".");
  git("commit", "-qm", "the toy workspace");
  daemon = spawn(
    "../target/debug/illogicald",
    [
      ...["--listen", ANY, "--state-dir", join(dir, "state"), "--owner", OWNER, "--tailscale-socket", "/nonexistent/sock"],
      ...["--shell", "bash --norc --noprofile", "--no-manager-env", "--wisp-token-file", "/nonexistent"],
    ],
    { stdio: "ignore" },
  );
  PORT = await daemonPort(join(dir, "state"), daemon);
  for (let i = 0; i < 100; i++) {
    try {
      if ((await fetch(`${base()}/api/host`)).ok) return;
    } catch {
      // not up yet
    }
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error("daemon did not start");
});

test.afterAll(() => {
  daemon?.kill("SIGKILL");
  if (dir) rmSync(dir, { recursive: true, force: true });
});

test("illogical workspace shows its members; a gate reached while it's drawn is attention in seconds", async ({ page }) => {
  test.setTimeout(90_000);
  const out = cli("workspace", ws);
  block = Number(/^%(\d+)/.exec(out)![1]);
  expect(out).toContain("toy: 2 members, 0 records, 0 waiting at a gate");
  await open(page);
  await page.evaluate((b) => window.__illogical.client.setActive(b), block);
  const shown = page.locator(`[data-workspace-block="${block}"]`);
  await expect(shown.locator("[data-member]")).toHaveCount(2);
  await expect(shown.locator('[data-member="app"] .ws-kind')).toHaveText("other");
  await expect(shown.locator(".review-live")).toHaveText("live");

  // The op stops at its gate, elsewhere (a terminal, CI): the block sees
  // chant's ledger move and reads again.
  expect(run()).toBe(3);
  const t = Date.now();
  await expect(shown.locator('[data-gate="delivery/ship/approve-ship"]')).toBeVisible({ timeout: 8_000 });
  const took = Date.now() - t;
  console.log(`chant run exits 3 → the gate on screen: ${took} ms`);
  expect(took).toBeLessThan(6_000);
  // The pane's attention follows the block's state: poll it, as it may
  // come a moment after the gate is drawn.
  await expect
    .poll(async () => {
      const r = await reasonOf(page, block);
      return r && [r.kind, r.headline, r.bundle];
    })
    .toEqual(["gate", "delivery: ship waits at gate approve-ship", `gate:${ws}`]);
  const r = (await reasonOf(page, block))!;
  expect(r.actions).toEqual(["allow", "dismiss"]);
  await expect(shown.locator('[data-member="delivery"]')).toHaveClass(/waits/);
});

test("the owner approves; the next run walks through", async ({ page }) => {
  await open(page);
  await page.evaluate((b) => window.__illogical.client.setActive(b), block);
  const shown = page.locator(`[data-workspace-block="${block}"]`);
  await shown.locator('[data-gate="delivery/ship/approve-ship"] [data-approve]').click();
  // Approving runs chant, which on a busy machine takes seconds.
  await expect(shown.locator("[data-ws-said]")).toContainText("Approved approve-ship", { timeout: 15_000 });
  await expect(shown.locator("[data-gate]")).toHaveCount(0);
  await expect.poll(async () => (await panesOf(page)).find((p) => p.id === block)?.attention).toBe("idle");
  // chant's ledger names the owner by their illogical name.
  expect(approvers()).toEqual([OWNER]);
  expect(run()).toBe(0);
});

test("on a phone, an editor approves from the sheet, gates first; a viewer sees it and can't", async ({ browser, page }) => {
  test.setTimeout(90_000);
  await open(page);
  // Something else wanting attention too, to come after the gate.
  await post("/api/run", { command: "sleep 1; false" });
  // Another op stops at its gate.
  expect(run("release")).toBe(3);
  const session = await page.evaluate((b) => {
    const c = window.__illogical.client;
    return c.sessionOfTab(c.tabOfPane(b)!.id);
  }, block);

  // On the swarm's rail: a gate card, with Approve.
  await page.goto("/#swarm");
  const card = page.locator('.swarm-card[data-kind="gate"]');
  await expect(card).toBeVisible({ timeout: 15_000 });
  await expect(card.locator(".ch b")).toHaveText("Waits at a gate");
  await expect(card.locator(".cq")).toHaveText("delivery: release waits at gate approve-release");
  await expect(card.locator("[data-approve-gate]")).toHaveText("Approve");
  expect(await card.getAttribute("data-bundle")).toBe(`gate:${ws}`);

  // A viewer: the gate, no Approve.
  expect((await post("/api/acl", { session, principal: `tailnet:${FRIEND}`, role: "viewer" })).ok).toBe(true);
  const ctx = await browser.newContext({ ...phone, baseURL: base(), extraHTTPHeaders: { "tailscale-user-login": FRIEND } });
  const friend = await ctx.newPage();
  await friend.goto("/");
  await expect.poll(() => friend.evaluate(() => window.__illogical?.client.role())).toBe("viewer");
  await expect.poll(async () => (await reasonOf(friend, block))?.kind ?? null, { timeout: 15_000 }).toBe("gate");
  await friend.locator(".sheet-button").click();
  await expect(friend.locator(".needs-you [data-wants]").first()).toHaveAttribute("data-wants", String(block));
  await expect(friend.locator("[data-approve-gate]")).toHaveCount(0);
  await friend.locator(".sheet-backdrop").click({ position: { x: 5, y: 5 } });
  await friend.evaluate((b) => window.__illogical.client.setActive(b), block);
  const theirs = friend.locator(`[data-workspace-block="${block}"]`);
  await expect(theirs.locator('[data-gate="delivery/release/approve-release"]')).toBeVisible();
  await expect(theirs.locator("[data-approve]")).toHaveCount(0);
  expect((await post(`/api/blocks/${block}/call/approve`, {}, { "tailscale-user-login": FRIEND })).status).toBe(403);

  // Made an editor: Approve, first in the sheet.
  expect((await post("/api/acl", { session, principal: `tailnet:${FRIEND}`, role: "editor" })).ok).toBe(true);
  await expect.poll(() => friend.evaluate(() => window.__illogical.client.role())).toBe("editor");
  await friend.locator(".sheet-button").click();
  const first = friend.locator(".needs-you [data-wants]").first();
  await expect(first).toHaveAttribute("data-wants", String(block));
  await expect(friend.locator(".needs-you [data-wants]")).toHaveCount(2, { timeout: 10_000 });
  await first.locator("[data-approve-gate]").tap();
  await expect.poll(async () => (await reasonOf(page, block))?.kind ?? null, { timeout: 15_000 }).toBeNull();
  expect(approvers().sort()).toEqual([FRIEND, OWNER].sort());
  expect(run("release")).toBe(0);
  await ctx.close();
});

test("Open as workspace, from a pane's menu and the picker, in a workspace's directory", async ({ page }) => {
  await open(page);
  const term = (await panesOf(page)).find((p) => p.type === "terminal")!.id;
  await page.evaluate((t) => window.__illogical.client.setActive(t), term);
  await post(`/api/panes/${term}/send`, { text: `cd ${ws}`, enter: true });
  await expect.poll(async () => (await panesOf(page)).find((p) => p.id === term)?.cwd).toBe(ws);
  const before = (await panesOf(page)).filter((p) => p.type === "workspace").length;
  await menu(page, paneEl(page, term), "Open as workspace");
  await expect.poll(async () => (await panesOf(page)).filter((p) => p.type === "workspace").length).toBe(before + 1);

  // The picker, from a terminal elsewhere.
  await post(`/api/panes/${term}/send`, { text: `cd ${dir}`, enter: true });
  await expect.poll(async () => (await panesOf(page)).find((p) => p.id === term)?.cwd).toBe(dir);
  await page.evaluate((t) => window.__illogical.client.setActive(t), term);
  await page.keyboard.press("Control+Shift+G");
  await expect(page.locator(".picker")).toBeVisible();
  await expect(page.locator("[data-open-workspace]")).toHaveCount(0);
  await page.locator(`.picker-row:not(.recent)[data-path="${ws}"]`).click();
  await page.locator("[data-open-workspace]").click();
  await expect.poll(async () => (await panesOf(page)).filter((p) => p.type === "workspace").length).toBe(before + 2);
});
