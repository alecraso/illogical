// M2: stop the daemon the way a reboot does (its shells die with it), start
// it again, and the layout, scrollback and working directories come back,
// with each pane doing what its restart policy says.

import { spawn, type ChildProcess } from "node:child_process";
import { mkdtempSync, realpathSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test } from "@playwright/test";
import { active, menu, open, paneEl, panes, ready, run, tab, tabsInSession, text, type } from "./helpers";
import { ANY, daemonPort } from "./ports";
import { labs } from "./labs";

let PORT = 0;
let state = "";
test.use({ baseURL: async ({}, use) => use(`http://127.0.0.1:${PORT}`) });

/** Start the daemon: on a port of its choosing, then on the same one again. */
async function startDaemon(): Promise<ChildProcess> {
  const d = spawn(
    "../target/debug/illogicald",
    ["--listen", PORT ? `127.0.0.1:${PORT}` : ANY, "--shell", "bash --norc --noprofile", "--no-manager-env", "--state-dir", labs(state)],
    { stdio: "ignore" },
  );
  PORT ||= await daemonPort(state, d);
  for (let i = 0; i < 100; i++) {
    try {
      if ((await fetch(`http://127.0.0.1:${PORT}/`)).ok) return d;
    } catch {
      // not up yet
    }
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error("daemon did not start");
}

async function stopDaemon(d: ChildProcess) {
  const exited = new Promise((r) => d.once("exit", r));
  d.kill("SIGTERM");
  await exited;
}

let daemon: ChildProcess | undefined;
test.beforeAll(async () => {
  state = mkdtempSync(join(tmpdir(), "illogical-e2e-restore-"));
  daemon = await startDaemon();
});
test.afterAll(() => {
  daemon?.kill("SIGKILL");
  if (state) rmSync(state, { recursive: true, force: true });
});

test("after a restart: tabs, splits, cwd and scrollback are back; policies apply", async ({ page }) => {
  await open(page);
  const first = await active(page);
  await ready(page, first);
  await run(page, first, "cd /tmp && echo scroll-$((7*7))", "scroll-49");

  // A second pane running something, set to re-run it (asking first).
  await menu(page, paneEl(page, first), "Split right");
  await expect.poll(() => panes(page)).toHaveLength(2);
  const second = await active(page);
  await ready(page, second);
  await run(page, second, "bash -c 'echo rr-$((2*2)); sleep 300; true'", "rr-4");
  await expect
    .poll(() => page.evaluate((p) => window.__illogical.client.info(p)?.command, second), { timeout: 10_000 })
    .toContain("sleep 300");
  await paneEl(page, second).click({ button: "right", position: { x: 60, y: 60 } });
  await page.getByRole("menuitemradio", { name: /asking first/ }).click();

  // The first pane runs a command of our choosing when restored.
  await paneEl(page, first).click({ button: "right", position: { x: 60, y: 60 } });
  await page.getByRole("menuitemradio", { name: /Run a command/ }).click();
  await page.getByRole("dialog").getByRole("textbox").fill("echo hook-$((3*3))");
  await page.getByRole("button", { name: "OK" }).click();
  await expect
    .poll(() => page.evaluate((p) => window.__illogical.client.info(p)?.policy, first))
    .toEqual({ kind: "hook", command: "echo hook-$((3*3))" });

  // A second tab with a name.
  await page.getByTitle("New tab").click();
  await expect.poll(() => tabsInSession(page)).toHaveLength(2);
  const t2 = (await tabsInSession(page))[1];
  await page.locator(`[data-tab-id="${t2}"]`).dblclick();
  await page.locator("input.rename").fill("two");
  await page.keyboard.press("Enter");
  await expect(page.locator(`[data-tab-id="${t2}"]`)).toContainText("two");
  await page.waitForTimeout(400); // the layout save is debounced

  // "Reboot".
  await stopDaemon(daemon);
  daemon = await startDaemon();
  await page.reload();
  await open(page);

  expect(await tabsInSession(page)).toHaveLength(2);
  await expect(page.locator(`[data-tab-id="${t2}"]`)).toContainText("two");
  const [t1] = await tabsInSession(page);
  await page.locator(`[data-tab-id="${t1}"]`).click();
  await expect.poll(() => tab(page).then((t) => t.id)).toBe(t1);
  expect(await panes(page)).toEqual([first, second]);

  // Scrollback, the restore marker, and the hook ran in the old directory.
  await ready(page, first);
  await expect.poll(() => text(page, first)).toContain("scroll-49");
  await expect.poll(() => text(page, first)).toContain("restored");
  await expect.poll(() => text(page, first)).toContain("hook-9");
  // The daemon keeps the real path: /private/tmp on macOS.
  await run(page, first, "echo at-$(pwd)", `at-${realpathSync("/tmp")}`);

  // The re-run pane asks first; its button runs it.
  await ready(page, second);
  await expect.poll(() => text(page, second)).toContain("press Enter to re-run");
  const start = paneEl(page, second).getByRole("button", { name: "Re-run" });
  await expect(start).toBeVisible();
  await start.click();
  await expect.poll(async () => (await text(page, second)).match(/^rr-4$/gm)?.length).toBe(2);
  await expect(start).toBeHidden();
  await type(page, second, "\x03");
});
