// M11 on an iPhone (WebKit, #214 section 6): Changes from the sheet lists
// what the agent (a stand-in writing files) changed, a hunk's line opens a
// live file block there, and a build that fails is pushed to the phone's
// subscription and rerun from Needs you. WebKit in Playwright has no
// PushManager, so the subscription is posted the way the page would post
// it, and the fake push service checks what the daemon sent: decrypted,
// with Rerun among its actions. changes.spec.ts does the same on a Pixel 7,
// where the push also goes through the service worker.

import { execFileSync, type ChildProcess } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test, type Page } from "@playwright/test";
import { open, text } from "./helpers";
import { daemon, FakePush, iphone } from "./phones";
import type { PaneId } from "../src/proto";

let dir = "";
let repo = "";
let url = "";
let proc: ChildProcess | undefined;
let push: FakePush;

test.describe.configure({ mode: "serial" });
test.use({ ...iphone, baseURL: async ({}, use) => use(url) });

const git = (...args: string[]) => execFileSync("git", ["-C", repo, "-c", "user.email=t@example.com", "-c", "user.name=t", ...args]);

test.beforeAll(async () => {
  dir = mkdtempSync(join(tmpdir(), "ilg-e2e-changes-ios-"));
  repo = join(dir, "repo");
  mkdirSync(join(repo, "src"), { recursive: true });
  git("init", "-q", "-b", "main");
  writeFileSync(join(repo, "src/lib.rs"), "pub fn answer() -> u32 {\n    41\n}\n");
  git("add", ".");
  git("commit", "-qm", "init");
  push = await FakePush.start();
  ({ proc, url } = await daemon(join(dir, "state"), ["--tailscale-socket", "/nonexistent/sock", "--wisp-token-file", "/nonexistent"]));
});

test.afterAll(() => {
  proc?.kill("SIGKILL");
  push?.close();
  if (dir) rmSync(dir, { recursive: true, force: true });
});

const panesOf = (page: Page) => page.evaluate(() => window.__illogical.client.state!.panes);
const send = (pane: PaneId, line: string) =>
  fetch(`${url}/api/panes/${pane}/send`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ text: line, enter: true }) });
const newPane = async (page: Page, type: string, before: PaneId[]) => {
  await expect.poll(async () => (await panesOf(page)).find((p) => p.type === type && !before.includes(p.id))?.id ?? null, { timeout: 30_000 }).not.toBeNull();
  return (await panesOf(page)).find((p) => p.type === type && !before.includes(p.id))!.id;
};

let term: PaneId = 0;
let diff: PaneId = 0;

test("Changes, a hunk's line, a live file block: on an iPhone", async ({ page }) => {
  await open(page);
  term = (await panesOf(page))[0].id;
  await send(term, `cd ${repo}`);
  await expect.poll(async () => (await panesOf(page)).find((p) => p.id === term)?.cwd).toBe(repo);
  writeFileSync(join(repo, "src/lib.rs"), "pub fn answer() -> u32 {\n    42\n}\n\npub fn more() {}\n");

  const before = (await panesOf(page)).map((p) => p.id);
  await page.locator(".sheet-button").tap();
  await page.locator("[data-changes]").tap();
  diff = await newPane(page, "diff", before);
  const row = page.locator(`[data-diff="${diff}"] .diff-file[data-file="src/lib.rs"]`);
  await expect(row).toBeVisible({ timeout: 30_000 });
  await expect(row).toContainText("+3");
  await row.locator(".diff-file-head").tap();
  const line = row.locator(".dl.add", { hasText: "pub fn more" });
  await expect(line).toBeVisible();
  await line.tap();
  const file = await newPane(page, "file", [...before, diff]);
  const shown = page.locator(`[data-file-block="${file}"]`);
  await expect(shown.locator(".cm-mark-line")).toContainText("pub fn more", { timeout: 30_000 });
  await expect(shown.locator(".review-live")).toHaveText("live");
  // The agent edits on; the block follows.
  writeFileSync(join(repo, "src/lib.rs"), "// edited again\npub fn answer() -> u32 {\n    42\n}\n\npub fn more() {}\n");
  await expect(shown.locator(".cm-content")).toContainText("edited again", { timeout: 10_000 });
  await expect(shown.locator(".cm-mark-line")).toContainText("pub fn more");
});

test("a failed build is pushed to the iPhone, and Rerun from Needs you runs it again", async ({ page }) => {
  await open(page);
  // What the page posts once iOS hands it a subscription.
  const sub = push.subscription("iphone");
  const r = await page.evaluate(async (sub) => (await fetch("/api/push/subscribe", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(sub) })).status, sub);
  expect(r).toBe(200);

  const runs = join(dir, "runs");
  await send(term, `build() { echo x >> ${runs}; sleep 3.2; echo IOS-BUILD-$((40+2)); return 1; }`);
  await page.evaluate((d) => window.__illogical.client.setActive(d), diff);
  await send(term, "build");
  const payload = await push.next("iphone", (p) => p.pane === term && p.reason?.kind === "failed", 20_000);
  expect(payload.body).toMatch(/^build failed \(exit 1\)/);
  expect(payload.reason!.actions).toEqual(["rerun", "dismiss"]);
  expect(push.refused).toEqual([]);

  await page.locator(".sheet-button").tap();
  await page.locator(`[data-wants="${term}"] [data-rerun]`).tap();
  await expect.poll(() => readFileSync(runs, "utf8").split("\n").filter(Boolean).length, { timeout: 10_000 }).toBe(2);
  await page.evaluate((t) => window.__illogical.client.setActive(t), term);
  await expect.poll(async () => (await text(page, term)).match(/IOS-BUILD-42/g)?.length ?? 0, { timeout: 15_000 }).toBe(2);
});
