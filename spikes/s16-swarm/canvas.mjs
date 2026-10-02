// S16: frame cost of the prototype's Canvas 2D swarm at 500 / 2,000 / 5,000
// panes, in headless Chrome, at a laptop viewport and at a phone viewport
// with 4x CPU throttling (a proxy until a real phone run).
//
//   node spikes/s16-swarm/canvas.mjs > spikes/s16-swarm/work/canvas.json
//
// Uses the system Chrome through web/'s Playwright, as the e2e tests do.

import { createRequire } from "node:module";
import { fileURLToPath, pathToFileURL } from "node:url";
import path from "node:path";

const here = path.dirname(fileURLToPath(import.meta.url));
const require = createRequire(path.join(here, "../../web/package.json"));
const { chromium } = require("@playwright/test");

const page_url = pathToFileURL(path.join(here, "canvas.html")).href;
const SETTLE_MS = 6000;
const MEASURE_MS = 10000;

const profiles = [
  { name: "laptop", viewport: { width: 1440, height: 900 }, dpr: 2, throttle: 1 },
  { name: "phone-4x", viewport: { width: 390, height: 844 }, dpr: 3, throttle: 4 },
];

const pct = (xs, p) => {
  const s = [...xs].sort((a, b) => a - b);
  return s[Math.min(s.length - 1, Math.floor((p / 100) * s.length))];
};
const r1 = (x) => Math.round(x * 10) / 10;

const browser = await chromium.launch({ channel: "chrome", args: ["--enable-gpu-rasterization"] });
const out = [];
for (const prof of profiles) {
  for (const n of [500, 2000, 5000]) {
    const ctx = await browser.newContext({ viewport: prof.viewport, deviceScaleFactor: prof.dpr });
    const page = await ctx.newPage();
    const cdp = await ctx.newCDPSession(page);
    await cdp.send("Emulation.setCPUThrottlingRate", { rate: prof.throttle });
    await page.goto(`${page_url}?n=${n}`);
    await page.waitForTimeout(SETTLE_MS);
    await page.evaluate(() => { window.__s16.work = []; window.__s16.gaps = []; });
    await page.waitForTimeout(MEASURE_MS);
    const s = await page.evaluate(() => window.__s16);
    const step = s.work.map((w) => w[0]);
    const draw = s.work.map((w) => w[1]);
    const total = s.work.map((w) => w[0] + w[1]);
    const row = {
      profile: prof.name, panes: n, frames: s.gaps.length,
      fps: r1(s.gaps.length / (MEASURE_MS / 1000)),
      step_p50_ms: r1(pct(step, 50)), draw_p50_ms: r1(pct(draw, 50)),
      work_p50_ms: r1(pct(total, 50)), work_p95_ms: r1(pct(total, 95)),
      gap_p95_ms: r1(pct(s.gaps, 95)),
    };
    console.error(JSON.stringify(row));
    out.push(row);
    await ctx.close();
  }
}
await browser.close();
console.log(JSON.stringify(out, null, 1));
