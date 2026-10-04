// #176: a newer release shows as a chip in the top bar, with the command
// that updates this install. The daemon's check is off for a build run
// from target/, so these answer `/api/update` themselves.

import { expect, test, type Page } from "@playwright/test";
import { reset } from "./helpers";

const status = (over: Record<string, unknown> = {}) => ({
  current: "0.16.0",
  latest: "0.17.0",
  newer: true,
  enabled: true,
  kind: "script",
  command: "curl -fsSL https://illogical.widgets.wtf/install.sh | sh",
  url: "https://github.com/arugula-salad/illogical/releases/tag/v0.17.0",
  ...over,
});

async function serve(page: Page, body: object) {
  await page.route("**/api/update", (r) => r.fulfill({ json: body }));
}

test("a newer release: the chip, the command, and Not now", async ({ page }) => {
  await serve(page, status());
  await reset(page);
  const chip = page.locator("[data-update-chip]");
  await expect(chip).toHaveText("Update 0.17.0");
  await chip.click();
  const pop = page.getByRole("dialog", { name: "Update illogical" });
  await expect(pop).toContainText("this daemon is 0.16.0");
  await expect(pop.locator("[data-update-command]")).toHaveText("curl -fsSL https://illogical.widgets.wtf/install.sh | sh");
  await expect(pop.getByRole("link", { name: "What's new" })).toHaveAttribute("href", /\/tag\/v0\.17\.0$/);

  // Dismissed for this version: gone, and still gone after a reload.
  await pop.locator("[data-update-dismiss]").click();
  await expect(chip).toBeHidden();
  await page.reload();
  await expect(page.locator(".session-button")).toBeVisible();
  await expect(chip).toBeHidden();
  await page.evaluate(() => localStorage.removeItem("illogical.update.dismissed"));
});

test("Homebrew's command, the app's download, and nothing when up to date", async ({ page }) => {
  await serve(page, status({ kind: "brew", command: "brew upgrade illogical && illogicald install" }));
  await reset(page);
  await page.locator("[data-update-chip]").click();
  await expect(page.locator("[data-update-command]")).toHaveText("brew upgrade illogical && illogicald install");

  await page.unroute("**/api/update");
  await serve(page, status({ kind: "app", command: undefined }));
  await page.reload();
  await page.locator("[data-update-chip]").click();
  await expect(page.getByRole("dialog", { name: "Update illogical" })).toContainText("Download the new app");

  await page.unroute("**/api/update");
  await serve(page, status({ latest: "0.16.0", newer: false }));
  await page.reload();
  await expect(page.locator(".session-button")).toBeVisible();
  await expect(page.locator("[data-update-chip]")).toBeHidden();
});

test("the real daemon answers: off for a build run from target/", async ({ page }) => {
  await reset(page);
  const got = await page.evaluate(() => fetch("/api/update").then((r) => r.json()));
  expect(got).toMatchObject({ enabled: false, newer: false, kind: "source" });
  await expect(page.locator("[data-update-chip]")).toBeHidden();
});
