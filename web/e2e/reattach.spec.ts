// M0's promises, still true with many panes: the daemon owns the terminals,
// the page is disposable.

import { expect, test } from "@playwright/test";
import { active, open, panes, ready, reset, run, screen, size, text, type } from "./helpers";

test("nvim survives closing and reopening the page", async ({ browser }) => {
  const ctx = await browser.newContext();
  let page = await ctx.newPage();
  await reset(page);
  const pane = await active(page);
  await run(page, pane, "clear; seq 1 40; echo before-nvim-$((1+1))", "before-nvim-2");
  await type(page, pane, "nvim -u NONE -i NONE /etc/services\n");
  await expect.poll(() => screen(page, pane)).toContain("/etc/services");
  // Scrolling one side of a vertical split is where a renderer that lacks
  // what the program was promised (left/right margins) draws garbage.
  await page.keyboard.type(":set number cursorline\n:vsplit\n30Gzz:syntax on\n", { delay: 2 });
  await expect.poll(() => screen(page, pane)).toMatch(/\b30 /);
  await page.waitForTimeout(300);
  const before = await screen(page, pane);
  await ctx.close();

  // A different browser context: no state survives except on the daemon.
  const ctx2 = await browser.newContext();
  page = await ctx2.newPage();
  await open(page);
  await ready(page, pane);
  await expect.poll(() => screen(page, pane)).toBe(before);

  // Leaving nvim brings back the shell output from before it started.
  await type(page, pane, ":qa!\n");
  await expect.poll(() => screen(page, pane)).toContain("before-nvim-2");
  await ctx2.close();
});

test("output produced while no browser is attached is complete", async ({ browser }) => {
  const ctx = await browser.newContext();
  let page = await ctx.newPage();
  await reset(page);
  const pane = await active(page);
  await run(page, pane, "clear; echo ready-$((5+5))", "ready-10");
  await type(page, pane, "for i in $(seq 1 2000); do echo line-$i; sleep 0.0005; done; echo loop-$((1000+1))\n");
  await expect.poll(() => text(page, pane)).toContain("line-50");
  await ctx.close(); // mid-command

  await new Promise((r) => setTimeout(r, 2500));
  const ctx2 = await browser.newContext();
  page = await ctx2.newPage();
  await open(page);
  await ready(page, pane);
  await expect.poll(() => text(page, pane), { timeout: 15_000 }).toContain("loop-1001");
  const seen = new Set((await text(page, pane)).match(/^line-\d+$/gm));
  const missing = Array.from({ length: 2000 }, (_, i) => `line-${i + 1}`).filter((l) => !seen.has(l));
  expect(missing).toEqual([]);
  await ctx2.close();
});

test("two clients see the same output; the last to type sets the size", async ({ browser }) => {
  const a = await (await browser.newContext({ viewport: { width: 1000, height: 640 } })).newPage();
  await reset(a);
  const b = await (await browser.newContext({ viewport: { width: 760, height: 500 } })).newPage();
  await open(b);
  const pane = (await panes(a))[0];
  await ready(b, pane);

  await run(a, pane, "clear; echo from-a-$((3*3))", "from-a-9");
  await expect.poll(() => text(b, pane)).toContain("from-a-9");
  const sizeA = (await size(a, pane))!;

  await run(b, pane, "echo from-b-$((4*4))", "from-b-16");
  await expect.poll(() => text(a, pane)).toContain("from-b-16");
  const sizeB = (await size(b, pane))!;
  expect(sizeB[0]).toBeLessThan(sizeA[0]);
  // A now draws at B's size rather than resizing the pane back.
  await expect.poll(() => size(a, pane)).toEqual(sizeB);
  await run(b, pane, "tput cols", `\n${sizeB[0]}`);

  // Typing in A takes the size back.
  await run(a, pane, "tput cols", `\n${sizeA[0]}`);
  await expect.poll(() => size(b, pane)).toEqual(sizeA);
});
