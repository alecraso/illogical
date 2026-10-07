// #139: the command palette's checks, run in Chrome (palette.spec.ts) and
// WebKit (palette.webkit.spec.ts), on a desktop and a phone.

import { expect, type Page } from "@playwright/test";
import { active, paneEl, panes, ready, reset, tabsInSession, text, type as typeIn } from "./helpers";

const palette = (page: Page) => page.getByRole("dialog", { name: "Command palette" });
const filter = (page: Page) => palette(page).getByRole("textbox", { name: "Command" });
const rows = (page: Page) => palette(page).getByRole("option");

/** Type into the palette and run the best match with Enter. */
async function pick(page: Page, query: string, key: string) {
  await filter(page).fill(query);
  await expect(rows(page).first()).toHaveAttribute("data-command", key);
  await filter(page).press("Enter");
  await expect(palette(page)).toBeHidden();
}

/** The pane menu's actions as the palette names them: a header names the
 * items after it, up to the next separator. */
async function paneMenu(page: Page, pane: number): Promise<string[]> {
  await paneEl(page, pane).click({ button: "right", position: { x: 60, y: 60 } });
  const menu = page.getByRole("menu");
  await expect(menu).toBeVisible();
  const labels = await menu.evaluate((m) => {
    const out: string[] = [];
    let under: string | null = null;
    for (const el of Array.from(m.children)) {
      if (el.classList.contains("menu-sep")) under = null;
      else if (el.classList.contains("menu-header")) under = el.textContent!.trim();
      else {
        const label = Array.from(el.childNodes)
          .filter((n) => !(n instanceof HTMLElement && (n.classList.contains("menu-check") || n.classList.contains("menu-key"))))
          .map((n) => n.textContent)
          .join("")
          .trim();
        out.push(under ? `${under}: ${label}` : label);
      }
    }
    return out;
  });
  await page.keyboard.press("Escape");
  await expect(menu).toBeHidden();
  return labels;
}

/** `mod` is the chord's modifier: Control everywhere, Meta on a Mac too. */
export async function desktop(page: Page, mod: "Control" | "Meta" = "Control") {
  await page.addInitScript(() => localStorage.removeItem("arugula.palette.recent"));
  await reset(page);
  const [first] = await panes(page);
  await ready(page, first);

  // The chord from a focused terminal: the palette opens and the shell
  // gets nothing (a stray Ctrl+P would recall history).
  await typeIn(page, first, "echo palette-$((2+3))\n");
  // Its output and the next prompt, so nothing more is on the way.
  await expect.poll(() => text(page, first)).toMatch(/palette-5\n[^\n]*\$ /);
  const before = await text(page, first);
  await page.keyboard.press(`${mod}+Shift+KeyP`);
  await expect(palette(page)).toBeVisible();
  await expect(filter(page)).toBeFocused();

  // Every action in the pane menu is in the palette, for the same pane.
  await page.keyboard.press("Escape");
  await expect(palette(page)).toBeHidden();
  const menu = await paneMenu(page, first);
  expect(menu).toContain("Split right");
  expect(menu).toContain("After a restart: Nothing (wait for Enter)");
  await page.keyboard.press(`${mod}+Shift+KeyP`);
  await expect(palette(page)).toBeVisible();
  const listed = await palette(page)
    .locator('[data-command^="Pane/"]')
    .evaluateAll((els) => els.map((e) => (e as HTMLElement).dataset.command!.slice("Pane/".length)));
  expect(listed).toEqual(menu);
  // Shortcuts show beside the actions that have one.
  await expect(palette(page).locator('[data-command="Pane/Go to directory…"] .menu-key')).toHaveText("Ctrl+Shift+G");

  // Type and run: Split right on the active pane.
  await pick(page, "split right", "Pane/Split right");
  await expect.poll(async () => (await panes(page)).length).toBe(2);
  expect(await text(page, first)).toBe(before);

  // A restart policy (a checked item) on the new pane, which is active.
  const second = await active(page);
  expect(second).not.toBe(first);
  await page.keyboard.press(`${mod}+Shift+KeyP`);
  await pick(page, "wait for enter", "Pane/After a restart: Nothing (wait for Enter)");
  await expect
    .poll(() => page.evaluate((p) => window.__arugula.client.info(p)?.policy.kind, second))
    .toBe("none");

  // Move it to a tab of its own, then rename that tab through a prompt.
  await page.keyboard.press(`${mod}+Shift+KeyP`);
  await pick(page, "move to new tab", "Pane/Move to new tab");
  await expect.poll(async () => (await tabsInSession(page)).length).toBe(2);
  await page.keyboard.press(`${mod}+Shift+KeyP`);
  await pick(page, "rename tab", "Tab/Rename tab");
  const prompt = page.getByRole("dialog", { name: "Rename tab" });
  await prompt.getByRole("textbox").fill("palette-tab");
  await prompt.getByRole("textbox").press("Enter");
  await expect(page.locator(".tab.selected .tab-label")).toHaveText("palette-tab");

  // Recent picks come first, newest first, with nothing typed.
  await page.keyboard.press(`${mod}+Shift+KeyP`);
  await expect(rows(page).nth(0)).toHaveAttribute("data-command", "Tab/Rename tab");
  await expect(rows(page).nth(1)).toHaveAttribute("data-command", "Pane/Move to new tab");
  await expect(rows(page).nth(0).locator(".palette-recent")).toBeVisible();

  // Jump to the first tab by name; arrows move the selection.
  const firstTab = (await tabsInSession(page))[0];
  await filter(page).fill("tab");
  const jump = palette(page).locator(`[data-command="Go/tab ${firstTab}"]`);
  await expect(jump).toBeVisible();
  const at = await rows(page).evaluateAll((els, key) => els.findIndex((e) => (e as HTMLElement).dataset.command === key), `Go/tab ${firstTab}`);
  for (let i = 0; i < at; i++) await filter(page).press("ArrowDown");
  await expect(jump).toHaveAttribute("aria-selected", "true");
  await filter(page).press("Enter");
  await expect.poll(() => page.evaluate(() => window.__arugula.client.tab)).toBe(firstTab);
  await expect.poll(() => active(page)).toBe(first);

  // The chord again closes it, and so does a click outside.
  await page.keyboard.press(`${mod}+Shift+KeyP`);
  await expect(palette(page)).toBeVisible();
  await page.keyboard.press(`${mod}+Shift+KeyP`);
  await expect(palette(page)).toBeHidden();

  // And it's listed, with its chord, in the session menu.
  await page.locator(".session-button").click();
  await page.getByRole("menuitem", { name: "Command palette…" }).click();
  await expect(palette(page)).toBeVisible();
  await page.mouse.click(5, 300);
  await expect(palette(page)).toBeHidden();
}

/** On a phone: from the sheet's Commands button, a full-height sheet. */
export async function phone(page: Page) {
  await reset(page);
  await page.locator(".sheet-button").click();
  await page.getByRole("button", { name: "Commands" }).click();
  await expect(palette(page)).toBeVisible();
  const box = (await palette(page).locator(".palette").boundingBox())!;
  const view = page.viewportSize()!;
  expect(box.width).toBeGreaterThanOrEqual(view.width - 1);
  expect(box.height).toBeGreaterThan(view.height * 0.8);
  // The keyboard stays down until asked for.
  await expect(filter(page)).not.toBeFocused();
  await filter(page).tap();
  await filter(page).fill("split down");
  await expect(rows(page).first()).toHaveAttribute("data-command", "Pane/Split down");
  await rows(page).first().tap();
  await expect(palette(page)).toBeHidden();
  await expect.poll(() => page.evaluate(() => window.__arugula.client.state!.panes.length)).toBe(2);
}
