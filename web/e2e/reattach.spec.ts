// M0's promises, still true with many panes: the daemon owns the terminals,
// the page is disposable.

import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test } from "@playwright/test";
import { active, open, panes, ready, reset, run, screen, size, text, type, closeContexts } from "./helpers";

test.afterAll(closeContexts);

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
  // The last command redraws and then says so: once the page shows that,
  // nvim has drawn everything before it.
  await page.keyboard.type(":set number cursorline\n:vsplit\n30Gzz:syntax on\n:redraw | echo 'drawn-'.(6*7)\n", { delay: 2 });
  await expect.poll(() => screen(page, pane)).toContain("drawn-42");
  expect(await screen(page, pane)).toMatch(/\b30 /);
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

test("output produced while no browser is attached is complete", async ({ browser, request }) => {
  const ctx = await browser.newContext();
  let page = await ctx.newPage();
  await reset(page);
  const pane = await active(page);
  await run(page, pane, "clear; echo ready-$((5+5))", "ready-10");
  // Half the lines, then a wait for the test to close the page, then the
  // rest while nobody is attached. (It slept 0.5 ms a line, hoping to be
  // detached mid-command: 2000 runs of /bin/sleep took 17 s on a Mac, as
  // long as the deadline.)
  const dir = mkdtempSync(join(tmpdir(), "arugula-e2e-reattach-"));
  const go = join(dir, "go");
  await type(
    page,
    pane,
    `for i in $(seq 1 2000); do echo line-$i; if [ $i = 1000 ]; then until [ -e ${go} ]; do sleep 0.05; done; fi; done; echo loop-$((1000+1))\n`,
  );
  await expect.poll(() => text(page, pane)).toContain("line-1000");
  await ctx.close(); // mid-command
  writeFileSync(go, "");
  // The daemon's screen says when it has ended.
  const capture = async () => (await request.get(`/api/panes/${pane}/capture?format=text`)).text();
  await expect.poll(capture).toContain("loop-1001");
  const ctx2 = await browser.newContext();
  page = await ctx2.newPage();
  await open(page);
  await ready(page, pane);
  await expect.poll(() => text(page, pane)).toContain("loop-1001");
  const seen = new Set((await text(page, pane)).match(/^line-\d+$/gm));
  const missing = Array.from({ length: 2000 }, (_, i) => `line-${i + 1}`).filter((l) => !seen.has(l));
  expect(missing).toEqual([]);
  await ctx2.close();
  rmSync(dir, { recursive: true, force: true });
});

// #333: typing takes the size only once the window that has it has left
// the keyboard for SIZE_HOLD (3 s); until then the other types into it.
test("two clients see the same output; typing takes the size once its owner is idle", async ({ browser }) => {
  const hold = 3_500;
  const a = await (await browser.newContext({ viewport: { width: 1000, height: 640 } })).newPage();
  await reset(a);
  const b = await (await browser.newContext({ viewport: { width: 760, height: 500 } })).newPage();
  await open(b);
  const pane = (await panes(a))[0];
  await ready(b, pane);

  // B opened the tab last, so the size is B's until it has been idle a while.
  await a.waitForTimeout(hold);
  await run(a, pane, "clear; echo from-a-$((3*3))", "from-a-9");
  await expect.poll(() => text(b, pane)).toContain("from-a-9");
  const sizeA = (await size(a, pane))!;
  await expect.poll(() => size(b, pane)).toEqual(sizeA);
  await run(a, pane, "tput cols", `\n${sizeA[0]}`);

  // B types straight after A: A keeps the size, and B types into it.
  await run(b, pane, "echo from-b-$((4*4)); tput cols", "from-b-16");
  await expect.poll(() => text(a, pane)).toContain("from-b-16");
  expect(await text(b, pane)).toContain(`from-b-16\n${sizeA[0]}`);
  expect(await size(a, pane)).toEqual(sizeA);
  expect(await size(b, pane)).toEqual(sizeA);

  // Once A has been idle past the hold, B's typing takes the size, and A
  // draws at it rather than resizing the pane back.
  await b.waitForTimeout(hold);
  await type(b, pane, "echo from-b-$((5*5))\n");
  await expect.poll(async () => (await size(b, pane))![0]).toBeLessThan(sizeA[0]);
  const sizeB = (await size(b, pane))!;
  await expect.poll(() => size(a, pane)).toEqual(sizeB);
  await run(b, pane, "tput cols", `\n${sizeB[0]}`);
  await expect.poll(() => text(a, pane)).toContain("from-b-25");
  await Promise.all([a.context().close(), b.context().close()]);
});

test("a page opened while a full-screen app is mid escape sequence draws the rest right (#53)", async ({ browser }) => {
  const ctx = await browser.newContext();
  const page = await ctx.newPage();
  await reset(page);
  const pane = await active(page);
  // The stream stops inside an SGR, under an alt screen, until the test
  // lets it go on (a fixed sleep there raced a slow attach).
  const dir = mkdtempSync(join(tmpdir(), "arugula-e2e-reattach-"));
  const go = join(dir, "go");
  await run(
    page,
    pane,
    `clear; printf '\\e[?1049h\\e[H\\e[2Jalt-%s \\e[38;2;1' $((6*7)); until [ -e ${go} ]; do sleep 0.05; done; printf ';2;3mafter-%s' $((2+2)); sleep 600`,
    "alt-42",
  );
  // A page attaching now gets a snapshot taken mid-sequence.
  const ctx2 = await browser.newContext();
  const late = await ctx2.newPage();
  await open(late);
  await ready(late, pane);
  expect(await screen(late, pane)).not.toContain("after-4");
  writeFileSync(go, "");
  for (const p of [page, late]) {
    await expect.poll(() => screen(p, pane), { timeout: 10_000 }).toContain("alt-42 after-4");
    expect(await screen(p, pane)).not.toContain(";2;3m");
  }
  // And the daemon's own terminal kept the sequence whole: a fresh snapshot.
  const ctx3 = await browser.newContext();
  const fresh = await ctx3.newPage();
  await open(fresh);
  await ready(fresh, pane);
  await expect.poll(() => screen(fresh, pane)).toBe(await screen(late, pane));
  expect(await screen(fresh, pane)).not.toContain(";2;3m");
  await type(page, pane, "\x03");
  await Promise.all([ctx.close(), ctx2.close(), ctx3.close()]);
  rmSync(dir, { recursive: true, force: true });
});
