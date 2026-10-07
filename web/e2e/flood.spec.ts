// #49: a pane that floods output puts a slow page behind, and the daemon
// resyncs it. It catches up with the screen alone, below the scrollback it
// already had, and the pane beside it stays quick. #52: the page acks what
// it has drawn, so it is never far behind and catches up as the flood ends.

import { expect, test } from "@playwright/test";
import { active, panes, ready, reset, run, screen, text, type } from "./helpers";

test("a flooded pane catches up without losing its scrollback; its neighbour stays quick", async ({ page }) => {
  test.setTimeout(90_000);
  await reset(page);
  const loud = await active(page);
  await run(page, loud, "clear; echo before-$((2+2))", "before-4");
  await page.evaluate((pane) => window.__arugula.client.intent({ op: "split", pane, edge: "right" }), loud);
  await expect.poll(() => panes(page)).toHaveLength(2);
  const quiet = (await panes(page)).find((p) => p !== loud)!;
  await ready(page, quiet);

  // A phone's CPU, so the page falls behind for sure.
  const cdp = await page.context().newCDPSession(page);
  await cdp.send("Emulation.setCPUThrottlingRate", { rate: 6 });
  await type(page, loud, "timeout 8 yes arugula-flood-line; echo after-$((3+3))\n");
  const flooding = Date.now();
  await page.waitForTimeout(2500);

  // Mid-flood, the neighbour answers.
  await type(page, quiet, "echo quiet-$((2*5))\n");
  const typed = Date.now();
  // The screen, not the whole history: reading 10k rows over and over on a
  // throttled CPU is itself enough to put the page behind.
  await expect.poll(() => screen(page, quiet), { intervals: [25], timeout: 10_000 }).toContain("quiet-10");
  const echoMs = Date.now() - typed;
  console.log(`echo beside the flood: ${echoMs} ms`);

  await expect.poll(() => screen(page, loud), { intervals: [100], timeout: 45_000 }).toContain("after-6");
  const caughtUp = Date.now() - flooding - 8000;
  console.log(`caught up ${caughtUp} ms after the flood ended`);
  await cdp.send("Emulation.setCPUThrottlingRate", { rate: 1 });

  // Held back past what the log replays, it caught up with the screen
  // alone: the gap is marked, and what came before it is still there rather
  // than reset. (A slower daemon may only ever replay: no gap at all.)
  const all = await text(page, loud);
  const gap = all.indexOf("output skipped here");
  if (gap >= 0) expect(all.slice(0, gap)).toContain("arugula-flood-line");
  expect(all).toContain("arugula-flood-line");
  expect(echoMs).toBeLessThan(2000);
  expect(caughtUp).toBeLessThan(2000);
});
