// S16: one Chrome page holding summary connections to many daemons.
//   node fleet.mjs <page base url> <ports,comma,separated>
// Prints JSON: page memory before and after connecting, and how long a
// wake (every socket reconnecting at once) takes. fleet.py runs it.
import { createRequire } from "node:module";
import { readFileSync, readdirSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const require = createRequire(path.join(here, "../../web/package.json"));
const { chromium } = require("@playwright/test");
const [base, portList] = process.argv.slice(2);

// RSS of every process under this one (Chrome's), and of its renderers.
function treeRss() {
  const kids = new Map();
  for (const d of readdirSync("/proc")) {
    if (!/^\d+$/.test(d)) continue;
    try {
      const s = readFileSync(`/proc/${d}/stat`, "utf8");
      const ppid = Number(s.slice(s.lastIndexOf(")") + 2).split(" ")[1]);
      if (!kids.has(ppid)) kids.set(ppid, []);
      kids.get(ppid).push(Number(d));
    } catch {}
  }
  const out = { total: 0, renderer: 0 };
  const todo = [process.pid];
  while (todo.length) {
    for (const c of kids.get(todo.pop()) ?? []) {
      todo.push(c);
      try {
        const rss = Number(readFileSync(`/proc/${c}/status`, "utf8").match(/VmRSS:\s+(\d+)/)?.[1] ?? 0);
        const cmd = readFileSync(`/proc/${c}/cmdline`, "utf8");
        out.total += rss;
        if (cmd.includes("--type=renderer")) out.renderer += rss;
      } catch {}
    }
  }
  return { total_mib: Math.round(out.total / 1024), renderer_mib: Math.round(out.renderer / 1024) };
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const browser = await chromium.launch({ channel: "chrome" });
const page = await browser.newPage();
const cdp = await page.context().newCDPSession(page);
await cdp.send("Performance.enable");
const heap = async () => {
  await cdp.send("HeapProfiler.collectGarbage");
  const m = (await cdp.send("Performance.getMetrics")).metrics;
  return Math.round((m.find((x) => x.name === "JSHeapUsedSize").value / 1024 / 1024) * 10) / 10;
};
await page.goto(`${base}/fleet.html?ports=${portList}`);
await sleep(1500);
const before = { ...treeRss(), heap_mib: await heap() };
const t0 = Date.now();
await page.evaluate(() => window.__start());
await page.waitForFunction(() => window.__fleet.conns.every((c) => c.hellos >= 1), null, { timeout: 60000 });
const firstConnectMs = Date.now() - t0;
await sleep(3000);
const after = { ...treeRss(), heap_mib: await heap(), panes: await page.evaluate(() => window.__panes()) };
// Sleep and wake, three times.
const wakes = [];
for (let i = 0; i < 3; i++) {
  await page.evaluate(() => window.__sleep());
  await sleep(2000);
  const w = await page.evaluate(async () => {
    const want = window.__fleet.conns.map((c) => c.hellos + 1);
    const bytes0 = window.__fleet.conns.reduce((n, c) => n + c.bytes, 0);
    window.__wake();
    const t = performance.now();
    while (!window.__fleet.conns.every((c, i) => c.hellos >= want[i])) {
      await new Promise((r) => setTimeout(r, 5));
      if (performance.now() - t > 30000) break;
    }
    const conns = window.__fleet.conns;
    return {
      all_ms: Math.round(performance.now() - t),
      slowest_ms: Math.round(Math.max(...conns.map((c) => c.helloAt - window.__wakeAt))),
      hello_bytes: conns.reduce((n, c) => n + c.bytes, 0) - bytes0,
      failures: conns.reduce((n, c) => n + c.failures, 0),
      each_ms: conns.map((c) => Math.round(c.helloAt - window.__wakeAt)).sort((a, b) => a - b),
    };
  });
  wakes.push(w);
  await sleep(1000);
}
console.log(JSON.stringify({ connections: portList.split(",").length, first_connect_ms: firstConnectMs, before, after, wakes }));
await browser.close();
