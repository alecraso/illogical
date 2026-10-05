// A Next dev server (Turbopack) in a block: the page, its chunks and its
// hot-reload socket all go through the worker and the shim, and an edit to
// the page shows in control's page.

import { readFileSync, writeFileSync } from "node:fs";
import { next, type Server } from "./devservers.ts";
import { expect, openBlock, openDirect, type Opened, record, test } from "./stack.ts";

let s: Server;
test.beforeAll(async () => {
  test.setTimeout(180_000);
  s = await next();
});
test.afterAll(() => s?.stop());

async function edit(b: Opened, text: string) {
  const h = b.frame.locator("#h");
  await expect(h).toBeVisible();
  // Hydrated, and the hot-reload socket has had time to connect.
  await b.frame.page().waitForTimeout(1500);
  await b.frame.evaluate(() => ((window as any).kept = true));
  const t = Date.now();
  const page = `${s.dir}/app/page.jsx`;
  writeFileSync(page, readFileSync(page, "utf8").replace(/next [\w ]+</, `${text}<`));
  await expect(h).toHaveText(text, { timeout: 30_000 });
  // Fast Refresh: the page wasn't reloaded.
  return { refreshMs: Date.now() - t, reloaded: !(await b.frame.evaluate(() => (window as any).kept)) };
}

test("Next: an edit refreshes the block inside control's page", async ({ parent, stack, browserName }) => {
  test.setTimeout(180_000);
  const b = await openBlock(parent, stack, { port: s.port, ready: "#h" });
  const through = await edit(b, `next ${browserName} through`);
  expect(await b.frame.evaluate(() => (window as any).s27shim.sockets)).toBeGreaterThan(0);
  const direct = await edit(await openDirect(parent, stack, { port: s.port, ready: "#h" }), `next ${browserName} direct`);
  await record("next-edit", browserName, { through, direct });
  expect(through.reloaded).toBe(false);
});
