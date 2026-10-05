// #17 on two machines (testnet/hosts): `home` and `mac` are containers,
// each with its own illogicald, and the page is home's. A tab and a split
// of home's layout hold shells on mac. Then mac drops off the network
// (`docker network disconnect`: no clean close, its connections just go
// quiet) with its daemon and shells still running, and comes back.
// #17's done-when: its panes show as unreachable while it's away and come
// back on their own when it returns; home's panes carry on.
//
// Runs only with ILLOGICAL_TESTNET_HOSTS=1 (`just testnet-hosts`), which
// needs Docker and the static build for this machine.

import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { expect, test, type Page } from "@playwright/test";
import { active, menu, open, paneEl, panes, ready, run, text } from "./helpers";

const enabled = process.env.ILLOGICAL_TESTNET_HOSTS === "1";
const homePort = Number(process.env.ILLOGICAL_TESTNET_HOME_PORT) || 17748;
const macPort = Number(process.env.ILLOGICAL_TESTNET_MAC_PORT) || 17749;
const homeUrl = `http://127.0.0.1:${homePort}`;
const macUrl = `http://127.0.0.1:${macPort}`;
const net = fileURLToPath(new URL("../../testnet/hosts/net.sh", import.meta.url));
const sh = (...args: string[]) => execFileSync(net, args, { stdio: ["ignore", "inherit", "inherit"], env: process.env });

test.skip(!enabled, "ILLOGICAL_TESTNET_HOSTS=1 runs it (just testnet-hosts)");
test.use({ baseURL: homeUrl });
test.describe.configure({ mode: "serial" });

test.beforeAll(async () => {
  test.setTimeout(120_000);
  sh("up");
  for (const url of [homeUrl, macUrl]) {
    await expect
      .poll(async () => {
        try {
          return (await fetch(`${url}/api/host`)).ok;
        } catch {
          return false;
        }
      }, { timeout: 30_000 })
      .toBe(true);
  }
  const res = await fetch(`${homeUrl}/api/hosts`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ name: "mac", urls: [macUrl] }),
  });
  expect(res.ok).toBe(true);
});

test.afterAll(() => {
  if (enabled && !process.env.ILLOGICAL_TESTNET_KEEP) sh("down");
});

const remoteBlocks = (page: Page) =>
  page.evaluate(() => {
    const c = window.__illogical.client;
    return c.state!.panes.filter((p) => p.type === "remote").map((p) => [p.id, (c.blocks.get(p.id)?.state as { pane: number }).pane]);
  });
const stateOf = (page: Page, id: number) => paneEl(page, id).locator(".block-remote").getAttribute("data-state");
/** The highest `tick-N` a pane shows. */
const lastTick = async (page: Page, id: number) => Math.max(0, ...[...(await text(page, id)).matchAll(/tick-(\d+)/g)].map((m) => Number(m[1])));

test("mac's panes go unreachable when it drops off the network and come back by themselves", async ({ page }) => {
  test.setTimeout(240_000);
  await open(page);
  const local = await active(page);
  await ready(page, local);

  // A tab on mac, and a split of home's tab on mac.
  await page.locator(".new-tab").click({ button: "right" });
  await page.getByRole("menuitem", { name: "New tab on mac" }).click();
  await expect.poll(() => remoteBlocks(page)).toHaveLength(1);
  const [[tabBlock]] = await remoteBlocks(page);
  await expect.poll(() => stateOf(page, tabBlock)).toBe("live");
  await run(page, tabBlock, "echo on-$(hostname)", "on-mac");
  await page.locator(".tab").first().click();
  await expect.poll(() => active(page)).toBe(local);
  await menu(page, paneEl(page, local), "Split right on mac");
  await expect.poll(() => remoteBlocks(page)).toHaveLength(2);
  const [splitBlock] = (await remoteBlocks(page)).find(([b]) => b !== tabBlock)!;
  await expect.poll(() => panes(page)).toEqual([local, splitBlock]);
  await expect.poll(() => stateOf(page, splitBlock)).toBe("live");
  await run(page, local, "echo on-$(hostname)", "on-home");
  // Something that keeps going on mac while it's away.
  await run(page, splitBlock, "for i in $(seq 1 900); do echo tick-$i; sleep 1; done", "tick-2");

  // mac drops off the network: nothing closes, its connections go quiet.
  const before = await lastTick(page, splitBlock);
  const t0 = Date.now();
  sh("offline", "mac");
  await expect.poll(() => stateOf(page, splitBlock), { timeout: 90_000 }).toBe("unreachable");
  const noticed = Date.now() - t0;
  await expect(paneEl(page, splitBlock).locator(".remote-note")).toContainText("mac is unreachable");
  await page.locator(`[data-tab-id]`).last().click();
  await expect.poll(() => stateOf(page, tabBlock), { timeout: 30_000 }).toBe("unreachable");
  await page.locator(".tab").first().click();
  // Home is unaffected.
  await run(page, local, "echo still-$(hostname)", "still-home");
  // Away a while: mac's loop keeps counting there.
  await page.waitForTimeout(8_000);
  const during = await lastTick(page, splitBlock);

  // Back on the network: live again with no reload, with what it printed
  // while away.
  const t1 = Date.now();
  sh("online", "mac");
  await expect.poll(() => stateOf(page, splitBlock), { timeout: 90_000 }).toBe("live");
  const back = Date.now() - t1;
  await expect.poll(() => lastTick(page, splitBlock), { timeout: 30_000 }).toBeGreaterThan(during + 8);
  await page.locator(`[data-tab-id]`).last().click();
  await expect.poll(() => stateOf(page, tabBlock), { timeout: 30_000 }).toBe("live");
  await run(page, tabBlock, "echo back-$((6*7))", "back-42");
  console.log(`mac offline: unreachable after ${noticed} ms; live again ${back} ms after it returned (ticks ${before} → ${during} while away)`);
});
