// The README's site/img/dive.gif: the project page's tour, one full cycle,
// frame by frame on Playwright's fake clock. From web/:
//
//   node screenshots/dive.mjs /tmp/dive-frames
//   gifski --fps 9 --width 880 --quality 70 -o ../site/img/dive.gif /tmp/dive-frames/f*.png
//
// (gifski: `cargo install gifski`; it keeps the GIF near 1.5 MB.)

import { mkdirSync } from "node:fs";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { chromium } from "@playwright/test";

const out = process.argv[2] ?? "dive-frames";
/** 16 ms ticks per frame. */
const K = Number(process.argv[3] ?? 3);
mkdirSync(out, { recursive: true });

const b = await chromium.launch({ channel: "chrome" });
const page = await b.newPage({ viewport: { width: 1400, height: 877 }, deviceScaleFactor: 1 });
await page.clock.install();
await page.goto(pathToFileURL(join(import.meta.dirname, "../../site/index.html")).href);
await page.evaluate(() => document.fonts.ready);
const stage = page.locator("#stage");
await stage.scrollIntoViewIfNeeded();
const step = () =>
  page.evaluate(() => [...document.querySelectorAll(".tour button")].findIndex((b) => b.getAttribute("aria-pressed") === "true"));

// Through one tour, to the start of the next, so the GIF loops cleanly.
const seen = new Set();
for (let t = 0; t < 120_000; t += 16) {
  await page.clock.runFor(16);
  const s = await step();
  seen.add(s);
  if (s === 0 && seen.has(3)) break;
}
const steps = [];
let n = 0;
for (let i = 0; i < 20_000; i++) {
  const s = await step();
  if (i > 100 && s === 0 && steps.includes(3)) break;
  if (i % K === 0) {
    steps.push(s);
    await stage.screenshot({ path: join(out, `f${String(n++).padStart(4, "0")}.png`) });
  }
  await page.clock.runFor(16);
}
console.log(`${n} frames: ${steps.join("")}`);
await b.close();
