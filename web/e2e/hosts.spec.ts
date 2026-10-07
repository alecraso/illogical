// M4a: one page, several daemons. The page comes from the home daemon,
// which lists another; switching to it connects straight there (its own
// layout and scrollback), on a phone too, and the saved list keeps working
// when the home daemon is down.

import { spawn, type ChildProcess } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { devices, expect, test, type Page } from "@playwright/test";
import { active, menu, paneEl, panes, ready, run, text } from "./helpers";
import { ANY, daemonPort } from "./ports";
import { labs } from "./labs";

let homeUrl = "";
let otherUrl = "";
const states: string[] = [];
const daemons = new Map<string, ChildProcess>();

test.use({ baseURL: async ({}, use) => use(homeUrl) });
test.describe.configure({ mode: "serial" });

/** Start a daemon; its URL. */
async function startDaemon(name: string, extra: string[] = []) {
  const state = mkdtempSync(join(tmpdir(), `arugula-e2e-hosts-${name}-`));
  states.push(state);
  const d = spawn(
    "../target/debug/arugulad",
    [
      ...["--listen", ANY, "--name", name, "--state-dir", labs(state)],
      ...["--shell", "bash --norc --noprofile", "--no-manager-env", "--tailscale-socket", "/nonexistent/tailscaled.sock"],
      ...extra,
    ],
    { stdio: "ignore" },
  );
  daemons.set(name, d);
  const url = `http://127.0.0.1:${await daemonPort(state, d)}`;
  for (let i = 0; i < 100; i++) {
    try {
      if ((await fetch(`${url}/api/host`)).ok) return url;
    } catch {
      // not up yet
    }
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error(`daemon ${name} did not start`);
}

async function stopDaemon(name: string) {
  const d = daemons.get(name);
  if (!d) return;
  const exited = new Promise((r) => d.once("exit", r));
  d.kill("SIGTERM");
  await exited;
  daemons.delete(name);
}

test.beforeAll(async () => {
  homeUrl = await startDaemon("home");
  // The other daemon accepts the home daemon's page, exactly.
  otherUrl = await startDaemon("other", ["--allow-origin", homeUrl]);
  const res = await fetch(`${homeUrl}/api/hosts`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ name: "other", urls: [otherUrl] }),
  });
  expect(res.ok).toBe(true);
});

test.afterAll(() => {
  for (const d of daemons.values()) d.kill("SIGKILL");
  for (const s of states) rmSync(s, { recursive: true, force: true });
});

const base = (page: Page) => page.evaluate(() => window.__arugula?.client.base);
const connected = (page: Page) =>
  page.evaluate(() => !!window.__arugula?.client.connected && window.__arugula.client.state !== null);
const remotePanes = async () => ((await (await fetch(`${otherUrl}/api/panes`)).json()) as { id: number }[]).map((p) => p.id);
const homePanes = async () => ((await (await fetch(`${homeUrl}/api/panes`)).json()) as { id: number }[]).map((p) => p.id);

async function switchTo(page: Page, name: string) {
  await page.locator(".host-button").click();
  await page.getByRole("menuitem", { name: new RegExp(`^\\s*(✓ )?${name}\\b`) }).click();
}

test("switch to another host: its own layout, connected straight to it", async ({ page }) => {
  await page.goto("/");
  await expect.poll(() => connected(page)).toBe(true);
  await expect(page.locator(".host-button")).toHaveText(/home/);
  const homeFirst = await active(page);

  await switchTo(page, "other");
  await expect.poll(() => base(page)).toBe(otherUrl);
  await expect.poll(() => connected(page)).toBe(true);
  await expect(page.locator(".host-button")).toHaveText(/other/);
  const pane = await active(page);
  await ready(page, pane);
  await run(page, pane, "echo on-other-$((6*7))", "on-other-42");
  // It is the other daemon's pane, made by its own shell.
  expect(await remotePanes()).toContain(pane);

  // Its layout is its own: a split there doesn't touch home's.
  await menu(page, paneEl(page, pane), "Split right");
  await expect.poll(() => panes(page)).toHaveLength(2);
  await expect.poll(remotePanes).toHaveLength(2);
  expect(await homePanes()).toEqual([homeFirst]);

  // The API works across origins too (CORS for the home page only).
  const opened = await page.evaluate(() =>
    window.__arugula.client.api("/api/blocks", { type: "browser", config: { url: "https://example.com" } }),
  );
  expect(opened).toBe(true);
  await expect.poll(async () => (await remotePanes()).length).toBe(3);

  // Back home: one pane, as it was.
  await switchTo(page, "home");
  await expect.poll(() => base(page)).toBe("");
  await expect.poll(() => connected(page)).toBe(true);
  await expect.poll(() => page.evaluate(() => window.__arugula.client.state!.panes.length)).toBe(1);

  // And back again: the other host's scrollback is still there.
  await switchTo(page, "other");
  await expect.poll(() => connected(page)).toBe(true);
  await ready(page, pane);
  await expect.poll(() => text(page, pane)).toContain("on-other-42");

  // The shown host survives a reload.
  await page.reload();
  await expect.poll(() => connected(page)).toBe(true);
  expect(await base(page)).toBe(otherUrl);
});

test.describe("phone", () => {
  const { viewport, userAgent, deviceScaleFactor, isMobile, hasTouch } = devices["Pixel 7"];
  test.use({ viewport, userAgent, deviceScaleFactor, isMobile, hasTouch });

  test("hosts are listed in the sheet and switch from there", async ({ page }) => {
    await page.goto("/");
    await expect.poll(() => connected(page)).toBe(true);
    await page.evaluate(() => window.__arugula.hosts.select("home"));
    await expect.poll(() => base(page)).toBe("");
    await expect.poll(() => connected(page)).toBe(true);
    await page.locator(".sheet-button").click();
    await expect(page.locator(".sheet-host")).toHaveCount(2);
    await page.locator('.sheet-host[data-host="other"]').click();
    await expect.poll(() => base(page)).toBe(otherUrl);
    await expect.poll(() => connected(page)).toBe(true);
    await expect(page.locator(".host-crumb")).toHaveText("other");
    const pane = await active(page);
    await ready(page, pane);
    await run(page, pane, "echo phone-$((5*5))", "phone-25");
  });
});

test("with the home daemon down, the saved list still reaches the other host", async ({ page }) => {
  // Load once with home up (the service worker keeps the page), then stop it.
  await page.goto("/");
  await expect.poll(() => connected(page)).toBe(true);
  await page.evaluate(() => navigator.serviceWorker.ready);
  await page.reload(); // now controlled by the worker
  await expect.poll(() => connected(page)).toBe(true);
  await page.evaluate(() => window.__arugula.hosts.select("home"));
  await expect.poll(() => base(page)).toBe("");
  await stopDaemon("home");

  await page.reload();
  // The list is the saved one; home is unreachable, so it offers the others.
  await expect.poll(() => page.evaluate(() => window.__arugula?.hosts.stale)).toBe(true);
  await page.locator('.host-picker button[data-host="other"]').click();
  await expect.poll(() => base(page)).toBe(otherUrl);
  await expect.poll(() => connected(page)).toBe(true);
  const pane = await active(page);
  await ready(page, pane);
  await run(page, pane, "echo still-$((3*3))", "still-9");
});
