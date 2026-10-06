// M26: the swarm's frame rate at 500 panes, as S16 measured the prototype:
// at least about 60 fps on the laptop, and 30 on a phone-sized viewport with
// the CPU slowed 4x (a stand-in until a real phone). The panes are the
// synthetic fleet (src/swarm/fake.ts), drawn with physics awake throughout.

import { devices, expect, test, type Page } from "@playwright/test";
import { open, closeContexts } from "./helpers";

test.afterAll(closeContexts);

/** What each theme's scene has that the others don't, to know it's drawn. */
const MARK = { city: "lotOf", hive: "cellOf", timeline: "runsOf" } as const;

async function measure(page: Page, theme: "blocks" | "city" | "hive" | "timeline" = "blocks") {
  await page.addInitScript(() => localStorage.setItem("illogical.more", "1"));
  await page.addInitScript((t) => localStorage.setItem("illogical.swarm.theme", t), theme);
  await page.goto("/#swarm");
  await open(page).catch(() => {});
  await page.goto("/#swarm");
  await expect(page.locator(".swarm")).toBeVisible();
  await page.evaluate(() => void window.__illogical.swarmFake(500));
  await page.waitForTimeout(3000);
  if (theme !== "blocks") {
    await expect.poll(() => page.evaluate((m) => !!(window.__illogical.swarm as Record<string, unknown> | null)?.[m], MARK[theme]), { timeout: 10_000 }).toBe(true);
    await page.waitForTimeout(2000);
    await page.screenshot({ path: `test-results/${theme}-500.png` });
  }
  return page.evaluate(() => (window.__illogical.swarm as { measure(ms: number): Promise<{ fps: number; workP50: number; frames: number }> }).measure(5000));
}

test("500 panes at 60 fps on the laptop", async ({ browser }) => {
  const page = await browser.newPage({ viewport: { width: 1400, height: 860 } });
  const m = await measure(page);
  console.log(`laptop, 500 panes: ${m.fps.toFixed(1)} fps, work p50 ${m.workP50.toFixed(2)} ms over ${m.frames} frames`);
  expect(m.fps).toBeGreaterThanOrEqual(55);
  await page.close();
});

test("500 panes at 30 fps on a phone with the CPU slowed 4x", async ({ browser }) => {
  const { viewport, userAgent, deviceScaleFactor, isMobile, hasTouch } = devices["Pixel 7"];
  const ctx = await browser.newContext({ viewport, userAgent, deviceScaleFactor, isMobile, hasTouch });
  const page = await ctx.newPage();
  const cdp = await ctx.newCDPSession(page);
  await cdp.send("Emulation.setCPUThrottlingRate", { rate: 4 });
  const m = await measure(page);
  console.log(`phone (4x), 500 panes: ${m.fps.toFixed(1)} fps, work p50 ${m.workP50.toFixed(2)} ms over ${m.frames} frames`);
  expect(m.fps).toBeGreaterThanOrEqual(30);
  await ctx.close();
});

// M41: the city at 500 panes. WebGL in headless Chromium is software
// (SwiftShader), so this is a floor that catches a regression, not what a
// GPU draws.
test("500 panes in the city", async ({ browser }) => {
  const page = await browser.newPage({ viewport: { width: 1400, height: 860 } });
  const m = await measure(page, "city");
  console.log(`city, laptop, 500 panes: ${m.fps.toFixed(1)} fps, work p50 ${m.workP50.toFixed(2)} ms over ${m.frames} frames`);
  expect(m.fps).toBeGreaterThanOrEqual(20);
  await page.close();
});

// M42: the hive and the timeline at 500 panes, both Canvas 2D like blocks.
for (const theme of ["hive", "timeline"] as const) {
  test(`500 panes in the ${theme}`, async ({ browser }) => {
    const page = await browser.newPage({ viewport: { width: 1400, height: 860 } });
    const m = await measure(page, theme);
    console.log(`${theme}, laptop, 500 panes: ${m.fps.toFixed(1)} fps, work p50 ${m.workP50.toFixed(2)} ms over ${m.frames} frames`);
    expect(m.fps).toBeGreaterThanOrEqual(50);
    await page.close();
  });
}
