// M3c: a VM tab's panes share one machine. Splits join it (or run here, if
// asked), its panes can't be dragged away from it, a pane's own machine can
// be given to its tab, it can be reset, it comes back as one fresh machine
// after a reboot, and closing the tab deletes it. Needs wispd and its token
// on this host; elsewhere these skip.

import { spawn, type ChildProcess } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { homedir, hostname, tmpdir } from "node:os";
import { join } from "node:path";
import { devices, expect, test, type Page } from "@playwright/test";
import { dragTo, menu, open, paneEl, screen, text, type as typeIn } from "./helpers";
import type { PaneId } from "../src/proto";
import { ANY, daemonPort } from "./ports";
import { labs } from "./labs";

let PORT = 0;
const WISP = process.env.ILLOGICAL_WISP_URL ?? "http://127.0.0.1:7788";
const token = (() => {
  try {
    return readFileSync(process.env.ILLOGICAL_WISP_TOKEN_FILE ?? `${homedir()}/.local/share/wisp/token`, "utf8").trim();
  } catch {
    return "";
  }
})();
let state = "";
test.use({ baseURL: async ({}, use) => use(`http://127.0.0.1:${PORT}`) });
test.describe.configure({ mode: "serial" });

/** Start the daemon: on a port of its choosing, then on the same one again. */
async function startDaemon(): Promise<ChildProcess> {
  const d = spawn(
    "../target/debug/illogicald",
    ["--listen", PORT ? `127.0.0.1:${PORT}` : ANY, "--shell", "bash --norc --noprofile", "--no-manager-env", "--state-dir", labs(state)],
    { stdio: "ignore" },
  );
  PORT ||= await daemonPort(state, d);
  for (let i = 0; i < 100; i++) {
    try {
      if ((await fetch(`http://127.0.0.1:${PORT}/`)).ok) return d;
    } catch {
      // not up yet
    }
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error("daemon did not start");
}

async function stopDaemon(d: ChildProcess) {
  const exited = new Promise((r) => d.once("exit", r));
  d.kill("SIGTERM");
  await exited;
}

const wisp = (method: string, path: string) =>
  fetch(`${WISP}/v1/sprites${path}`, { method, headers: { Authorization: `Bearer ${token}` } });
const spriteExists = async (name: string) => (await wisp("GET", `/${name}`)).status === 200;

let daemon: ChildProcess | undefined;
test.beforeAll(async () => {
  state = mkdtempSync(join(tmpdir(), "ilg-e2e-tm-"));
  if (token) daemon = await startDaemon();
});
test.afterAll(async () => {
  daemon?.kill("SIGKILL");
  // Anything a failed test left behind.
  const id = (() => {
    try {
      return readFileSync(join(state, "daemon-id"), "utf8").trim();
    } catch {
      return null;
    }
  })();
  if (id && token) {
    const list = await (await wisp("GET", `?prefix=illogical-eph-${id}-`)).json();
    for (const s of list.sprites ?? []) await wisp("DELETE", `/${s.name}`);
  }
  if (state) rmSync(state, { recursive: true, force: true });
});

const panesOf = (page: Page) => page.evaluate(() => window.__illogical.client.state!.panes);
const machines = (page: Page) => page.evaluate(() => window.__illogical.client.state!.machines);
const activePane = (page: Page) => page.evaluate(() => window.__illogical.client.active()!);
const host = (page: Page, p: PaneId) => page.evaluate((p) => window.__illogical.client.info(p)?.host ?? null, p);
const cwd = (page: Page, p: PaneId) => page.evaluate((p) => window.__illogical.client.info(p)?.cwd ?? null, p);

/** A pane's text with soft wraps undone (narrow panes wrap our notes). */
const flat = async (page: Page, p: PaneId) => (await text(page, p)).replace(/\n/g, "");

/** An item from a tab's right-click menu. */
async function tabMenu(page: Page, tab: number, item: string) {
  await page.locator(`.tab[data-tab-id="${tab}"]`).click({ button: "right" });
  await page.getByRole("menuitem", { name: item }).click();
}

/** Type into a pane, showing it first (a phone shows one at a time). */
async function type(page: Page, p: PaneId, s: string) {
  await page.evaluate((p) => {
    const c = window.__illogical.client;
    const t = c.tabOfPane(p)!;
    c.selectTab(t.id);
    c.setActive(p);
  }, p);
  await expect(paneEl(page, p)).toBeVisible();
  await typeIn(page, p, s);
}

/** The pane that appears next, after `act`. */
async function next(page: Page, act: () => Promise<unknown>): Promise<PaneId> {
  const before = (await panesOf(page)).map((p) => p.id);
  await act();
  let id: PaneId | undefined;
  await expect
    .poll(async () => {
      id = (await panesOf(page)).find((p) => !before.includes(p.id))?.id;
      return id ?? null;
    })
    .not.toBeNull();
  return id!;
}

let sprite = "";
let vmA = 0;
let vmB = 0;
let local = 0;

test.describe("phone", () => {
  const { viewport, userAgent, deviceScaleFactor, isMobile, hasTouch } = devices["Pixel 7"];
  test.use({ viewport, userAgent, deviceScaleFactor, isMobile, hasTouch });

  test("a VM tab's splits share its machine; a local split runs here", async ({ page }) => {
    test.skip(!token, "no wisp token on this host");
    await open(page);
    const sheet = () => page.locator(".sheet-button").click();
    vmA = await next(page, async () => {
      await sheet();
      await page.getByRole("button", { name: "New VM tab" }).click();
    });
    const [m] = await machines(page);
    sprite = m.sprite;
    expect(m.owner).toEqual({ tab: await page.evaluate((p) => window.__illogical.client.tabOfPane(p)!.id, vmA) });
    await expect.poll(() => cwd(page, vmA)).toBe("/home/sprite");

    vmB = await next(page, async () => {
      await sheet();
      await page.getByRole("button", { name: "Split pane" }).click();
    });
    expect(await host(page, vmB)).toBe(m.id);
    await expect.poll(() => cwd(page, vmB)).toBe("/home/sprite");
    // Same machine, same files.
    await type(page, vmA, "echo shared-$((40+2)) > ~/shared.txt\n");
    await type(page, vmB, "sleep 0.5; cat ~/shared.txt; hostname\n");
    await expect.poll(() => text(page, vmB)).toContain("shared-42");
    expect(await text(page, vmB)).toContain(sprite);

    local = await next(page, async () => {
      await sheet();
      await page.getByRole("button", { name: "Split (local)" }).click();
    });
    expect(await host(page, local)).toBeNull();
    await page.evaluate((p) => window.__illogical.client.setActive(p), local);
    await expect(paneEl(page, local).locator(".host-badge.local")).toBeVisible();
    await type(page, local, "hostname\n");
    await expect.poll(() => text(page, local)).toContain(hostname());
    // Only the local pane is badged; the tab carries the machine.
    await page.evaluate((p) => window.__illogical.client.setActive(p), vmB);
    await expect(paneEl(page, vmB).locator(".host-badge")).toHaveCount(0);
  });
});

test("a VM pane can't be dragged out of its tab; a local one can", async ({ page }) => {
  test.skip(!token, "no wisp token on this host");
  await open(page);
  await page.evaluate((p) => window.__illogical.client.setActive(p), vmB);
  // The right half of the last tab: a new tab after it.
  const endOfTabs = async () => {
    const b = (await page.locator(".tabbar .tab").last().boundingBox())!;
    return { x: b.x + b.width * 0.85, y: b.y + b.height / 2 };
  };
  const tabsBefore = await page.evaluate(() => window.__illogical.client.state!.tabs.length);

  await paneEl(page, vmB).hover();
  await dragTo(page, paneEl(page, vmB).locator(".grip"), await endOfTabs());
  await expect.poll(() => page.evaluate(() => window.__illogical.client.error)).toContain("runs on this tab's machine");
  expect(await page.evaluate(() => window.__illogical.client.state!.tabs.length)).toBe(tabsBefore);
  expect(await host(page, vmB)).toBe((await machines(page))[0].id);

  // The refusal's message sits over the tab bar for a few seconds.
  await expect.poll(() => page.evaluate(() => window.__illogical.client.error), { timeout: 10_000 }).toBeNull();
  // Grabbing a pane makes it active, which retitles the tab: settle that
  // before aiming at the tab.
  await page.evaluate((p) => window.__illogical.client.setActive(p), local);
  await page.waitForTimeout(200);
  await paneEl(page, local).hover();
  await dragTo(page, paneEl(page, local).locator(".grip"), await endOfTabs());
  await expect.poll(() => page.evaluate(() => window.__illogical.client.state!.tabs.length)).toBe(tabsBefore + 1);
});

test("reset: the panes start again on a new machine", async ({ page }) => {
  test.skip(!token, "no wisp token on this host");
  await open(page);
  const tab = await page.evaluate((p) => window.__illogical.client.tabOfPane(p)!.id, vmA);
  await page.evaluate((t) => window.__illogical.client.selectTab(t), tab);
  await tabMenu(page, tab, "Reset machine");
  for (const p of [vmA, vmB]) {
    await expect.poll(() => flat(page, p)).toContain("── machine reset ──");
    await expect.poll(() => screen(page, p)).toMatch(/sprite@\S+:~\$\s*$/m);
  }
  await type(page, vmA, "cat ~/shared.txt 2>&1 | head -1\n");
  await expect.poll(() => flat(page, vmA)).toContain("No such file");
  expect(await spriteExists(sprite)).toBe(true);
});

test("after a reboot the tab gets one fresh machine and each pane restores", async ({ page }) => {
  test.skip(!token, "no wisp token on this host");
  await open(page);
  await type(page, vmA, "echo before-reboot-$((6*7)) > ~/mark.txt\n");
  await expect.poll(() => text(page, vmA)).toContain("echo before-reboot");
  const [before] = await machines(page);
  // A reboot of the VM host: the daemon stops, the sprite is gone.
  await stopDaemon(daemon!);
  expect((await wisp("DELETE", `/${sprite}`)).status).toBe(204);
  daemon = await startDaemon();
  await open(page);
  const ms = await machines(page);
  expect(ms.map((m) => m.id)).toEqual([before.id]);
  for (const p of [vmA, vmB]) {
    await expect.poll(() => flat(page, p)).toContain("the machine was lost; this is a new one");
    await expect.poll(() => cwd(page, p)).toBe("/home/sprite");
  }
  await expect.poll(() => page.evaluate(() => window.__illogical.client.state!.machines[0].state)).toBe("running");
  await type(page, vmB, "cat ~/mark.txt 2>&1 | head -1\n");
  await expect.poll(() => flat(page, vmB)).toContain("No such file");
  // One sprite, not one per pane.
  const id = readFileSync(join(state, "daemon-id"), "utf8").trim();
  const list = await (await wisp("GET", `?prefix=illogical-eph-${id}-`)).json();
  expect(list.sprites.map((s: { name: string }) => s.name)).toEqual([sprite]);
});

test("a pane's own machine can be shared with its tab, and new splits join it", async ({ page }) => {
  test.skip(!token, "no wisp token on this host");
  await open(page);
  const own = await next(page, () => page.evaluate(() => fetch("/api/run", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ vm: true }) })));
  await expect.poll(() => cwd(page, own)).toBe("/home/sprite");
  const m = (await machines(page)).find((m) => "pane" in m.owner && m.owner.pane === own)!;
  await page.evaluate((p) => window.__illogical.client.setActive(p), own);
  await expect(paneEl(page, own).locator(".host-badge")).toHaveText("VM");
  await menu(page, paneEl(page, own), "Share machine with tab");
  await expect.poll(async () => (await machines(page)).find((x) => x.id === m.id)?.owner).toHaveProperty("tab");
  const joined = await next(page, () => menu(page, paneEl(page, own), "Split right"));
  expect(await host(page, joined)).toBe(m.id);
  await expect.poll(() => cwd(page, joined)).toBe("/home/sprite");

  // Closing that tab deletes its machine.
  const tab = await page.evaluate((p) => window.__illogical.client.tabOfPane(p)!.id, own);
  await page.evaluate((t) => window.__illogical.client.intent({ op: "close_tab", tab: t }), tab);
  await expect.poll(async () => (await machines(page)).some((x) => x.id === m.id)).toBe(false);
  await expect.poll(() => spriteExists(m.sprite)).toBe(false);
});

test("closing the VM tab deletes its machine", async ({ page }) => {
  test.skip(!token, "no wisp token on this host");
  await open(page);
  const tab = await page.evaluate((p) => window.__illogical.client.tabOfPane(p)!.id, vmA);
  await page.evaluate((t) => window.__illogical.client.selectTab(t), tab);
  await tabMenu(page, tab, "Close tab and machine");
  await expect.poll(() => machines(page)).toEqual([]);
  await expect.poll(() => spriteExists(sprite)).toBe(false);
  // (Its log may be moving to closed/ just then; ask until it answers.)
  await expect
    .poll(() => page.evaluate((p) => fetch(`/api/panes/${p}/tail?from=0&text=1`).then((r) => r.text()), vmB))
    .toContain("shared-42");
});
