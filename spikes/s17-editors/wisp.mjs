// S17: code-server in a wisp sandbox (Firecracker microVM) on geek.
//
//   node spikes/s17-editors/wisp.mjs <code-server.tgz>
//
// Creates a throwaway sprite (s17-editor), pushes the release tarball and wisp-inner.sh, starts
// code-server as a sprite service on :8080 with no auth of its own, opens it through the sprite URL
// (wispd's proxy on 127.0.0.1:7788, reached by the sprite's name under its url-domain, widgets.wtf
// here, with a Bearer token) in headless Chrome, and times the workbench and a file. Memory is
// measured inside the VM. Then the page closes, the sprite is left to suspend, and the page is
// opened again to time a wake. The sprite is destroyed at the end.

import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const require = createRequire("/home/me/dev/jhgaylor/illogical/web/package.json");
const { chromium } = require("@playwright/test");
const [tgz] = process.argv.slice(2);
const NAME = "s17-editor";
const token = readFileSync(path.join(os.homedir(), ".local/share/wisp/token"), "utf8").trim();
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const sprite = (...a) => execFileSync(path.join(here, "sprite.sh"), a, { maxBuffer: 1 << 26 }).toString();
const inner = (cmd) => sprite("exec", "-s", NAME, "--", "bash", "/home/sprite/wisp-inner.sh", cmd);
const state = async () => {
  const r = await fetch(`http://127.0.0.1:7788/v1/sprites/${NAME}`, { headers: { Authorization: `Bearer ${token}` } });
  return (await r.json()).status;
};
const out = {};
const t = () => Date.now();
let t0;
let browser;
try {
  t0 = t();
  sprite("create", "--skip-console", NAME);
  out.create_ms = t() - t0;
  t0 = t();
  sprite("file", "push", "-s", NAME, tgz, "/home/sprite/cs.tgz");
  sprite("file", "push", "-s", NAME, path.join(here, "wisp-inner.sh"), "/home/sprite/wisp-inner.sh");
  out.push_ms = t() - t0;
  out.setup = inner("setup").trim();
  out.start = inner("start").trim();
  out.mem_server_only = inner("mem").trim();

  // The sprite URL is <name>.<url-domain> (widgets.wtf here); reach wispd on loopback with that name.
  browser = await chromium.launch({ channel: "chrome", args: [`--host-resolver-rules=MAP ${NAME}.widgets.wtf 127.0.0.1`] });
  const open = async () => {
    const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 }, extraHTTPHeaders: { Authorization: `Bearer ${token}` } });
    const page = await ctx.newPage();
    const s = t();
    await page.goto(`http://${NAME}.widgets.wtf:7788/?folder=/home/sprite/proj`);
    await page.locator(".monaco-workbench").waitFor({ timeout: 90000 });
    const wb = t() - s;
    await page.locator(".explorer-folders-view .monaco-list-row", { hasText: "main.rs" }).first().click({ timeout: 90000 });
    await page.locator(".monaco-editor .view-lines", { hasText: "hello from a sprite" }).first().waitFor({ timeout: 30000 });
    return { ctx, page, workbench_ms: wb, file_ms: t() - s };
  };
  let o = await open();
  out.first_open = { workbench_ms: o.workbench_ms, file_ms: o.file_ms };
  await sleep(5000);
  out.mem_with_client = inner("mem").trim();
  await o.ctx.close();
  o = await open();
  out.warm_open = { workbench_ms: o.workbench_ms, file_ms: o.file_ms };
  await o.ctx.close();
  // Leave it alone: does it suspend with code-server in it, and how long does a wake take?
  const idleStart = t();
  let st = await state();
  while (!["warm", "cold", "suspended"].includes(st) && t() - idleStart < 180000) {
    await sleep(2000);
    st = await state();
  }
  out.idle_to_suspend = { state: st, after_ms: t() - idleStart };
  o = await open();
  out.open_after_suspend = { workbench_ms: o.workbench_ms, file_ms: o.file_ms };
  await o.ctx.close();
  await browser.close();
} catch (e) {
  out.error = String(e).slice(0, 500);
} finally {
  await browser?.close().catch(() => {});
  try {
    sprite("destroy", "--force", NAME);
  } catch {
    try {
      execFileSync(path.join(here, "sprite.sh"), ["destroy", NAME], { input: "y\n" });
    } catch (e) {
      out.destroy_error = String(e).slice(0, 200);
    }
  }
}
console.log(JSON.stringify(out, null, 1));
