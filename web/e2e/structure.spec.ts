// M3 in the browser: command marks from the shell integration, attention
// badges, and the switches that go with them.

import { devices, expect, test } from "@playwright/test";
import { active, menu, panes, paneEl, ready, reset, tab, tabsInSession, text, type } from "./helpers";

test("finished commands get marks; a mark selects its output and runs again", async ({ page }) => {
  await reset(page);
  const pane = await active(page);
  await type(page, pane, "echo good-$((1+1))\n");
  await expect.poll(() => text(page, pane)).toContain("good-2");
  await type(page, pane, "echo bad-$((2+2)); false\n");
  await expect(paneEl(page, pane).locator(".cmd-mark.fail")).toHaveCount(1);
  await expect(paneEl(page, pane).locator(".cmd-mark.ok")).toHaveCount(1);
  await expect(paneEl(page, pane).locator(".cmd-mark.fail")).toHaveAttribute("title", /false · exit 1/);

  // Click: the command's output is selected.
  await paneEl(page, pane).locator(".cmd-mark.fail").click();
  await expect.poll(() => page.evaluate((p) => window.__arugula.selection(p).trim(), pane)).toBe("bad-4");

  // Right-click: run it again.
  await paneEl(page, pane).locator(".cmd-mark.ok").click({ button: "right" });
  await page.getByRole("menuitem", { name: "Run again" }).click();
  await expect.poll(async () => (await text(page, pane)).match(/^good-2$/gm)?.length).toBe(2);
  await expect(paneEl(page, pane).locator(".cmd-mark.ok")).toHaveCount(2);
});

test("a background tab that wants you gets a badge until you look", async ({ page }) => {
  await reset(page);
  const [t1] = await tabsInSession(page);
  const pane = await active(page);
  // Notify (OSC 9) a moment after we've moved to another tab.
  await type(page, pane, "sleep 1; printf '\\e]9;ready for you\\a'\n");
  await page.getByTitle("New tab").click();
  await expect.poll(() => tabsInSession(page)).toHaveLength(2);
  const badge = page.locator(`[data-tab-id="${t1}"] .att.needs_input`);
  await expect(badge).toBeVisible({ timeout: 10_000 });

  // Back to it and type: answered.
  await page.locator(`[data-tab-id="${t1}"]`).click();
  await expect.poll(() => tab(page).then((t) => t.id)).toBe(t1);
  await type(page, pane, "true\n");
  await expect(badge).toBeHidden();
});

test("shell integration can be switched off for a pane's new shells", async ({ page }) => {
  await reset(page);
  const pane = await active(page);
  await paneEl(page, pane).click({ button: "right", position: { x: 60, y: 60 } });
  const item = page.getByRole("menuitemradio", { name: /Shell integration/ });
  await expect(item).toHaveAttribute("aria-checked", "true");
  await item.click();
  await expect
    .poll(() => page.evaluate((p) => window.__arugula.client.info(p)?.integration, pane))
    .toBe(false);
  // A split from it inherits the choice: no marks there.
  await menu(page, paneEl(page, pane), "Split right");
  await expect.poll(() => panes(page)).toHaveLength(2);
  const split = await active(page);
  await ready(page, split);
  await type(page, split, "false\n");
  await page.waitForTimeout(500);
  await expect(paneEl(page, split).locator(".cmd-mark")).toHaveCount(0);
});

test("the session menu offers notifications for this device", async ({ page }) => {
  await reset(page);
  await page.locator(".session-button").click();
  await expect(page.getByRole("menuitemradio", { name: "Notify this device" })).toBeVisible();
  // The service worker that shows them is registered.
  await expect.poll(() => page.evaluate(async () => !!(await navigator.serviceWorker.getRegistration()))).toBe(true);
});

test.describe("phone", () => {
  const { viewport, userAgent, deviceScaleFactor, isMobile, hasTouch } = devices["Pixel 7"];
  test.use({ viewport, userAgent, deviceScaleFactor, isMobile, hasTouch });

  test("the sheet lists panes that need you", async ({ page }) => {
    await reset(page);
    const pane = await active(page);
    await page.evaluate(
      (p) => fetch(`/api/panes/${p}/attention`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ state: "needs_input" }) }),
      pane,
    );
    await expect(page.locator(".sheet-button .att.needs_input")).toBeVisible();
    await page.locator(".sheet-button").click();
    await expect(page.locator(".needs-you .sheet-item")).toHaveCount(1);
  });
});
