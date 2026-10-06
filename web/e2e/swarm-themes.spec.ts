// A machine with labs (the fake fleet's daemons have the file) offers all
// four themes, and a theme saved in this browser is kept on a cold load,
// before the machine's features have arrived. Without labs the swarm offers
// one theme, the field of blocks: `labs-off.spec.ts`. Against the fake fleet
// (e2e/fake-fleet.ts).

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

test("with labs the swarm has the theme picker", async ({ page }) => {
  await swarm(page);
  await expect(page.locator("[data-theme-pick]")).toHaveCount(4);
  await expect(page.locator(".swarm")).toHaveAttribute("data-theme", "blocks");
  // The rest of the bar is still there.
  await expect(page.locator('[data-g="machine"]')).toBeVisible();
});

test("a theme saved in this browser is there on a cold load", async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem("illogical.swarm.theme", "city"));
  await swarm(page);
  await expect(page.locator(".swarm")).toHaveAttribute("data-theme", "city");
  await expect(page.locator("canvas.swarm-city")).toBeVisible();
  await expect(page.locator("[data-theme-pick][aria-pressed=true]")).toHaveAttribute("data-theme-pick", "city");
});
