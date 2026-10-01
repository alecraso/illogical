// HMR through the bridge: load the page, edit CSS and a self-accepting JS
// module inside the sprite, and check the page updates without reloading.
// usage: node hmr.mjs <url> [sprite] [app-dir]
//   env BROWSER=chromium|webkit, DEVICE="iPhone 15" etc, FRAME=1 to load the
//   dev server inside a sandboxed iframe on a wrapper page (FRAME_HOST serves it).
import { chromium, webkit, devices } from "../../web/node_modules/@playwright/test/index.mjs";
import { writeFile } from "./sprite.mjs";

const [url, sprite = "illogical-s6-vite", dir = "/home/sprite/app"] = process.argv.slice(2);
const t0 = Date.now();
const log = (...a) => console.log(`[${Date.now() - t0}ms]`, ...a);
const sleep = (n) => new Promise((r) => setTimeout(r, n));
const tag = `${Date.now() % 100000}`;

const mainJs = (word) => `import './style.css'
document.querySelector('#app').innerHTML = \`
  <h1 id="title">${word}</h1>
  <input id="field" placeholder="type here" />
  <button id="btn" type="button">tap</button>
  <p id="taps">0</p>
  <div style="height:3000px;background:linear-gradient(#fff,#88f)">tall</div>
\`
let taps = 0
document.querySelector('#btn').addEventListener('click', () => {
  document.querySelector('#taps').textContent = String(++taps)
})
if (import.meta.hot) import.meta.hot.accept()
`;

const browserType = process.env.BROWSER === "webkit" ? webkit : chromium;
const launch = process.env.BROWSER === "webkit" ? {} : { channel: "chrome" };
const browser = await browserType.launch(launch);
const ctxOpts = process.env.DEVICE ? { ...devices[process.env.DEVICE], ignoreHTTPSErrors: false } : {};
if (process.env.DEVICE && process.env.BROWSER !== "webkit") delete ctxOpts.defaultBrowserType;
const ctx = await browser.newContext(ctxOpts);
const page = await ctx.newPage();
const consoleLines = [];
page.on("console", (m) => consoleLines.push(m.text()));
page.on("pageerror", (e) => consoleLines.push(`pageerror: ${e.message}`));
const wsUrls = [];
page.on("websocket", (ws) => wsUrls.push(ws.url()));

const result = { url, browser: process.env.BROWSER ?? "chromium", device: process.env.DEVICE ?? null, frame: !!process.env.FRAME };
try {
  // Reset the entry module to a known self-accepting version.
  await writeFile(sprite, `${dir}/src/main.js`, mainJs(`start-${tag}`));
  await writeFile(sprite, `${dir}/src/style.css`, `body { margin: 0; font: 16px sans-serif }\n`);
  await sleep(500);

  let target; // a Frame
  if (process.env.FRAME) {
    const wrapper = `${process.env.FRAME_HOST}/?src=${encodeURIComponent(url)}`;
    await page.goto(wrapper);
    const el = await page.waitForSelector("iframe");
    target = await el.contentFrame();
    result.wrapper = wrapper;
  } else {
    await page.goto(url);
    target = page.mainFrame();
  }
  await target.waitForSelector("#title", { timeout: 20000 });
  result.loaded_ms = Date.now() - t0;
  result.origin = await target.evaluate(() => self.origin);
  result.storage = await target.evaluate(() => {
    const out = {};
    try { localStorage.setItem("k", "v"); out.localStorage = "ok"; } catch (e) { out.localStorage = e.name; }
    try { document.cookie = "c=1"; out.cookie = document.cookie.includes("c=1") ? "ok" : "ignored"; } catch (e) { out.cookie = e.name; }
    out.serviceWorker = "serviceWorker" in navigator ? "present" : "absent";
    try { out.parentAccess = String(window.parent.document.title); } catch (e) { out.parentAccess = e.name; }
    return out;
  });
  await target.evaluate(() => { window.__marker = "kept"; });
  await sleep(1500); // let the HMR socket connect

  // CSS update
  let t = Date.now();
  await writeFile(sprite, `${dir}/src/style.css`, `body { margin: 0; font: 16px sans-serif }\nh1 { color: rgb(1, 2, 3) }\n`);
  await target.waitForFunction(() => getComputedStyle(document.querySelector("#title")).color === "rgb(1, 2, 3)", null, { timeout: 15000 })
    .then(() => { result.css_hmr_ms = Date.now() - t; }, () => { result.css_hmr_ms = "timeout"; });
  result.css_marker = await target.evaluate(() => window.__marker ?? "lost");

  // JS update (self-accepting module)
  t = Date.now();
  await writeFile(sprite, `${dir}/src/main.js`, mainJs(`edited-${tag}`));
  await target.waitForFunction((w) => document.querySelector("#title")?.textContent === w, `edited-${tag}`, { timeout: 15000 })
    .then(() => { result.js_hmr_ms = Date.now() - t; }, () => { result.js_hmr_ms = "timeout"; });
  result.js_marker = await target.evaluate(() => window.__marker ?? "lost");

  // Interaction: tap, type, scroll
  const isTouch = !!ctxOpts.hasTouch;
  const btn = await target.$("#btn");
  if (isTouch) await btn.tap(); else await btn.click();
  if (isTouch) await btn.tap(); else await btn.click();
  result.taps = await target.$eval("#taps", (e) => e.textContent);
  await (await target.$("#field")).tap?.().catch(() => {});
  await target.click("#field");
  await page.keyboard.type("héllo 123");
  result.typed = await target.$eval("#field", (e) => e.value);
  result.focused_in_frame = await target.evaluate(() => document.activeElement?.id);
  // Scroll: a real wheel over the frame where Playwright supports it (not in
  // mobile WebKit), else a programmatic scroll (proves only that it scrolls).
  try {
    const box = process.env.FRAME ? await (await page.$("iframe")).boundingBox() : { x: 0, y: 0, width: 300, height: 300 };
    await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
    await page.mouse.wheel(0, 800);
    result.scroll_method = "wheel";
  } catch {
    await target.evaluate(() => window.scrollBy(0, 800));
    result.scroll_method = "scrollBy";
  }
  await sleep(500);
  result.scrollY = await target.evaluate(() => window.scrollY);
  result.viewport = await target.evaluate(() => `${innerWidth}x${innerHeight}@${devicePixelRatio}`);
} catch (e) {
  result.error = String(e).slice(0, 400);
}
if (process.env.SHOT) await page.screenshot({ path: process.env.SHOT }).catch(() => {});
result.websockets = wsUrls;
result.vite_console = consoleLines.filter((l) => /vite|error|refused|blocked/i.test(l)).slice(0, 12);
console.log(JSON.stringify(result, null, 1));
await browser.close();
