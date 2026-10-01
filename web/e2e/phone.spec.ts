// M1 on a phone: one pane at a time, a sheet to switch, and a key bar.

import { devices, expect, test } from "@playwright/test";
import { active, ready, reset, text, type } from "./helpers";

const { viewport, userAgent, deviceScaleFactor, isMobile, hasTouch } = devices["Pixel 7"];
test.use({ viewport, userAgent, deviceScaleFactor, isMobile, hasTouch });

test("one pane at a time, switch from the sheet, extra keys work", async ({ page }) => {
  await reset(page);
  // A split made on the phone.
  await page.locator(".sheet-button").click();
  await page.getByRole("button", { name: "Split pane" }).click();
  await expect.poll(() => page.evaluate(() => window.__illogical.client.state!.panes.length)).toBe(2);
  // Only the active pane is drawn, filling the screen.
  await expect.poll(() => page.locator(".pane").count()).toBe(1);
  const shown = await active(page);
  await ready(page, shown);
  const fill = await page.locator(".pane").boundingBox();
  expect(fill!.width).toBeGreaterThan(380);

  // Ctrl from the key bar, then "c", interrupts a running command.
  await type(page, shown, "sleep 100\n");
  await page.getByRole("button", { name: "Ctrl" }).click();
  await expect(page.getByRole("button", { name: "Ctrl" })).toHaveAttribute("aria-pressed", "true");
  await page.keyboard.type("c");
  await expect(page.getByRole("button", { name: "Ctrl" })).toHaveAttribute("aria-pressed", "false");
  await page.keyboard.type("echo back-$((8+1))\n");
  await expect.poll(() => text(page, shown)).toContain("back-9");
  // Drawn on screen, not just in the terminal's buffer.
  await expect(page.locator(".pane .xterm-rows")).toContainText("back-9");

  // Up arrow from the key bar recalls the last command.
  await page.getByRole("button", { name: "↑" }).click();
  await page.getByRole("button", { name: "Tab" }).click();
  await page.keyboard.press("Enter");
  await expect.poll(async () => (await text(page, shown)).match(/back-9/g)?.length).toBe(2);

  // The sheet lists both panes; pick the other one.
  await page.locator(".sheet-button").click();
  const other = page.locator(".sheet-item.sheet-pane:not(.current)");
  await expect(other).toHaveCount(1);
  await other.click();
  await expect.poll(() => active(page)).not.toBe(shown);
  await expect.poll(() => page.locator(".pane").count()).toBe(1);
});
