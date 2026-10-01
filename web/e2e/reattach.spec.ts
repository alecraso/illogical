// M0 in a real browser: the daemon owns the terminal, the page is
// disposable. Each test shares one daemon (one pane), so each one starts by
// getting the shell back to a known state.

import { expect, test, type Page } from "@playwright/test";

declare global {
  interface Window {
    __illogical: { text(): string; offset(): number | null; size(): [number, number] };
  }
}

const text = (page: Page) => page.evaluate(() => window.__illogical.text());
const size = (page: Page) => page.evaluate(() => window.__illogical.size());
/** The visible screen: the last `rows` lines of the active buffer. */
async function screen(page: Page) {
  const [, rows] = await size(page);
  return (await text(page)).split("\n").slice(-rows).map((l) => l.trimEnd()).join("\n");
}

async function open(page: Page) {
  await page.goto("/");
  await expect.poll(() => page.evaluate(() => window.__illogical?.offset())).not.toBeNull();
  await expect(page.locator("#status")).toBeHidden();
}

async function type(page: Page, s: string) {
  await page.locator(".xterm").click();
  await page.keyboard.type(s, { delay: 2 });
}

async function run(page: Page, cmd: string, marker: string) {
  await type(page, `${cmd}\n`);
  await expect.poll(() => text(page)).toContain(marker);
}

test("nvim survives closing and reopening the page", async ({ browser }) => {
  const ctx = await browser.newContext();
  let page = await ctx.newPage();
  await open(page);
  await run(page, "clear; seq 1 40; echo before-nvim-$((1+1))", "before-nvim-2");
  await type(page, "nvim -u NONE -i NONE /etc/services\n");
  await expect.poll(() => screen(page)).toContain("/etc/services");
  // Scrolling one side of a vertical split is where a renderer that lacks
  // what the program was promised (left/right margins) draws garbage.
  await type(page, ":set number cursorline\n:vsplit\n30Gzz:syntax on\n");
  await expect.poll(() => screen(page)).toMatch(/\b30 /);
  await page.waitForTimeout(300);
  const before = await screen(page);
  await ctx.close();

  // A different browser context: no state survives except on the daemon.
  const ctx2 = await browser.newContext();
  page = await ctx2.newPage();
  await open(page);
  await expect.poll(() => screen(page)).toBe(before);

  // Leaving nvim brings back the shell output from before it started.
  await type(page, ":qa!\n");
  await expect.poll(() => screen(page)).toContain("before-nvim-2");
  await run(page, "echo after-nvim-$((2+2))", "after-nvim-4");
  await ctx2.close();
});

test("output produced while no browser is attached is complete", async ({ browser }) => {
  const ctx = await browser.newContext();
  let page = await ctx.newPage();
  await open(page);
  await run(page, "clear; echo ready", "ready");
  await type(page, "for i in $(seq 1 2000); do echo line-$i; sleep 0.0005; done; echo loop-$((1000+1))\n");
  await expect.poll(() => text(page)).toContain("line-50");
  await ctx.close(); // mid-command

  await new Promise((r) => setTimeout(r, 2500));
  const ctx2 = await browser.newContext();
  page = await ctx2.newPage();
  await open(page);
  await expect.poll(() => text(page), { timeout: 15_000 }).toContain("loop-1001");
  const all = await text(page);
  const seen = new Set(all.match(/^line-\d+$/gm));
  const missing = Array.from({ length: 2000 }, (_, i) => `line-${i + 1}`).filter((l) => !seen.has(l));
  expect(missing).toEqual([]);
  await ctx2.close();
});

test("two clients see the same output; the last to type sets the size", async ({ browser }) => {
  const a = await (await browser.newContext({ viewport: { width: 1000, height: 640 } })).newPage();
  const b = await (await browser.newContext({ viewport: { width: 700, height: 500 } })).newPage();
  await open(a);
  await open(b);

  await run(a, "clear; echo from-a-$((3*3))", "from-a-9");
  await expect.poll(() => text(b)).toContain("from-a-9");
  const sizeA = await size(a);

  await run(b, "echo from-b-$((4*4))", "from-b-16");
  await expect.poll(() => text(a)).toContain("from-b-16");
  const sizeB = await size(b);
  expect(sizeB[0]).toBeLessThan(sizeA[0]);
  // A now draws at B's size rather than resizing the pane back.
  await expect.poll(() => size(a)).toEqual(sizeB);
  await run(b, "tput cols", String(sizeB[0]));

  // Typing in A takes the size back.
  await run(a, "tput cols", String(sizeA[0]));
  await expect.poll(() => size(b)).toEqual(sizeA);
});
