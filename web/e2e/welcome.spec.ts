// #110: Getting started opens once per browser, then from the session menu
// and the phone's sheet. #96: what a phone that can't be notified is told.

import { devices, expect, test } from "@playwright/test";
import { reset } from "./helpers";

// The app doesn't greet automation (every other spec wants a clean first
// screen); these pretend to be a person.
const person = () => Object.defineProperty(Navigator.prototype, "webdriver", { get: () => false });

test("opens once on first run, a step at a time, then from the session menu", async ({ browser }) => {
  const ctx = await browser.newContext();
  await ctx.addInitScript(person);
  const page = await ctx.newPage();
  await reset(page);
  const panel = page.getByRole("dialog", { name: "Getting started" });
  await expect(panel).toBeVisible();
  const progress = panel.locator("[data-start-progress]");
  await expect(progress).toHaveText("Step 1 / 5 · Welcome");
  await expect(panel.getByRole("heading", { name: "Right-click anything" })).toBeVisible();

  // Next walks the steps; the rail jumps to one.
  await panel.locator("[data-start-next]").click();
  await expect(progress).toHaveText("Step 2 / 5 · Phone");
  // What the daemon says about Tailscale: the test machine may or may not
  // have it, but the button is there until it serves this daemon.
  await expect(panel.locator("[data-start-serve]")).toBeVisible();
  await panel.locator("[data-start-next]").click();
  await expect(progress).toHaveText("Step 3 / 5 · Cloud");
  await expect(panel.locator("[data-start-connect]")).toHaveText("Connect to illogical cloud");
  await panel.locator("[data-start-next]").click();
  await expect(progress).toHaveText("Step 4 / 5 · Agents");
  await expect(panel.locator("[data-start-agent]")).toBeVisible();
  await panel.locator('[data-start-seg="ready"]').click();
  await expect(progress).toHaveText("Step 5 / 5 · Ready");
  // The test daemon isn't joined or served: those are still to do.
  await expect(panel.locator('[data-check="todo"]').first()).toBeVisible();
  await panel.getByRole("button", { name: "Done" }).click();
  await expect(panel).toBeHidden();
  // Closing says where it went.
  await expect(page.locator("[data-start-toast]")).toHaveText("Getting started is in the session menu, any time.");

  // Remembered: not again on reload.
  await page.reload();
  await expect.poll(() => page.evaluate(() => window.__illogical?.client.state !== null)).toBe(true);
  await expect(page.locator(".session-button")).toBeVisible();
  await expect(panel).toBeHidden();

  // Always in the session menu, from the start.
  await page.locator(".session-button").click();
  await page.getByRole("menuitem", { name: "Getting started" }).click();
  await expect(panel).toBeVisible();
  await expect(progress).toHaveText("Step 1 / 5 · Welcome");
  // Its agents step opens the agent dialog.
  await panel.locator('[data-start-seg="agents"]').click();
  await panel.locator("[data-start-agent]").click();
  await expect(panel).toBeHidden();
  await ctx.close();
});

test("the daemon's setup status, and the cheap poll for a join", async ({ page }) => {
  await reset(page);
  const status = await page.evaluate(async () => (await fetch("/api/setup")).json());
  expect(["missing", "stopped", "needs-login", "running"]).toContain(status.tailscale.state);
  expect(status.control.url).toBe("https://control.illogical.widgets.wtf");
  expect(typeof status.claude.installed).toBe("boolean");
  // Polling while a join waits asks for the cheap part only.
  const control = await page.evaluate(async () => (await fetch("/api/setup?part=control")).json());
  expect(Object.keys(control)).toEqual(["control"]);
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
    await expect(page.locator("[data-notify-blocked]")).toHaveText("Notifications are blocked for this site in your browser's settings.");
  });
});
