// #17 (M4's option (a)): one layout, panes from several daemons. The home
// daemon's tab bar holds a tab whose shell runs on another host, and a
// split of a home tab holds another; the page reaches that host directly
// for their bytes. Moving one works like a local pane, live on every
// window; with the host down they say so, and come back by themselves.

import { spawn, type ChildProcess } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test, type Page } from "@playwright/test";
import { active, at, dragTo, menu, open, paneEl, panes, ready, run, text, type, closeContexts } from "./helpers";
import { ANY, daemonPort } from "./ports";

test.afterAll(closeContexts);

let homeUrl = "";
let otherUrl = "";
const states = new Map<string, string>();
const ports = new Map<string, number>();
const daemons = new Map<string, ChildProcess>();

test.use({ baseURL: async ({}, use) => use(homeUrl) });
test.describe.configure({ mode: "serial" });

/** Start a daemon; again on the same state and port after `stopDaemon` (a
 * reboot). Its URL. */
async function startDaemon(name: string, extra: string[] = []) {
  const state = states.get(name) ?? mkdtempSync(join(tmpdir(), `illogical-e2e-remote-${name}-`));
  states.set(name, state);
  const known = ports.get(name);
  const d = spawn(
    "../target/debug/illogicald",
    [
      ...["--listen", known ? `127.0.0.1:${known}` : ANY, "--name", name, "--state-dir", state],
      ...["--shell", "bash --norc --noprofile", "--no-manager-env", "--tailscale-socket", "/nonexistent/tailscaled.sock"],
      ...extra,
    ],
    { stdio: "ignore" },
  );
  daemons.set(name, d);
  const port = known ?? (await daemonPort(state, d));
  ports.set(name, port);
  for (let i = 0; i < 100; i++) {
    try {
      if ((await fetch(`http://127.0.0.1:${port}/api/host`)).ok) return `http://127.0.0.1:${port}`;
    } catch {
      // not up yet
    }
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error(`daemon ${name} did not start`);
}

async function stopDaemon(name: string) {
  const d = daemons.get(name);
  if (!d) return;
  const exited = new Promise((r) => d.once("exit", r));
  d.kill("SIGTERM");
  await exited;
  daemons.delete(name);
}

const startOther = () => startDaemon("other", ["--allow-origin", homeUrl]);

test.beforeAll(async () => {
  homeUrl = await startDaemon("home");
  otherUrl = await startOther();
  const res = await fetch(`${homeUrl}/api/hosts`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ name: "other", urls: [otherUrl] }),
  });
  expect(res.ok).toBe(true);
});

test.afterAll(() => {
  for (const d of daemons.values()) d.kill("SIGKILL");
  for (const s of states.values()) rmSync(s, { recursive: true, force: true });
});

interface ApiPane {
  id: number;
  type: string;
  session_name: string;
}
const panesOf = async (url: string) => (await (await fetch(`${url}/api/panes`)).json()) as ApiPane[];
/** The remote blocks in the page's layout: id here → pane there. */
const remoteBlocks = (page: Page) =>
  page.evaluate(() => {
    const c = window.__illogical.client;
    return c.state!.panes.filter((p) => p.type === "remote").map((p) => [p.id, (c.blocks.get(p.id)?.state as { pane: number }).pane]);
  });
const stateOf = (page: Page, id: number) => paneEl(page, id).locator(".block-remote").getAttribute("data-state");

test("a tab and a split from another host, moved like local panes, through the host going away", async ({ page, browser }) => {
  await open(page);
  const tabs0 = await page.evaluate(() => window.__illogical.client.state!.tabs.length);
  const local = await active(page);
  await ready(page, local);

  // One tab bar: a home tab and a tab whose shell runs on the other host.
  await page.locator(".new-tab").click({ button: "right" });
  await page.getByRole("menuitem", { name: "New tab on other" }).click();
  await expect.poll(() => page.evaluate(() => window.__illogical.client.state!.tabs.length)).toBe(tabs0 + 1);
  await expect.poll(() => remoteBlocks(page)).toHaveLength(1);
  const [[tabBlock, tabPane]] = await remoteBlocks(page);
  await expect.poll(() => active(page)).toBe(tabBlock);
  await expect(page.locator(".tab.selected .host-tag.remote")).toHaveText("other");
  await expect(paneEl(page, tabBlock).locator(".remote-badge")).toHaveText("other");
  await expect.poll(() => stateOf(page, tabBlock)).toBe("live");
  await run(page, tabBlock, "echo on-other-$((6*7))", "on-other-42");
  // It is the other daemon's pane, in a session named after home; home
  // holds only its place.
  const there = (await panesOf(otherUrl)).find((p) => p.id === tabPane)!;
  expect(there).toMatchObject({ type: "terminal", session_name: "home" });
  expect((await panesOf(homeUrl)).find((p) => p.id === tabBlock)?.type).toBe("remote");

  // Its size there is its place here.
  const sizes = () =>
    page.evaluate(
      ([block, pane]) => {
        const c = window.__illogical.client;
        const here = c.tabOfPane(block)!.layout.panes.find(([id]) => id === block)![1];
        const rc = window.__illogical.remotes.client("other")!;
        const t = rc.tabOfPane(pane);
        const r = t?.zoom === pane ? { cols: t.cols, rows: t.rows } : t?.layout.panes.find(([id]) => id === pane)?.[1];
        return [here.cols, here.rows, r?.cols, r?.rows];
      },
      [tabBlock, tabPane],
    );
  await expect.poll(async () => { const [a, b, c, d] = await sizes(); return a === c && b === d; }).toBe(true);

  // A split of the home tab holds a shell on the other host too.
  await page.locator(".tab").first().click();
  await expect.poll(() => active(page)).toBe(local);
  await menu(page, paneEl(page, local), "Split right on other");
  await expect.poll(() => remoteBlocks(page)).toHaveLength(2);
  const [splitBlock, splitPane] = (await remoteBlocks(page)).find(([b]) => b !== tabBlock)!;
  await expect.poll(() => panes(page)).toEqual([local, splitBlock]);
  await expect.poll(() => stateOf(page, splitBlock)).toBe("live");
  await run(page, splitBlock, "echo split-$((5*5))", "split-25");
  await run(page, local, "echo home-$((3*3))", "home-9");
  expect((await panesOf(otherUrl)).filter((p) => p.session_name === "home").map((p) => p.id).sort()).toEqual(
    [tabPane, splitPane].sort(),
  );

  // A second window shows the same layout, live.
  const ctx2 = await browser.newContext({ baseURL: homeUrl });
  const page2 = await ctx2.newPage();
  await open(page2);
  await page2.evaluate((b) => window.__illogical.client.setActive(b), splitBlock);
  await expect.poll(() => panes(page2)).toEqual([local, splitBlock]);
  await expect.poll(() => text(page2, splitBlock)).toContain("split-25");

  // Drag the remote pane by its grip onto the local pane's left edge: it
  // moves like a local one, without redrawing from scratch, in both windows.
  await paneEl(page, splitBlock).hover();
  await dragTo(page, paneEl(page, splitBlock).locator(".grip"), await at(paneEl(page, local), 0.08, 0.5));
  await expect.poll(() => panes(page)).toEqual([splitBlock, local]);
  await expect.poll(() => panes(page2)).toEqual([splitBlock, local]);
  expect(await text(page, splitBlock)).toContain("split-25");
  await run(page, splitBlock, "echo moved-$((2*4))", "moved-8");
  await expect.poll(() => text(page2, splitBlock)).toContain("moved-8");
  // ...and into a tab of its own, then back beside the local pane.
  await dragTo(page, paneEl(page, splitBlock).locator(".grip"), await at(page.locator(".new-tab"), -1, 0.5));
  await expect
    .poll(() => page.evaluate((b) => window.__illogical.client.tabOfPane(b)!.layout.panes.map(([id]) => id), splitBlock))
    .toEqual([splitBlock]);
  await expect.poll(() => text(page, splitBlock)).toContain("moved-8");
  await page.evaluate(([b, l]) => window.__illogical.client.intent({ op: "move_pane", pane: b, target: l, edge: "right" }), [splitBlock, local]);
  await expect.poll(() => page.evaluate((l) => window.__illogical.client.tabOfPane(l)!.layout.panes.map(([id]) => id), local)).toEqual([
    local,
    splitBlock,
  ]);

  // The other host goes away: its panes say so, home's pane carries on.
  await page.locator(".tab").first().click();
  await stopDaemon("other");
  await expect.poll(() => stateOf(page, splitBlock)).toBe("unreachable");
  await expect(paneEl(page, splitBlock).locator(".remote-note")).toContainText("other is unreachable");
  await expect.poll(() => stateOf(page2, splitBlock)).toBe("unreachable");
  await run(page, local, "echo still-home", "still-home");
  // It comes back by itself, with its history (the daemon restored it).
  await startOther();
  await expect.poll(() => stateOf(page, splitBlock), { timeout: 20_000 }).toBe("live");
  await expect.poll(() => stateOf(page2, splitBlock), { timeout: 20_000 }).toBe("live");
  await expect.poll(() => text(page, splitBlock)).toContain("moved-8");
  await run(page, splitBlock, "echo back-$((7*3))", "back-21");
  await ctx2.close();

  // Closing it here closes it there.
  await menu(page, paneEl(page, splitBlock), "Close pane");
  await expect.poll(() => panes(page)).toEqual([local]);
  await expect.poll(async () => (await panesOf(otherUrl)).some((p) => p.id === splitPane)).toBe(false);

  // Closed on its host (it exited there): its place here goes too.
  await page.locator(`[data-tab-id]`).last().click();
  await expect.poll(() => active(page)).toBe(tabBlock);
  await type(page, tabBlock, "exit\n");
  await expect.poll(async () => (await panesOf(otherUrl)).some((p) => p.id === tabPane)).toBe(false);
  await expect.poll(() => remoteBlocks(page)).toHaveLength(0);
  await expect.poll(() => page.evaluate(() => window.__illogical.client.state!.tabs.length)).toBe(tabs0);
});
