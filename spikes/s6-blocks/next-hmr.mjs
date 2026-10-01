// Fast Refresh through the bridge for Next.js: edit app/page.js and
// app/globals.css inside the sprite; check the page updates, client state
// survives (no full reload), and a counter's state is kept.
// usage: node next-hmr.mjs <url>   env FRAME=1 FRAME_HOST=... as hmr.mjs
import { chromium } from "../../web/node_modules/@playwright/test/index.mjs";
import { writeFile } from "./sprite.mjs";
const [url, sprite = "illogical-s6-vite", dir = "/home/sprite/nextapp"] = process.argv.slice(2);
const sleep = (n) => new Promise((r) => setTimeout(r, n));
const tag = `${Date.now() % 100000}`;
const page_js = (word) => `'use client'
import { useState } from 'react'
export default function Page() {
  const [n, setN] = useState(0)
  return <main><h1 id="title">${word}</h1><button id="btn" onClick={() => setN(n + 1)}>tap</button><p id="n">{n}</p></main>
}
`;
const browser = await chromium.launch({ channel: "chrome" });
const page = await browser.newPage();
const cons = []; page.on("console", (m) => cons.push(m.text().slice(0, 300)));
const ws = []; page.on("websocket", (w) => ws.push(w.url()));
const r = { url, frame: !!process.env.FRAME };
try {
  await writeFile(sprite, `${dir}/app/page.js`, page_js(`start-${tag}`));
  await writeFile(sprite, `${dir}/app/globals.css`, `body { margin: 0 }\n`);
  await sleep(1000);
  let t;
  if (process.env.FRAME) {
    await page.goto(`${process.env.FRAME_HOST}/?src=${encodeURIComponent(url)}`);
    t = await (await page.waitForSelector("iframe")).contentFrame();
  } else { await page.goto(url); t = page.mainFrame(); }
  await t.waitForSelector("#title", { timeout: 60000 });
  await t.waitForFunction(() => document.querySelector("#title")?.textContent?.startsWith("start-"), null, { timeout: 30000 });
  await sleep(1500); // hydration + HMR socket
  await t.click("#btn"); await t.click("#btn");
  r.count_before = await t.$eval("#n", (e) => e.textContent);
  await t.evaluate(() => { window.__marker = "kept"; });
  let t0 = Date.now();
  await writeFile(sprite, `${dir}/app/page.js`, page_js(`edited-${tag}`));
  await t.waitForFunction((w) => document.querySelector("#title")?.textContent === w, `edited-${tag}`, { timeout: 30000 })
    .then(() => { r.js_refresh_ms = Date.now() - t0; }, () => { r.js_refresh_ms = "timeout"; });
  r.marker_after_js = await t.evaluate(() => window.__marker ?? "lost");
  r.count_after = await t.$eval("#n", (e) => e.textContent).catch(() => "?");
  t0 = Date.now();
  await writeFile(sprite, `${dir}/app/globals.css`, `body { margin: 0 }\nh1 { color: rgb(1, 2, 3) }\n`);
  await t.waitForFunction(() => getComputedStyle(document.querySelector("#title")).color === "rgb(1, 2, 3)", null, { timeout: 30000 })
    .then(() => { r.css_ms = Date.now() - t0; }, () => { r.css_ms = "timeout"; });
  r.marker_after_css = await t.evaluate(() => window.__marker ?? "lost");
} catch (e) { r.error = String(e).slice(0, 400); }
r.websockets = ws;
r.console = cons.filter((l) => !/Download the React DevTools|GL Driver/.test(l)).slice(0, 10);
console.log(JSON.stringify(r, null, 1));
await browser.close();
