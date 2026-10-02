// `just screenshots`, the swarm (M26): a fake fleet (e2e/fake-fleet.ts) of
// three machines with scripted work and two agents asking, a failing batch
// of tests on one machine, and the synthetic fleet (src/swarm/fake.ts) for
// a field of a few hundred panes. Nothing real is shown.

import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { devices, expect, test, type Page } from "@playwright/test";
import { FakeFleet } from "../e2e/fake-fleet";

const out = join(dirname(fileURLToPath(import.meta.url)), "../../site/img");
const base = "http://127.0.0.1:7690";
let fake: FakeFleet;

test.use({ baseURL: base });
test.describe.configure({ mode: "serial" });

test.beforeAll(async () => {
  fake = new FakeFleet();
  await fake.machine("workstation", 7690);
  await fake.machine("build-01", 7691);
  await fake.machine("build-02", 7692);
  await fake.populate();
  await fake.agentAsks("workstation", "illogical", "cargo test -p illogical-vt");
  await fake.agentAsks("build-01", "illogical", "git push origin swarm");
  await fake.trouble("build-02", 4);
});

test.afterAll(() => fake?.stop());

async function swarm(page: Page) {
  await page.goto("/#swarm");
  await expect.poll(() => page.evaluate(() => window.__illogical?.fleet.list.filter((h) => h.state === "connected").length ?? 0), { timeout: 20_000 }).toBe(3);
  await page.evaluate(() => void window.__illogical.swarmFake(360));
  await expect(page.locator(".swarm-card[data-bundle]").first()).toBeVisible({ timeout: 20_000 });
  await page.waitForTimeout(6000);
}

test("the swarm, with what needs you on the rail", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await swarm(page);
  await page.screenshot({ path: join(out, "swarm.png") });
});

test.describe("phone", () => {
  const { viewport, userAgent, deviceScaleFactor, isMobile, hasTouch } = devices["Pixel 7"];
  test.use({ viewport, userAgent, deviceScaleFactor, isMobile, hasTouch });
  test("the swarm on a phone: cards along the bottom", async ({ page }) => {
    await swarm(page);
    await page.screenshot({ path: join(out, "swarm-phone.png") });
  });
});
