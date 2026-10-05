// S32: run with `node spikes/s32-images/paste.mjs [chrome|webkit|firefox]` from the repo root.
import { createRequire } from "node:module";
import { pathToFileURL } from "node:url";
const require = createRequire(new URL("../../web/package.json", import.meta.url));
const pw = require("@playwright/test");
const which = process.argv[2] ?? "chrome";
const type = which === "chrome" ? pw.chromium : pw[which];
const browser = await type.launch(which === "chrome" ? { channel: "chrome" } : {});
const ctx = await browser.newContext(which === "chrome" ? { permissions: ["clipboard-read", "clipboard-write"] } : {});
const page = await ctx.newPage();
await page.goto(pathToFileURL(new URL("paste.html", import.meta.url).pathname).href);
await page.locator(".xterm-helper-textarea").focus();
const out = { browser: which, version: browser.version() };

// 1. A real paste of an image from the (browser's) clipboard.
out.clipboardWrite = await page.evaluate(async () => {
  const c = document.createElement("canvas"); c.width = 8; c.height = 8;
  const g = c.getContext("2d"); g.fillStyle = "red"; g.fillRect(0, 0, 8, 8);
  const blob = await new Promise((r) => c.toBlob(r, "image/png"));
  try { await navigator.clipboard.write([new ClipboardItem({ "image/png": blob })]); return "ok"; }
  catch (e) { return String(e); }
});
await page.keyboard.press("ControlOrMeta+V");
await page.waitForTimeout(300);
out.afterImagePaste = await page.evaluate(() => log.splice(0));

// 2. Text still goes to xterm (the capture listener only takes files).
await page.evaluate(() => navigator.clipboard.writeText("echo hi").catch(() => {}));
await page.keyboard.press("ControlOrMeta+V");
await page.waitForTimeout(300);
out.afterTextPaste = await page.evaluate(() => log.splice(0));

// 3. A drop with a File (synthetic: Playwright can't drag from the OS).
out.afterDrop = await page.evaluate(() => {
  const dt = new DataTransfer();
  dt.items.add(new File([new Uint8Array([137, 80, 78, 71])], "shot.png", { type: "image/png" }));
  document.querySelector(".xterm-screen").dispatchEvent(new DragEvent("drop", { dataTransfer: dt, bubbles: true, cancelable: true }));
  return log.splice(0);
});
console.log(JSON.stringify(out, null, 1));
await browser.close();
