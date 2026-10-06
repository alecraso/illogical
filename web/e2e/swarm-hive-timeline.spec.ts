// M42: the hive and the timeline, two more swarm themes. Against the fake
// fleet (e2e/fake-fleet.ts): each draws the same clusters as blocks, keeps
// the grouping and the rail, is remembered, and passes the checks the
// field does (a hover peek, a click that opens the pane in its tab, a
// card's Show going to the pane, a tap on a phone). The hive fills a cell
// with what needs you; the timeline draws commands from each daemon's
// history, failed ones marked, and a band on what needs you.

import { devices, expect, test, type Page } from "@playwright/test";
import { FakeFleet } from "./fake-fleet";

let fake: FakeFleet;

test.use({ baseURL: async ({}, use) => use(fake?.machines[0]?.url) });
test.describe.configure({ mode: "serial" });

test.beforeAll(async () => {
  fake = new FakeFleet();
  await fake.machine("workstation");
  await fake.machine("build-01");
  await fake.machine("build-02");
  await fake.populate();
});

test.afterAll(() => fake?.stop());

interface Scene {
  clusters: { name: string; n: number; need: number }[];
  screenOf(k: string): { x: number; y: number } | null;
  cellOf?(k: string): { x: number; y: number } | null;
  runsOf?(k: string): { t0: number; t1: number; exit: number | null; text: string | null }[];
}
const scene = <T,>(page: Page, fn: (c: Scene, arg: string) => T, arg = "") =>
  page.evaluate(([f, a]) => new Function("c", "a", `return (${f})(c, a)`)(window.__illogical.swarm, a), [fn.toString(), arg] as const) as Promise<T>;
const isHive = (page: Page) => page.evaluate(() => !!(window.__illogical.swarm as { cellOf?: unknown } | null)?.cellOf);
const isTimeline = (page: Page) => page.evaluate(() => !!(window.__illogical.swarm as { runsOf?: unknown } | null)?.runsOf);

async function swarm(page: Page) {
  await page.goto("/#swarm");
  await expect
    .poll(() => page.evaluate(() => window.__illogical?.fleet.list.filter((h) => h.state === "connected").length ?? 0), { timeout: 20_000 })
    .toBe(3);
  await expect.poll(() => page.evaluate(() => window.__illogical.fleet.panes.length)).toBeGreaterThanOrEqual(12);
  await expect(page.locator(".swarm")).toBeVisible();
}

async function clearRail(page: Page) {
  await page.evaluate(async () => {
    const f = window.__illogical.fleet;
    for (const p of f.panes) {
      if (!p.info.reason) continue;
      await f.request(p.host, "POST", "/api/attention/act", { action: "dismiss", pane: p.id }).catch(() => {});
    }
  });
  await expect(page.locator(".swarm-card[data-bundle]")).toHaveCount(0, { timeout: 10_000 });
}

/** How far a pane is from the middle of the part of the screen the bar and rail leave free. */
async function offCentre(page: Page, key: string) {
  const pos = await scene(page, (c, k) => c.screenOf(k), key);
  const bar = (await page.locator(".swarm-bar").boundingBox())!;
  const rail = (await page.locator(".swarm-rail").boundingBox())!;
  const vp = page.viewportSize()!;
  if (!pos) return Infinity;
  const top = bar.y + bar.height + 10;
  return Math.abs(pos.y - (top + (vp.height - top) / 2)) + (pos.x > rail.x ? 1e4 : 0);
}

for (const theme of ["hive", "timeline"] as const) {
  test(`${theme}: the same clusters as blocks, regroups, is remembered, and goes back`, async ({ page }) => {
    await swarm(page);
    await page.locator('[data-g="machine"]').click();
    await page.waitForTimeout(500);
    const blocks = await page.evaluate(() => (window.__illogical.swarm as { clusters: { name: string }[] }).clusters.map((c) => c.name).sort());
    expect(blocks).toEqual(["build-01", "build-02", "workstation"]);

    await page.locator(`[data-theme-pick="${theme}"]`).click();
    await expect(page.locator(".swarm")).toHaveAttribute("data-theme", theme);
    await expect.poll(() => (theme === "hive" ? isHive(page) : isTimeline(page)), { timeout: 10_000 }).toBe(true);
    await expect(page.locator(`canvas.swarm-${theme}`)).toBeVisible();
    expect((await scene(page, (c) => c.clusters.map((x) => x.name))).sort()).toEqual(blocks);
    const n = await page.evaluate(() => window.__illogical.fleet.panes.length);
    expect(await scene(page, (c) => c.clusters.reduce((s, x) => s + x.n, 0))).toBe(n);
    await expect(page.locator(`[data-theme-key="${theme}"]`)).toBeVisible();
    await expect(page.locator(`[data-theme-key="${theme}"] summary`)).toHaveText(`How to read the ${theme}`);

    // Regrouping lays the panes out again in new clusters.
    const key = await page.evaluate(() => window.__illogical.fleet.panes.find((p) => p.host === "build-01")!.key);
    const before = await scene(page, (c, k) => (c.cellOf ? c.cellOf(k) : c.screenOf(k)), key);
    await page.locator('[data-g="project"]').click();
    await expect.poll(async () => (await scene(page, (c) => c.clusters.map((x) => x.name))).includes("api")).toBe(true);
    await expect.poll(async () => JSON.stringify(await scene(page, (c, k) => (c.cellOf ? c.cellOf(k) : c.screenOf(k)), key))).not.toBe(JSON.stringify(before));
    await page.locator('[data-g="machine"]').click();

    await page.reload();
    await expect(page.locator(".swarm")).toHaveAttribute("data-theme", theme);
    await expect.poll(() => (theme === "hive" ? isHive(page) : isTimeline(page)), { timeout: 10_000 }).toBe(true);

    await page.locator('[data-theme-pick="blocks"]').click();
    await expect(page.locator(".swarm")).toHaveAttribute("data-theme", "blocks");
    await expect(page.locator("canvas.swarm-blocks")).toBeVisible();
    await expect(page.locator("[data-theme-key]")).toHaveCount(0);
  });

  test(`${theme}: a pane peeks and opens its tab; Show goes to what needs you`, async ({ page }) => {
    await page.addInitScript((t) => localStorage.setItem("illogical.swarm.theme", t), theme);
    await swarm(page);
    await expect.poll(() => (theme === "hive" ? isHive(page) : isTimeline(page)), { timeout: 10_000 }).toBe(true);
    await clearRail(page);
    await page.locator('[data-g="machine"]').click();
    await page.waitForTimeout(1000);

    // Three failed tests on the workstation: one card, three panes that need you.
    const failed = await fake.trouble("workstation", 3);
    const card = page.locator(`.swarm-card[data-panes~="workstation:${failed[0]}"]`);
    await expect(card).toBeVisible({ timeout: 20_000 });
    await expect.poll(() => scene(page, (c) => c.clusters.find((x) => x.name === "workstation")?.need ?? 0)).toBe(3);
    if (theme === "timeline") {
      // Each failed run is drawn from the daemon's history (or seen to end), exit 101.
      await expect
        .poll(() => scene(page, (c, k) => c.runsOf!(k).some((r) => r.exit === 101), `workstation:${failed[0]}`), { timeout: 45_000 })
        .toBe(true);
    }
    await card.getByRole("button", { name: "Show", exact: true }).click();
    await expect.poll(() => offCentre(page, `workstation:${failed[0]}`), { timeout: 5_000 }).toBeLessThan(30);
    await page.locator("[data-fit]").click();
    await clearRail(page);
    await expect.poll(() => scene(page, (c) => c.clusters.reduce((s, x) => s + x.need, 0))).toBe(0);

    // A pane: hover peeks at its last lines, a click opens its tab.
    const key = await page.evaluate(() => window.__illogical.fleet.panes.find((p) => p.host === "build-01" && p.info.kind === "test")!.key);
    await page.waitForTimeout(1200);
    let pos = (await scene(page, (c, k) => c.screenOf(k), key))!;
    await page.mouse.move(pos.x, pos.y);
    await expect(page.locator(".swarm-peek pre")).toContainText("test vt::parser::case_", { timeout: 10_000 });
    pos = (await scene(page, (c, k) => c.screenOf(k), key))!;
    await page.mouse.click(pos.x, pos.y);
    await expect(page.locator(".swarm")).toHaveCount(0);
    await expect.poll(() => page.evaluate(() => window.__illogical.hosts.current)).toBe("build-01");
    await expect.poll(() => page.evaluate(() => window.__illogical.client.active())).toBe(Number(key.split(":")[1]));
  });
}

test("timeline: a build that finished is a bar from history, as long as it ran", async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem("illogical.swarm.theme", "timeline"));
  const pane = await fake.finish("build-02");
  await swarm(page);
  await expect.poll(() => isTimeline(page), { timeout: 10_000 }).toBe(true);
  await expect
    .poll(() => scene(page, (c, k) => c.runsOf!(k).filter((r) => r.exit === 0).length, `build-02:${pane}`), { timeout: 45_000 })
    .toBeGreaterThan(0);
  const r = (await scene(page, (c, k) => c.runsOf!(k), `build-02:${pane}`)).find((x) => x.exit === 0)!;
  // The stand-in build prints for about six seconds.
  expect((r.t1 - r.t0) / 1000).toBeGreaterThan(3);
  expect((r.t1 - r.t0) / 1000).toBeLessThan(30);
});

test.describe("phone", () => {
  const { viewport, userAgent, deviceScaleFactor, isMobile, hasTouch } = devices["Pixel 7"];
  test.use({ viewport, userAgent, deviceScaleFactor, isMobile, hasTouch });

  for (const theme of ["hive", "timeline"] as const) {
    test(`${theme} on a phone: cards along the bottom, a tap opens a pane`, async ({ page }) => {
      await page.addInitScript((t) => localStorage.setItem("illogical.swarm.theme", t), theme);
      await swarm(page);
      await expect.poll(() => (theme === "hive" ? isHive(page) : isTimeline(page)), { timeout: 10_000 }).toBe(true);
      const rail = await page.locator(".swarm-rail").boundingBox();
      expect(rail!.y).toBeGreaterThan(viewport.height / 2);
      const key = await page.evaluate(() => window.__illogical.fleet.panes.find((p) => p.host === "build-02")!.key);
      await page.evaluate((k) => (window.__illogical.swarm as unknown as { diveTo(k: string): void }).diveTo(k), key);
      await page.waitForTimeout(1200);
      const pos = (await scene(page, (c, k) => c.screenOf(k), key))!;
      expect(pos.y).toBeLessThan(rail!.y);
      await page.touchscreen.tap(pos.x, pos.y);
      await expect(page.locator(".swarm")).toHaveCount(0);
      await expect.poll(() => page.evaluate(() => window.__illogical.hosts.current)).toBe("build-02");
    });
  }
});
