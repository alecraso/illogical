// M41: the swarm's themes. Blocks (the field) is the default; the city
// draws the same panes in 3D, loaded only when picked. Against the fake
// fleet (e2e/fake-fleet.ts): the city keeps the grouping and the rail, is
// remembered, and passes the checks the field does: clusters, a hover peek,
// a click that opens the pane in its tab, a card's Show and a
// notification's deep link flying to the pane, a beam on what needs you,
// and a tap on a phone.

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

interface CityScene {
  clusters: { name: string; n: number }[];
  screenOf(k: string): { x: number; y: number } | null;
  target: { x: number; z: number };
  lotOf(k: string): { x: number; z: number } | null;
  beamKeys: string[];
  heightOf(k: string): number | null;
}
const city = <T,>(page: Page, fn: (c: CityScene, arg: string) => T, arg = "") =>
  page.evaluate(([f, a]) => new Function("c", "a", `return (${f})(c, a)`)(window.__arugula.swarm, a), [fn.toString(), arg] as const) as Promise<T>;
const isCity = (page: Page) => page.evaluate(() => !!(window.__arugula.swarm as { lotOf?: unknown } | null)?.lotOf);

async function swarm(page: Page) {
  await page.goto("/#swarm");
  await expect
    .poll(() => page.evaluate(() => window.__arugula?.fleet.list.filter((h) => h.state === "connected").length ?? 0), { timeout: 20_000 })
    .toBe(3);
  await expect.poll(() => page.evaluate(() => window.__arugula.fleet.panes.length)).toBeGreaterThanOrEqual(12);
  await expect(page.locator(".swarm")).toBeVisible();
}

async function clearRail(page: Page) {
  await page.evaluate(async () => {
    const f = window.__arugula.fleet;
    for (const p of f.panes) {
      if (!p.info.reason) continue;
      await f.request(p.host, "POST", "/api/attention/act", { action: "dismiss", pane: p.id }).catch(() => {});
    }
  });
  await expect(page.locator(".swarm-card[data-bundle]")).toHaveCount(0, { timeout: 10_000 });
}

/** The camera looks at (near) a pane's lot. */
async function lookingAt(page: Page, key: string) {
  const [t, lot] = await Promise.all([city(page, (c) => c.target), city(page, (c, k) => c.lotOf(k), key)]);
  return lot ? Math.hypot(t.x - lot.x, t.z - lot.z) : Infinity;
}

const loadedCity = (page: Page) => page.evaluate(() => performance.getEntriesByType("resource").some((r) => /\/city-[^/]*\.js/.test(r.name)));

test("blocks by default; the city draws the same clusters, is remembered, and goes back", async ({ page }) => {
  await swarm(page);
  await expect(page.locator(".swarm")).toHaveAttribute("data-theme", "blocks");
  await expect(page.locator('[data-theme-pick="blocks"]')).toHaveAttribute("aria-pressed", "true");
  // three.js isn't fetched until the city is picked.
  expect(await loadedCity(page)).toBe(false);
  await page.locator('[data-g="machine"]').click();
  await page.waitForTimeout(500);
  const blocks = await page.evaluate(() => (window.__arugula.swarm as { clusters: { name: string }[] }).clusters.map((c) => c.name).sort());
  expect(blocks).toEqual(["build-01", "build-02", "workstation"]);

  await page.locator('[data-theme-pick="city"]').click();
  await expect(page.locator(".swarm")).toHaveAttribute("data-theme", "city");
  await expect.poll(() => isCity(page), { timeout: 10_000 }).toBe(true);
  expect(await loadedCity(page)).toBe(true);
  await expect(page.locator("canvas.swarm-city")).toBeVisible();
  await expect(page.locator("canvas.swarm-blocks")).toHaveCount(0);
  // Same grouping, same clusters, same pane count.
  expect((await city(page, (c) => c.clusters.map((x) => x.name))).sort()).toEqual(blocks);
  const n = await page.evaluate(() => window.__arugula.fleet.panes.length);
  expect(await city(page, (c) => c.clusters.reduce((s, x) => s + x.n, 0))).toBe(n);
  await expect(page.locator("[data-city-key]")).toBeVisible();
  // Regrouping moves the buildings to new blocks.
  await page.locator('[data-g="project"]').click();
  await expect.poll(async () => (await city(page, (c) => c.clusters.map((x) => x.name))).includes("api")).toBe(true);
  await page.locator('[data-g="machine"]').click();

  // Remembered across a reload.
  await page.reload();
  await expect(page.locator(".swarm")).toHaveAttribute("data-theme", "city");
  await expect.poll(() => isCity(page), { timeout: 10_000 }).toBe(true);

  await page.locator('[data-theme-pick="blocks"]').click();
  await expect(page.locator(".swarm")).toHaveAttribute("data-theme", "blocks");
  await expect.poll(() => isCity(page)).toBe(false);
  await expect(page.locator("canvas.swarm-blocks")).toBeVisible();
  await expect(page.locator("[data-city-key]")).toHaveCount(0);
});

test("a building peeks and opens its tab; Show and a notification fly to it; a beam stands on what needs you", async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem("arugula.swarm.theme", "city"));
  await swarm(page);
  await expect.poll(() => isCity(page), { timeout: 10_000 }).toBe(true);
  await clearRail(page);
  await page.locator('[data-g="machine"]').click();
  await page.waitForTimeout(1800);

  // Trouble on the workstation: three failed tests, one card, three beams.
  const failed = await fake.trouble("workstation", 3);
  const card = page.locator(`.swarm-card[data-panes~="workstation:${failed[0]}"]`);
  await expect(card).toBeVisible({ timeout: 20_000 });
  await expect.poll(() => city(page, (c) => c.beamKeys.length)).toBe(3);
  // A card's Show flies to its pane.
  await card.getByRole("button", { name: "Show", exact: true }).click();
  await expect.poll(() => lookingAt(page, `workstation:${failed[0]}`), { timeout: 5_000 }).toBeLessThan(0.5);
  await page.locator("[data-fit]").click();
  await clearRail(page);
  await expect.poll(() => city(page, (c) => c.beamKeys.length)).toBe(0);

  // A building: hover peeks at its last lines, a click opens its tab.
  const key = await page.evaluate(() => window.__arugula.fleet.panes.find((p) => p.host === "build-01" && p.info.kind === "test")!.key);
  expect(await city(page, (c, k) => c.heightOf(k)!, key)).toBeGreaterThan(0.1);
  await page.waitForTimeout(1600);
  let pos = (await city(page, (c, k) => c.screenOf(k), key))!;
  await page.mouse.move(pos.x, pos.y);
  await expect(page.locator(".swarm-peek pre")).toContainText("test vt::parser::case_", { timeout: 10_000 });
  pos = (await city(page, (c, k) => c.screenOf(k), key))!;
  await page.mouse.click(pos.x, pos.y);
  await expect(page.locator(".swarm")).toHaveCount(0);
  await expect.poll(() => page.evaluate(() => window.__arugula.hosts.current)).toBe("build-01");
  await expect.poll(() => page.evaluate(() => window.__arugula.client.active())).toBe(Number(key.split(":")[1]));

  // A notification's deep link opens the city at the pane's card, and flies to it.
  const [pane] = await fake.trouble("workstation", 1);
  await page.goto(`/#swarm=${pane}`);
  await expect(page.locator(`.swarm-card.focus[data-panes~="workstation:${pane}"]`)).toBeVisible({ timeout: 20_000 });
  await expect.poll(() => isCity(page), { timeout: 10_000 }).toBe(true);
  await expect.poll(() => lookingAt(page, `workstation:${pane}`), { timeout: 8_000 }).toBeLessThan(0.5);
  await clearRail(page);
});

test.describe("phone", () => {
  const { viewport, userAgent, deviceScaleFactor, isMobile, hasTouch } = devices["Pixel 7"];
  test.use({ viewport, userAgent, deviceScaleFactor, isMobile, hasTouch });

  test("the city on a phone: cards along the bottom, a tap opens a building", async ({ page }) => {
    await page.addInitScript(() => localStorage.setItem("arugula.swarm.theme", "city"));
    await swarm(page);
    await expect.poll(() => isCity(page), { timeout: 10_000 }).toBe(true);
    const rail = await page.locator(".swarm-rail").boundingBox();
    expect(rail!.y).toBeGreaterThan(viewport.height / 2);
    const key = await page.evaluate(() => window.__arugula.fleet.panes.find((p) => p.host === "build-02" && /cargo build/.test(p.info.command ?? p.info.current?.text ?? ""))?.key ?? window.__arugula.fleet.panes.find((p) => p.host === "build-02")!.key);
    await city(page, (c, k) => (window.__arugula.swarm as unknown as { diveTo(k: string): void }).diveTo(k), key);
    await page.waitForTimeout(1800);
    const pos = (await city(page, (c, k) => c.screenOf(k), key))!;
    await page.touchscreen.tap(pos.x, pos.y);
    await expect(page.locator(".swarm")).toHaveCount(0);
    await expect.poll(() => page.evaluate(() => window.__arugula.hosts.current)).toBe("build-02");
  });
});
