// #333: two windows with edit rights on one pane used to resize it back
// and forth (every resize is a SIGWINCH and a redraw) at every turn of
// typing, and whenever a window was focused. The size now stays with the
// window that has it until it stops typing for a few seconds.

import { expect, test, type Page } from "@playwright/test";
import { active, closeContexts, open, ready, reset, run, tab, text, type } from "./helpers";

test.afterAll(closeContexts);

const me = (p: Page) => p.evaluate(() => window.__arugula.client.clientId);

test("two windows typing in turn don't resize the pane at every turn", async ({ browser }) => {
  const a = await (await browser.newContext({ viewport: { width: 1100, height: 650 } })).newPage();
  await reset(a);
  const pane = await active(a);
  const b = await (await browser.newContext({ viewport: { width: 760, height: 480 } })).newPage();
  await open(b);
  await ready(b, pane);
  // B showed the tab last, so the size is B's; A asks for it back.
  await expect.poll(async () => (await tab(a)).owner).toBe(await me(b));
  await a.locator(".sized-elsewhere").click();
  await expect.poll(async () => (await tab(a)).owner).toBe(await me(a));
  const sized = await tab(a);

  // Every size the tab has from now on (each one resizes the PTY).
  await a.evaluate((id) => {
    const c = window.__arugula.client;
    const w = window as unknown as { sizes: string[] };
    w.sizes = [];
    let last = "";
    c.subscribe(() => {
      const t = c.state?.tabs.find((t) => t.id === id);
      const now = t ? `${t.cols}x${t.rows}` : "";
      if (now && now !== last) w.sizes.push(now);
      last = now;
    });
  }, sized.id);
  const sizes = () => a.evaluate(() => (window as unknown as { sizes: string[] }).sizes);
  await run(a, pane, "echo start-$((1+1))", "start-2");

  // They take turns, about a second apart.
  for (let i = 0; i < 3; i++) {
    await type(b, pane, `echo b${i}\n`);
    await b.waitForTimeout(800);
    await type(a, pane, `echo a${i}\n`);
    await a.waitForTimeout(800);
  }
  // Focusing B's window doesn't take the size either.
  await b.bringToFront();
  await b.evaluate(() => window.dispatchEvent(new Event("focus")));
  await run(a, pane, "echo done-$((2+2))", "done-4");
  await expect.poll(() => text(b, pane)).toContain("done-4");
  expect(await sizes(), "the pane was resized while they took turns").toEqual([`${sized.cols}x${sized.rows}`]);
  expect((await tab(a)).owner).toBe(await me(a));

  // Once A has left the keyboard for a while, B's typing takes the size.
  await b.waitForTimeout(3500);
  await type(b, pane, "echo b-takes\n");
  await expect.poll(async () => (await tab(a)).owner).toBe(await me(b));
  expect(await sizes()).toHaveLength(2);
});
