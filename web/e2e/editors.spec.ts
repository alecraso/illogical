// M27: editor blocks, with the real code-server (the release illogical
// pins, downloaded into ~/.cache/illogical/code-server on first use). VS Code
// opens from a pane's menu on that pane's directory, on the block's own
// origin, in illogical's theme; `illogical edit FILE:LINE` opens a file in
// under 3 s once the server is warm, on the desktop, a Pixel 7 and an
// iPhone (WebKit);
// the extension reports the file and the cursor; the swarm shows the block
// as an editor and opens one from a tile; the block survives a daemon
// restart (and a "reboot", server and all) with the file still open; and a
// viewer can't open one. The app (127.0.0.1) and the blocks (*.localhost)
// are different sites, and one test runs in a browser that blocks
// third-party cookies, and so the block's storage (#69).

import { execFile, spawn, type ChildProcess } from "node:child_process";
import { cpSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { promisify } from "node:util";
import { chromium, expect, test, type FrameLocator, type Page } from "@playwright/test";
import { menu, paneEl, panes, ready, reset, run, closeContexts } from "./helpers";
import type { PaneId } from "../src/proto";
import { tokenCookies } from "./local-token";
import { ANY, blockPort, daemonPort } from "./ports";
import { iphone, launchWebkit, pixel7 } from "./phones";

test.afterAll(closeContexts);

let PORT = 0;
let BLOCKS = 0;
let APP = "";
const OWNER = "me@example.com";
const FRIEND = "friend@example.com";
test.use({ baseURL: async ({}, use) => use(APP) });
test.describe.configure({ mode: "serial" });

// Made in beforeAll: this module is loaded more than once.
let dir = "";
let state = "";
let proj = "";
const AUTH = "crates/control/src/auth.rs";
/** Lines of AUTH (a copy of the repo's, so they move as it changes): one
 * that says Redirect, and STATE_COOKIE's. */
const lineOf = (re: RegExp) => readFileSync(`../${AUTH}`, "utf8").split("\n").findIndex((l) => re.test(l)) + 1;
const AT = lineOf(/\bRedirect\b/);
const LATER = lineOf(/^const STATE_COOKIE/);
const sh = promisify(execFile);
let daemon: ChildProcess | undefined;

async function start() {
  daemon = spawn(
    "../target/debug/illogicald",
    [
      ...["--listen", PORT ? `127.0.0.1:${PORT}` : ANY, "--block-listen", BLOCKS ? `127.0.0.1:${BLOCKS}` : ANY, "--state-dir", state],
      ...["--shell", "bash --norc --noprofile", "--no-manager-env", "--owner", OWNER, "--tailscale-socket", "/nonexistent/sock"],
    ],
    // code-server keeps its own logs under XDG_DATA_HOME: not the user's.
    { stdio: "ignore", env: { ...process.env, ILLOGICAL_WISP_TOKEN_FILE: "/nonexistent", XDG_DATA_HOME: join(dir, "data") } },
  );
  // On ports of its choosing, then on the same ones again.
  PORT ||= await daemonPort(state, daemon);
  BLOCKS ||= await blockPort(state, daemon);
  APP = `http://127.0.0.1:${PORT}`;
  for (let i = 0; i < 100; i++) {
    try {
      if ((await fetch(`${APP}/api/host`)).ok) return;
    } catch {
      // not up yet
    }
    await new Promise((r) => setTimeout(r, 100));
  }
}

/** Stop the daemon as a restart does: it saves, then exits. */
async function stop() {
  const d = daemon;
  daemon = undefined;
  if (!d || d.exitCode !== null) return;
  const gone = new Promise((r) => d.once("exit", r));
  d.kill("SIGTERM");
  await gone;
}

/** Stop code-server too (what a reboot does). */
function stopServer() {
  try {
    process.kill(Number(readFileSync(join(state, "editor/code-server.pid"), "utf8").trim()));
  } catch {
    // not running
  }
}

test.beforeAll(async () => {
  // Resolved: macOS's temp dir is behind a symlink, and VS Code reports
  // the real path.
  dir = realpathSync(mkdtempSync(join(tmpdir(), "ilg-e2e-editors-")));
  state = join(dir, "state");
  proj = join(dir, "illogical");
  mkdirSync(join(proj, ".git"), { recursive: true });
  mkdirSync(join(proj, "crates/control/src"), { recursive: true });
  cpSync(`../${AUTH}`, join(proj, AUTH));
  await start();
});

test.afterAll(async () => {
  await stop();
  stopServer();
  rmSync(dir, { recursive: true, force: true });
});

const CLI = resolve("../target/debug/illogical");
const cli = (...args: string[]) => sh(CLI, ["--socket", join(state, "sock"), "--json", ...args], { cwd: proj });
const blockState = (page: Page, b: PaneId) => page.evaluate((b) => window.__illogical.client.blocks.get(b)?.state as Record<string, unknown> | null, b);
const editors = (page: Page) => page.evaluate(() => window.__illogical.client.state!.panes.filter((p) => p.type === "editor").map((p) => p.id));
const frame = (page: Page, b: PaneId): FrameLocator => page.frameLocator(`[data-pane="${b}"] iframe`);
/** A line of the file, as VS Code draws it. */
const shown = (f: FrameLocator, text: string) => f.locator(".monaco-editor .view-lines", { hasText: text }).first();

let term: PaneId = 0;
let first: PaneId = 0;

test("Open in editor: VS Code on the pane's directory, on the block's own origin, in illogical's colours", async ({ page }) => {
  // The first run downloads code-server (about 230 MB).
  test.setTimeout(600_000);
  await reset(page);
  [term] = await panes(page);
  await run(page, term, `cd ${proj} && echo in-$((40+2))`, "in-42");
  await expect.poll(() => page.evaluate((p) => window.__illogical.client.cwd(p), term)).toBe(proj);
  await menu(page, paneEl(page, term), "Open in editor");
  await expect.poll(() => editors(page)).toHaveLength(1);
  [first] = await editors(page);
  await page.evaluate((b) => window.__illogical.client.setActive(b), first);
  await expect.poll(async () => (await blockState(page, first))?.server, { timeout: 540_000 }).toEqual({ is: "running" });
  const f = frame(page, first);
  await expect(f.locator(".monaco-workbench")).toBeVisible({ timeout: 60_000 });
  // The pane's directory, in the explorer.
  await expect(f.locator(".explorer-folders-view .monaco-list-row", { hasText: "crates" }).first()).toBeVisible({ timeout: 30_000 });
  const info = await page.evaluate((b) => window.__illogical.client.info(b), first);
  expect(info?.kind).toBe("editor");
  expect(info?.project?.name).toBe("illogical");
  // Its own origin, sandboxed to it.
  const origin = await page.evaluate((b) => new URL((document.querySelector(`[data-pane="${b}"] iframe`) as HTMLIFrameElement).src).origin, first);
  expect(origin).toMatch(new RegExp(`^http://b-${first}-[a-z0-9]{20}\\.localhost:${BLOCKS}$`));
  expect(await paneEl(page, first).locator("iframe").getAttribute("sandbox")).toContain("allow-same-origin");
  // illogical's theme: the terminal's background.
  await expect
    .poll(() => f.locator(".monaco-workbench .part.sidebar").evaluate((e) => getComputedStyle(e).backgroundColor))
    .toBe("rgb(24, 24, 37)");
  // The server has no TCP port: only a 0600 socket.
  const { stdout } = await sh("ls", ["-l", `${join(state, "sock")}-code`]);
  expect(stdout).toMatch(/^srw-------/);
});

test("illogical edit FILE:LINE: the file at its line in under 3 s once warm, reported as it moves", async ({ page }) => {
  await page.goto("/");
  await expect.poll(() => page.evaluate(() => window.__illogical?.client.connected)).toBe(true);
  const t0 = Date.now();
  const block = JSON.parse((await cli("edit", `${AUTH}:${AT}`)).stdout).block as PaneId;
  await expect.poll(() => page.evaluate((b) => !!window.__illogical.client.info(b), block)).toBe(true);
  await page.evaluate((b) => window.__illogical.client.setActive(b), block);
  const f = frame(page, block);
  await expect(shown(f, "Redirect")).toBeVisible({ timeout: 10_000 });
  const ms = Date.now() - t0;
  console.log(`desktop: ${AUTH} shown ${ms} ms after illogical edit`);
  expect(ms).toBeLessThan(3000);
  await expect(f.locator(".tab.active", { hasText: "auth.rs" })).toBeVisible();

  // The extension says which file and where, with the lines around it.
  await expect.poll(async () => ((await blockState(page, block))?.lines as string[]).some((l) => l.includes("Redirect"))).toBe(true);
  const s = (await blockState(page, block))!;
  expect(s.file).toBe(AUTH);
  expect(s.line).toBe(AT);
  expect(await page.evaluate((b) => window.__illogical.client.info(b)?.file, block)).toBe(AUTH);
  // It follows the cursor.
  await f.locator(".monaco-editor .view-lines").first().click();
  await page.keyboard.press("Control+g");
  await page.keyboard.type(`${LATER}`);
  await page.keyboard.press("Enter");
  await expect.poll(async () => (await blockState(page, block))?.line).toBe(LATER);
  expect(((await blockState(page, block))!.lines as string[]).some((l) => l.includes("STATE_COOKIE"))).toBe(true);
  // ...which is what a capture (the swarm's preview) shows.
  const text = await (await fetch(`${APP}/api/panes/${block}/capture`)).text();
  expect(text.split("\n")[0]).toBe(`${AUTH}:${LATER}`);
  expect(text).toContain("STATE_COOKIE");
});

/** From a phone's page: open a file at a line (one that says Redirect) as
 * an editor block, show it, and time it to the line drawn in VS Code. */
async function openFromPhone(page: Page, line: number): Promise<number> {
  await page.goto("/");
  await expect.poll(() => page.evaluate(() => window.__illogical?.client.connected)).toBe(true);
  await expect.poll(() => page.evaluate(() => window.__illogical.client.state !== null)).toBe(true);
  const t0 = Date.now();
  const block = await page.evaluate(
    async ([path, from, line]) => {
      const res = await window.__illogical.client.request("POST", "/api/blocks", { type: "editor", config: { path, line }, from_pane: from });
      return (await res.json<{ block: number }>()).block;
    },
    [join(proj, AUTH), term, line] as const,
  );
  await expect.poll(() => page.evaluate((b) => !!window.__illogical.client.info(b), block)).toBe(true);
  await page.evaluate((b) => window.__illogical.client.setActive(b), block);
  // The phone draws one pane at a time: this one, filling the screen.
  await expect(page.locator(".pane")).toHaveCount(1);
  await expect(page.locator(`[data-pane="${block}"]`)).toBeVisible();
  await expect(shown(frame(page, block), "Redirect")).toBeVisible({ timeout: 10_000 });
  const ms = Date.now() - t0;
  await expect.poll(async () => (await blockState(page, block))?.line).toBe(line);
  return ms;
}

// #214 section 6: from a phone context (touch, mobile, its own size), on a
// warm server. Chrome as a Pixel 7.
test("from a Pixel 7, a file opens in under 3 s once warm", async ({ browser }) => {
  const ctx = await browser.newContext({ ...pixel7, baseURL: APP, storageState: { cookies: tokenCookies, origins: [] } });
  const ms = await openFromPhone(await ctx.newPage(), AT);
  console.log(`Pixel 7: ${AUTH} shown ${ms} ms after opening`);
  expect(ms).toBeLessThan(3000);
  await ctx.close();
});

// And Safari's engine as an iPhone (Playwright's WebKit: the app on
// 127.0.0.1 and the block on *.localhost are other sites there too).
test("from an iPhone (WebKit), a file opens in under 3 s once warm", async () => {
  const browser = await launchWebkit();
  try {
    const ctx = await browser.newContext({ ...iphone, baseURL: APP, storageState: { cookies: tokenCookies, origins: [] } });
    const ms = await openFromPhone(await ctx.newPage(), AT);
    console.log(`iPhone: ${AUTH} shown ${ms} ms after opening`);
    expect(ms).toBeLessThan(3000);
  } finally {
    await browser.close();
  }
});

test("from another site, in a browser that blocks third-party cookies: the file at its line (#69)", async () => {
  // Chrome's "Block third-party cookies" refuses a cross-site frame its
  // storage too: localStorage throws, and VS Code's workbench used to stop
  // there, blank.
  const profile = join(dir, "chrome-3pc-blocked");
  mkdirSync(join(profile, "Default"), { recursive: true });
  writeFileSync(join(profile, "Default/Preferences"), JSON.stringify({ profile: { cookie_controls_mode: 1, block_third_party_cookies: true } }));
  const ctx = await chromium.launchPersistentContext(profile, { channel: "chrome", baseURL: APP, viewport: { width: 1000, height: 640 } });
  // A profile of its own: signed in to the daemon as the config's contexts are.
  await ctx.addCookies(tokenCookies);
  try {
    const page = ctx.pages()[0] ?? (await ctx.newPage());
    await page.goto("/");
    await expect.poll(() => page.evaluate(() => window.__illogical?.client.connected)).toBe(true);
    const nav = page.waitForRequest((r) => r.isNavigationRequest() && r.frame() !== page.mainFrame() && r.url().includes("workspace="));
    const block = JSON.parse((await cli("edit", `${AUTH}:${AT}`)).stdout).block as PaneId;
    await expect.poll(() => page.evaluate((b) => !!window.__illogical.client.info(b), block)).toBe(true);
    await page.evaluate((b) => window.__illogical.client.setActive(b), block);
    // A cross-site frame, refused its storage.
    const h = await (await nav).allHeaders();
    expect(h["sec-fetch-site"]).toBe("cross-site");
    expect(h["sec-fetch-storage-access"]).toBe("none");
    const f = frame(page, block);
    await expect(shown(f, "Redirect")).toBeVisible({ timeout: 30_000 });
    await expect(f.locator(".tab.active", { hasText: "auth.rs" })).toBeVisible();
    await expect.poll(async () => (await blockState(page, block))?.line).toBe(AT);
  } finally {
    await ctx.close();
  }
});

test("in the swarm: an editor tile with its file, and Open in editor from a tile", async ({ page }) => {
  await page.goto("/#swarm");
  await expect(page.locator(".swarm")).toBeVisible();
  await expect.poll(() => page.evaluate(() => window.__illogical.fleet.panes.filter((p) => p.info.kind === "editor").length)).toBeGreaterThanOrEqual(2);
  const ed = await page.evaluate(() => window.__illogical.fleet.panes.find((p) => p.info.kind === "editor" && p.info.file)!);
  expect(ed.info.project?.name).toBe("illogical");
  expect(ed.info.file).toBe(AUTH);
  // Its preview: the file and the lines around the cursor.
  await page.waitForTimeout(1500);
  const at = (key: string) => page.evaluate((k) => (window.__illogical.swarm as { screenOf(k: string): { x: number; y: number } }).screenOf(k), key);
  let pos = (await at(ed.key))!;
  await page.mouse.move(pos.x, pos.y);
  await expect(page.locator(".swarm-peek pre")).toContainText(`${AUTH}:`, { timeout: 10_000 });

  // Right-click the terminal's tile: VS Code where it runs.
  const before = (await editors(page)).length;
  const key = await page.evaluate((t) => window.__illogical.fleet.panes.find((p) => p.id === t)!.key, term);
  pos = (await at(key))!;
  await page.mouse.click(pos.x, pos.y, { button: "right" });
  await page.getByRole("menuitem", { name: "Open in editor" }).click();
  await expect(page.locator(".swarm")).toHaveCount(0);
  await expect.poll(async () => (await editors(page)).length).toBe(before + 1);
  const made = (await editors(page)).at(-1)!;
  await expect.poll(() => page.evaluate(() => window.__illogical.client.active())).toBe(made);
  await expect(frame(page, made).locator(".monaco-workbench")).toBeVisible({ timeout: 30_000 });
  expect((await blockState(page, made))?.folder).toBe(proj);
});

test("the block survives a daemon restart, and a reboot, with the file still open", async ({ page }) => {
  test.setTimeout(120_000);
  await page.goto("/");
  await expect.poll(() => page.evaluate(() => window.__illogical?.client.connected)).toBe(true);
  const block = JSON.parse((await cli("edit", `${AUTH}:${LATER}`)).stdout).block as PaneId;
  await expect.poll(() => page.evaluate((b) => !!window.__illogical.client.info(b), block)).toBe(true);
  await page.evaluate((b) => window.__illogical.client.setActive(b), block);
  await expect(shown(frame(page, block), "STATE_COOKIE")).toBeVisible({ timeout: 10_000 });
  await expect.poll(async () => (await blockState(page, block))?.line).toBe(LATER);

  // A restart: the page reconnects, the same VS Code session carries on.
  await stop();
  await start();
  await expect.poll(() => page.evaluate(() => window.__illogical.client.connected), { timeout: 20_000 }).toBe(true);
  await expect.poll(() => page.evaluate((b) => window.__illogical.client.info(b)?.file, block), { timeout: 20_000 }).toBe(AUTH);
  await page.evaluate((b) => window.__illogical.client.setActive(b), block);
  const f = frame(page, block);
  await expect(f.locator(".tab.active", { hasText: "auth.rs" })).toBeVisible({ timeout: 30_000 });
  await expect(shown(f, "STATE_COOKIE")).toBeVisible();

  // A reboot: code-server is gone too. The block comes back and asks a
  // new server for the file.
  await stop();
  stopServer();
  await start();
  await page.reload();
  await expect.poll(() => page.evaluate(() => window.__illogical?.client.connected), { timeout: 20_000 }).toBe(true);
  await expect.poll(() => page.evaluate((b) => !!window.__illogical.client.info(b), block), { timeout: 20_000 }).toBe(true);
  await page.evaluate((b) => window.__illogical.client.setActive(b), block);
  const g = frame(page, block);
  await expect(g.locator(".tab.active", { hasText: "auth.rs" })).toBeVisible({ timeout: 60_000 });
  await expect(shown(g, "STATE_COOKIE")).toBeVisible();
});

test("a viewer can't open one", async ({ browser, page }) => {
  await page.goto("/");
  await expect.poll(() => page.evaluate(() => window.__illogical?.client.connected)).toBe(true);
  const session = await page.evaluate((t) => window.__illogical.client.state!.panes.find((p) => p.id === t) && window.__illogical.client.session, term);
  const acl = await fetch(`${APP}/api/acl`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ session, principal: `tailnet:${FRIEND}`, role: "viewer" }),
  });
  expect(acl.ok).toBe(true);
  const before = (await editors(page)).length;
  const friend = await (await browser.newContext({ extraHTTPHeaders: { "tailscale-user-login": FRIEND }, baseURL: APP })).newPage();
  await friend.goto("/");
  await expect.poll(() => friend.evaluate(() => window.__illogical?.client.role())).toBe("viewer");
  await friend.evaluate((t) => window.__illogical.client.setActive(t), term);
  await ready(friend, term);
  // Not on the pane's menu...
  await paneEl(friend, term).click({ button: "right", position: { x: 60, y: 60 } });
  await expect(friend.getByRole("menuitem", { name: "Split right" })).toBeVisible();
  await expect(friend.getByRole("menuitem", { name: "Open in editor" })).toHaveCount(0);
  await friend.keyboard.press("Escape");
  // ...and refused if asked anyway.
  const res = await friend.evaluate(async (t) => (await window.__illogical.client.request("POST", "/api/blocks", { type: "editor", config: {}, from_pane: t, split: t })).status, term);
  expect([400, 403]).toContain(res);
  expect((await editors(page)).length).toBe(before);
});
