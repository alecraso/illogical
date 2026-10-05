// code-server (the release editor blocks run) in a block: the workbench,
// its remote connection (a WebSocket, through the shim), a file opened
// from the explorer, and a webview (Markdown preview), which needs a
// service worker of VS Code's own.

import { codeServer, haveCodeServer, type Server } from "./devservers.ts";
import { controlStats, expect, openBlock, openDirect, type Opened, record, test } from "./stack.ts";

let s: Server;
test.skip(!haveCodeServer(), "fetch code-server first: ./fetch-code-server.sh");
test.beforeAll(async () => {
  s = await codeServer();
});
test.afterAll(() => s?.stop());

async function editor(b: Opened) {
  const t0 = Date.now();
  await b.frame.locator(".monaco-workbench").waitFor({ timeout: 90_000 });
  const item = b.frame.locator('.explorer-item:has-text("hello.txt")');
  await item.waitFor({ timeout: 90_000 });
  const explorerMs = Date.now() - t0;
  await item.click();
  await expect(b.frame.locator(".view-lines").first()).toContainText("hello from the block", { timeout: 30_000 });
  // A webview: Markdown's preview of notes.md.
  await b.frame.locator('.explorer-item:has-text("notes.md")').click();
  await expect(b.frame.locator(".view-lines").first()).toContainText("Notes heading", { timeout: 30_000 });
  await b.frame.locator(".view-lines").first().click();
  await b.frame.page().keyboard.press("F1");
  await b.frame.locator(".quick-input-widget input").fill(">Markdown: Open Preview");
  await b.frame.locator('.quick-input-list .monaco-list-row:has-text("Markdown: Open Preview")').first().click();
  let webview = false;
  try {
    await expect
      .poll(
        async () => {
          for (const f of b.frame.page().frames()) {
            if (await f.locator("h1:has-text('Notes heading')").count().catch(() => 0)) return true;
          }
          return false;
        },
        { timeout: 30_000 },
      )
      .toBe(true);
    webview = true;
  } catch {
    await b.frame.page().screenshot({ path: `test-results/cs-webview-${b.origin.slice(8, 16)}.png` });
  }
  return { explorerMs, webview };
}

test("code-server: the workbench, a file and a webview", async ({ parent, stack, browserName }) => {
  test.setTimeout(300_000);
  const q = `/?folder=${encodeURIComponent(`${s.dir}/project`)}`;
  const before = await controlStats(stack);
  const through = await editor(await openBlock(parent, stack, { port: s.port, path: q, ready: ".monaco-workbench" }));
  const after = await controlStats(stack);
  const appWorkers = after.app_workers - before.app_workers;
  const misses = after.misses - before.misses;
  const direct = await editor(await openDirect(parent, stack, { port: s.port, path: q, ready: ".monaco-workbench" }));
  await record("code-server", browserName, { through, direct, appWorkers, misses });
  expect(direct.webview).toBe(true);
  // VS Code registers two workers of its own (the PWA's and the webview's),
  // both run inside the block's worker, and the webview works.
  expect(appWorkers).toBeGreaterThanOrEqual(2);
  expect(misses).toBe(0);
  expect(through.webview).toBe(true);
});
