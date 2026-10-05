// M1: tabs and splits held by the daemon, driven by the mouse.

import { expect, test } from "@playwright/test";
import {
  active,
  at,
  dragTo,
  menu,
  open,
  paneEl,
  panes,
  ready,
  reset,
  run,
  screen,
  size,
  tab,
  tabsInSession,
  text,
  type,
  closeContexts,
} from "./helpers";

test.afterAll(closeContexts);

test("split from the context menu, use both panes, close one", async ({ page }) => {
  await reset(page);
  const first = await active(page);
  await menu(page, paneEl(page, first), "Split right");
  await expect.poll(() => panes(page)).toHaveLength(2);
  const second = (await panes(page))[1];
  // The new pane is the active one and has the keyboard.
  await expect.poll(() => active(page)).toBe(second);
  await ready(page, second);
  await page.keyboard.type("echo right-$((2+3))\n");
  await expect.poll(() => text(page, second)).toContain("right-5");

  await menu(page, paneEl(page, second), "Split down");
  await expect.poll(() => panes(page)).toHaveLength(3);
  const t = await tab(page);
  const rects = Object.fromEntries(t.layout.panes);
  expect(rects[first].x).toBe(0);
  expect(rects[second].x).toBeGreaterThan(0);
  expect(rects[second].y).toBe(0);

  await run(page, first, "echo left-$((1+1))", "left-2");
  await menu(page, paneEl(page, second), "Close pane");
  await expect.poll(() => panes(page)).toHaveLength(2);
  await expect.poll(() => panes(page)).not.toContain(second);
});

test("exiting a shell closes its pane", async ({ page }) => {
  await reset(page);
  await menu(page, paneEl(page, await active(page)), "Split down");
  await expect.poll(() => panes(page)).toHaveLength(2);
  const second = await active(page);
  await ready(page, second);
  await type(page, second, "exit\n");
  await expect.poll(() => panes(page)).toHaveLength(1);
});

test("dragging a divider resizes the panes' terminals", async ({ page }) => {
  await reset(page);
  const first = await active(page);
  await menu(page, paneEl(page, first), "Split right");
  await expect.poll(() => panes(page)).toHaveLength(2);
  const second = await active(page);
  const before = (await size(page, first))!;
  const divider = page.locator(".divider").first();
  const b = (await divider.boundingBox())!;
  await dragTo(page, divider, { x: b.x - 200, y: b.y + b.height / 2 });
  await expect.poll(async () => (await size(page, first))![0]).toBeLessThan(before[0] - 15);
  // Let the last throttled resize land, then the program agrees with what we draw.
  await expect.poll(async () => (await tab(page)).layout.panes[0][1].cols).toBe((await size(page, first))![0]);
  await page.waitForTimeout(200);
  const after = (await size(page, first))!;
  await run(page, first, "tput cols", `\n${after[0]}\n`);
  expect((await size(page, second))![0]).toBeGreaterThan(before[0]);
});

test("dragging a pane onto another pane's edge moves it; vim inside is untouched", async ({ page }) => {
  await reset(page);
  const left = await active(page);
  await menu(page, paneEl(page, left), "Split right");
  await expect.poll(() => panes(page)).toHaveLength(2);
  const right = await active(page);
  await ready(page, right);
  await type(page, right, "nvim -u NONE -i NONE /etc/services\n");
  await expect.poll(() => screen(page, right)).toContain("/etc/services");
  await page.keyboard.type(":set number\n10G", { delay: 2 });
  await expect.poll(() => screen(page, right)).toMatch(/\b10 /);

  // Grab the right pane's grip and drop it on the top edge of the left one.
  await paneEl(page, right).hover();
  await dragTo(page, paneEl(page, right).locator(".grip"), await at(paneEl(page, left), 0.5, 0.08));
  await expect.poll(async () => (await tab(page)).root.type === "split" && (await tab(page)).root).toMatchObject({
    dir: "column",
  });
  expect(await panes(page)).toEqual([right, left]);
  // Same nvim, redrawn at its new size, still on line 10.
  await expect.poll(() => screen(page, right)).toMatch(/\b10 /);
  await expect.poll(() => screen(page, right)).toContain("/etc/services");
  await type(page, right, ":qa!\n");
});

test("tabs: create, rename, reorder by dragging, dock into a split, close", async ({ page }) => {
  await reset(page);
  const [t1] = await tabsInSession(page);
  await page.getByTitle("New tab").click();
  await expect.poll(() => tabsInSession(page)).toHaveLength(2);
  const t2 = (await tabsInSession(page))[1];
  // The new tab is shown.
  await expect.poll(() => tab(page).then((t) => t.id)).toBe(t2);

  // Double-click to rename.
  await page.locator(`[data-tab-id="${t2}"]`).dblclick();
  await page.locator("input.rename").fill("logs");
  await page.keyboard.press("Enter");
  await expect(page.locator(`[data-tab-id="${t2}"]`)).toContainText("logs");

  // Drag it before the first tab.
  await dragTo(page, page.locator(`[data-tab-id="${t2}"]`), await at(page.locator(`[data-tab-id="${t1}"]`), 0.1, 0.5));
  await expect.poll(() => tabsInSession(page)).toEqual([t2, t1]);

  // Show tab 1, then drag "logs" onto the right edge of its pane: one tab, two panes.
  await page.locator(`[data-tab-id="${t1}"]`).click();
  await expect.poll(() => tab(page).then((t) => t.id)).toBe(t1);
  const target = (await panes(page))[0];
  await dragTo(page, page.locator(`[data-tab-id="${t2}"]`), await at(paneEl(page, target), 0.92, 0.5));
  await expect.poll(() => tabsInSession(page)).toEqual([t1]);
  await expect.poll(() => panes(page)).toHaveLength(2);

  // Middle-click closes a tab.
  await page.getByTitle("New tab").click();
  await expect.poll(() => tabsInSession(page)).toHaveLength(2);
  const t3 = (await tabsInSession(page))[1];
  await page.locator(`[data-tab-id="${t3}"]`).click({ button: "middle" });
  await expect.poll(() => tabsInSession(page)).toEqual([t1]);
});

test("pane to a new tab and back", async ({ page }) => {
  await reset(page);
  await menu(page, paneEl(page, await active(page)), "Split right");
  await expect.poll(() => panes(page)).toHaveLength(2);
  const moved = await active(page);
  await menu(page, paneEl(page, moved), "Move to new tab");
  await expect.poll(() => tabsInSession(page)).toHaveLength(2);
  await expect.poll(() => panes(page)).toHaveLength(1);
});

test("sessions: create, switch, close", async ({ page }) => {
  await reset(page);
  await page.locator(".session-button").click();
  await page.getByRole("menuitem", { name: "New session" }).click();
  await expect.poll(() => page.evaluate(() => window.__illogical.client.state!.sessions.length)).toBe(2);
  const [s1, s2] = await page.evaluate(() => window.__illogical.client.state!.sessions.map((s) => s.id));
  // Sessions are named ("drifting cedar"), not numbered.
  const name1 = await page.evaluate(() => window.__illogical.client.state!.sessions[0].name);
  expect(name1).toMatch(/^[a-z]+ [a-z]+$/);
  await expect.poll(() => page.evaluate(() => window.__illogical.client.session)).toBe(s2);
  await page.locator(".session-button").click();
  await page.getByRole("menuitem", { name: new RegExp(`${name1}$`) }).click();
  await expect.poll(() => page.evaluate(() => window.__illogical.client.session)).toBe(s1);
  await page.locator(".session-button").click();
  await page.getByRole("menuitem", { name: "Close session" }).click();
  await expect.poll(() => page.evaluate(() => window.__illogical.client.state!.sessions.map((s) => s.id))).toEqual([s2]);
});

test("a second window sees layout changes live, and the same terminal", async ({ browser }) => {
  const a = await (await browser.newContext({ viewport: { width: 1100, height: 650 } })).newPage();
  await reset(a);
  const b = await (await browser.newContext({ viewport: { width: 1100, height: 650 } })).newPage();
  await open(b);
  const first = await active(a);
  await menu(a, paneEl(a, first), "Split right");
  await expect.poll(() => panes(b)).toHaveLength(2);
  // A learns of its own split no sooner than B does.
  await expect.poll(() => panes(a)).toHaveLength(2);
  const second = (await panes(a))[1];
  await ready(b, second);
  await run(a, second, "echo shared-$((7*6))", "shared-42");
  await expect.poll(() => text(b, second)).toContain("shared-42");
  // B drags the divider; A follows.
  const divider = b.locator(".divider").first();
  const box = (await divider.boundingBox())!;
  const before = (await tab(a)).layout.panes[0][1].cols;
  await dragTo(b, divider, { x: box.x + 150, y: box.y + box.height / 2 });
  await expect.poll(async () => (await tab(a)).layout.panes[0][1].cols).toBeGreaterThan(before + 10);
});
