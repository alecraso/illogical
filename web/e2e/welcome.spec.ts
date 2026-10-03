// #110: Getting started opens once per browser, then from the session menu
// and the phone's sheet. #96: what a phone that can't be notified is told.

import { devices, expect, test } from "@playwright/test";
import { reset } from "./helpers";

// The app doesn't greet automation (every other spec wants a clean first
// screen); these pretend to be a person.
const person = () => Object.defineProperty(Navigator.prototype, "webdriver", { get: () => false });

test("opens once on first run, then from the session menu", async ({ browser }) => {
  const ctx = await browser.newContext();
  await ctx.addInitScript(person);
  const page = await ctx.newPage();
  await reset(page);
  const panel = page.getByRole("dialog", { name: "Getting started" });
  await expect(panel).toBeVisible();
  for (const h of ["Right-click anything", "On your phone", "From anywhere, or with your team", "Agents", "Claude Code", "In a terminal"]) {
    await expect(panel.getByRole("heading", { name: h })).toBeVisible();
  }
  await expect(panel.locator("[data-serve-command]")).toHaveText(/^tailscale serve --bg --https=443 http:\/\/127\.0\.0\.1:\d+$/);
  await expect(panel.locator("[data-join-command]")).toHaveText("illogicald join https://control.illogical.widgets.wtf");
  await expect(panel.locator("[data-mcp-command]")).toHaveText("claude mcp add illogical -- illogical mcp");
  // The test daemon isn't on a tailnet or joined: no done marks.
  await expect(panel.locator(".start-done")).toHaveCount(0);
  await panel.getByRole("button", { name: "Close" }).click();
  await expect(panel).toBeHidden();

  // Remembered: not again on reload.
  await page.reload();
  await expect.poll(() => page.evaluate(() => window.__illogical?.client.state !== null)).toBe(true);
  await expect(page.locator(".session-button")).toBeVisible();
  await expect(panel).toBeHidden();

  // Always in the session menu.
  await page.locator(".session-button").click();
  await page.getByRole("menuitem", { name: "Getting started" }).click();
  await expect(panel).toBeVisible();
  // Its agents link opens the agent dialog.
  await panel.locator("[data-start-agent]").click();
  await expect(panel).toBeHidden();
  await ctx.close();
});

test.describe("phone", () => {
  const { viewport, deviceScaleFactor, isMobile, hasTouch } = devices["Pixel 7"];
  test.use({ viewport, deviceScaleFactor, isMobile, hasTouch });

  test("in the sheet", async ({ page }) => {
    await reset(page);
    await expect(page.getByRole("dialog", { name: "Getting started" })).toBeHidden();
    await page.locator(".sheet-button").click();
    await page.getByRole("button", { name: "Getting started" }).click();
    await expect(page.getByRole("dialog", { name: "Getting started" })).toBeVisible();
  });

  test("iOS in a Safari tab: add it to the Home Screen, once", async ({ browser }) => {
    const ctx = await browser.newContext({ ...devices["iPhone 13"], hasTouch: true });
    // Safari in a tab has no push at all.
    await ctx.addInitScript(() => {
      delete (window as { PushManager?: unknown }).PushManager;
    });
    const page = await ctx.newPage();
    await reset(page);
    const hint = page.locator("[data-install-hint]");
    await expect(hint).toContainText("Add it to your Home Screen");
    await page.locator(".sheet-button").click();
    await expect(page.locator("[data-notify-blocked]")).toHaveText(/^Add it to your Home Screen first \(Share › Add to Home Screen\), then open it from there\.$/);
    await expect(page.getByRole("button", { name: "Notify this device" })).toHaveCount(0);
    await page.locator(".sheet-backdrop").click({ position: { x: 5, y: 600 } });
    await hint.getByRole("button", { name: "Dismiss" }).click();
    await expect(hint).toBeHidden();
    await page.reload();
    await expect(page.locator(".sheet-button")).toBeVisible();
    await expect(hint).toBeHidden();
    await ctx.close();
  });

  test("blocked: says where to unblock it", async ({ page }) => {
    await page.addInitScript(() => Object.defineProperty(Notification, "permission", { get: () => "denied" }));
    await reset(page);
    await page.locator(".sheet-button").click();
    await expect(page.locator("[data-notify-blocked]")).toHaveText("Blocked for this site in your browser's settings.");
  });
});
