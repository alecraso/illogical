// A real Vite dev server in a block: it loads through the worker, its
// hot-reload socket goes through the shim, and a save shows in control's
// page, both as a CSS hot update (no reload) and as a full reload.

import { writeFileSync } from "node:fs";
import { type Server, vite } from "./devservers.ts";
import { expect, openBlock, openDirect, type Opened, record, test } from "./stack.ts";

let s: Server;
test.beforeAll(async () => {
  s = await vite();
});
test.afterAll(() => s?.stop());

async function saves(b: Opened, n: number) {
  const h = b.frame.locator("#h");
  const msg = b.frame.locator("#msg");
  await expect(msg).toHaveText(/message/);
  // Give Vite's client a moment to connect its socket.
  await b.frame.page().waitForTimeout(500);
  await b.frame.evaluate(() => ((window as any).kept = true));
  let t = Date.now();
  writeFileSync(`${s.dir}/style.css`, `#h { color: rgb(${n}, 5, 6); }\n`);
  await expect(h).toHaveCSS("color", `rgb(${n}, 5, 6)`, { timeout: 15_000 });
  const css = Date.now() - t;
  // A CSS update is hot: the page wasn't reloaded.
  expect(await b.frame.evaluate(() => (window as any).kept)).toBe(true);
  t = Date.now();
  writeFileSync(`${s.dir}/main.js`, `document.querySelector("#msg").textContent = "message ${n}";\n`);
  await expect(msg).toHaveText(`message ${n}`, { timeout: 15_000 });
  const js = Date.now() - t;
  // main.js accepts no update, so Vite reloads the page.
  expect(await b.frame.evaluate(() => (window as any).kept)).toBeUndefined();
  return { cssHotMs: css, jsReloadMs: js };
}

test("Vite: a save reloads the block inside control's page", async ({ parent, stack, browserName }) => {
  const b = await openBlock(parent, stack, { port: s.port, ready: "#msg" });
  const through = await saves(b, 11);
  // The hot-reload socket went through the shim (one per page load).
  expect(await b.frame.evaluate(() => (window as any).s27shim.sockets)).toBeGreaterThan(0);
  const direct = await saves(await openDirect(parent, stack, { port: s.port, ready: "#msg" }), 22);
  await record("vite-save", browserName, { through, direct });
});
