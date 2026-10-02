// #49: a pane that floods output puts a slow page behind, and the daemon
// resyncs it. It catches up with the screen alone, below the scrollback it
// already had, and the pane beside it stays quick.

import { expect, test } from "@playwright/test";
import { active, panes, ready, reset, run, text, type } from "./helpers";

test("a flooded pane catches up without losing its scrollback; its neighbour stays quick", async ({ page }) => {
  test.setTimeout(90_000);
  await reset(page);
  const loud = await active(page);
  await run(page, loud, "clear; echo before-$((2+2))", "before-4");
  await page.evaluate((pane) => window.__illogical.client.intent({ op: "split", pane, edge: "right" }), loud);
  await expect.poll(() => panes(page)).toHaveLength(2);
  const quiet = (await panes(page)).find((p) => p !== loud)!;
  await ready(page, quiet);

  // A phone's CPU, so the page falls behind for sure.
  const cdp = await page.context().newCDPSession(page);
  await cdp.send("Emulation.setCPUThrottlingRate", { rate: 6 });
  await type(page, loud, "timeout 8 yes illogical-flood-line; echo after-$((3+3))\n");
  await page.waitForTimeout(2500);

  // Mid-flood, the neighbour answers.
  await type(page, quiet, "echo quiet-$((2*5))\n");
  const typed = Date.now();
  await expect.poll(() => text(page, quiet), { intervals: [25], timeout: 10_000 }).toContain("quiet-10");
  const echoMs = Date.now() - typed;
  console.log(`echo beside the flood: ${echoMs} ms`);

  await expect.poll(() => text(page, loud), { timeout: 45_000 }).toContain("after-6");
  await cdp.send("Emulation.setCPUThrottlingRate", { rate: 1 });

  // It fell behind and caught up with the screen alone: the gap is marked,
  // and what came before it is still there rather than reset.
  const all = await text(page, loud);
  const gap = all.indexOf("output skipped here");
  expect(gap).toBeGreaterThan(0);
  expect(all.slice(0, gap)).toContain("illogical-flood-line");
  expect(echoMs).toBeLessThan(2000);
});
