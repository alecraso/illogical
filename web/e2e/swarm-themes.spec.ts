// The swarm offers one theme, the field of blocks, so there's no theme
// picker; a city, hive or timeline saved from before falls back to blocks.
// The other themes stay built, for whoever sets `illogical.more` (the
// tests of those themes do). Against the fake fleet (e2e/fake-fleet.ts).

import { expect, test, type Page } from "@playwright/test";
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

async function swarm(page: Page) {
  await page.goto("/#swarm");
  await expect(page.locator(".swarm")).toBeVisible();
  await expect(page.locator(".swarm-bar")).toBeVisible();
}

test("only blocks: no theme picker", async ({ page }) => {
  await swarm(page);
  await expect(page.locator(".swarm")).toHaveAttribute("data-theme", "blocks");
  await expect(page.locator("[data-theme-pick]")).toHaveCount(0);
  await expect(page.getByText("Theme", { exact: true })).toHaveCount(0);
  // The rest of the bar is still there.
  await expect(page.locator('[data-g="machine"]')).toBeVisible();
});

test("a theme saved from before falls back to blocks", async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem("illogical.swarm.theme", "city"));
  await swarm(page);
  await expect(page.locator(".swarm")).toHaveAttribute("data-theme", "blocks");
  await expect(page.locator("canvas.swarm-blocks")).toBeVisible();
  await expect(page.locator("canvas.swarm-city")).toHaveCount(0);
});

test("whoever turns the others on gets the picker", async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem("illogical.more", "1"));
  await swarm(page);
  await expect(page.locator("[data-theme-pick]")).toHaveCount(4);
});
