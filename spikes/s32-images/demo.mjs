// S32 demo: a real image paste in Chrome lands in a `claude` pane as
// [Image #N]. Needs this branch's illogicald on 127.0.0.1:7692 and `claude`
// waiting at its prompt in pane PANE (argv[2], default 2).
// `node spikes/s32-images/demo.mjs 2` from the repo root.
import { createRequire } from "node:module";
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
const require = createRequire(new URL("../../web/package.json", import.meta.url));
const { chromium } = require("@playwright/test");
const pane = Number(process.argv[2] ?? 2);
const capture = () =>
  execFileSync("illogical", ["capture", `%${pane}`], { env: { ...process.env, ILLOGICAL_SOCK: "/tmp/il32/sock" } }).toString();
const browser = await chromium.launch({ channel: "chrome" });
const ctx = await browser.newContext({ permissions: ["clipboard-read", "clipboard-write"] });
const page = await ctx.newPage();
page.on("console", (m) => m.type() !== "debug" && console.log("  page:", m.text()));
// The local token signs this browser in (macOS has no loopback owner check).
const token = readFileSync("/tmp/il32/local-token", "utf8").trim();
await page.goto(`http://127.0.0.1:7692/auth?token=${token}`);
await page.goto("http://127.0.0.1:7692/");
await page.waitForFunction(() => window.__illogical?.client.connected && window.__illogical.client.state);
// Show the pane's tab: the client makes views only for the tab it shows.
await page.evaluate((p) => {
  const c = window.__illogical.client;
  const tab = c.state.tabs.find((t) => JSON.stringify(t.layout ?? t).includes(`"pane":${p}`) || (t.panes ?? []).includes(p));
  if (tab) c.selectTab(tab.id);
}, pane);
await page.waitForFunction((p) => window.__illogical.client.panes.has(p), pane);
await page.evaluate(async (p) => {
  window.__illogical.client.panes.get(p).view.host.querySelector(".xterm-helper-textarea").focus();
  const c = document.createElement("canvas"); c.width = 320; c.height = 200;
  const g = c.getContext("2d"); g.fillStyle = "#1d6b2f"; g.fillRect(0, 0, 320, 200);
  g.fillStyle = "white"; g.font = "bold 40px sans-serif"; g.fillText("S32 GREEN", 40, 115);
  const blob = await new Promise((r) => c.toBlob(r, "image/png"));
  await navigator.clipboard.write([new ClipboardItem({ "image/png": blob })]);
}, pane);
const before = capture().match(/\[Image #\d+\]/g)?.length ?? 0;
const t0 = Date.now();
await page.keyboard.press("ControlOrMeta+V");
let line = "";
for (let i = 0; i < 50; i++) {
  line = capture().split("\n").filter((l) => l.startsWith("❯")).pop() ?? "";
  if ((line.match(/\[Image #\d+\]/g)?.length ?? 0) > 0 && capture().match(/\[Image #\d+\]/g).length > before) break;
  await new Promise((r) => setTimeout(r, 100));
}
console.log(`prompt after paste (${Date.now() - t0} ms): ${line}`);
await browser.close();
