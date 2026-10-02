// ghostty-web issue #198 on the emulated phones: does the renderer resize and
// fully repaint every frame while idle at a fractional devicePixelRatio?
import { chromium, webkit, devices } from "playwright";
import { serve } from "./server.mjs";

const server = await serve();
for (const [name, type, ctx] of [
  ["chromium desktop", chromium, { viewport: { width: 1280, height: 900 } }],
  ["chromium Pixel 7", chromium, devices["Pixel 7"]],
  ["webkit iPhone 15", webkit, devices["iPhone 15"]],
]) {
  const browser = await type.launch();
  const page = await (await browser.newContext(ctx)).newPage();
  await page.goto(`http://127.0.0.1:${server.address().port}/page.html`);
  await page.waitForFunction(() => window.S10_READY);
  const r = await page.evaluate(async () => {
    const { create } = await import("./engines.mjs");
    const t = await create("gw-next", document.getElementById("term"), { cols: 80, rows: 24 });
    await t.write(new TextEncoder().encode("idle\r\n"));
    await new Promise((r) => setTimeout(r, 500));
    const rd = t.t.renderer;
    let resizes = 0, renders = 0;
    const rs = rd.resize.bind(rd), rn = rd.render.bind(rd);
    rd.resize = (...a) => { resizes++; return rs(...a); };
    rd.render = (...a) => { renders++; return rn(...a); };
    await new Promise((r) => setTimeout(r, 1000));
    return { dpr: devicePixelRatio, coarse: matchMedia("(pointer: coarse)").matches, resizesPerSec: resizes, rendersPerSec: renders };
  });
  console.log(name.padEnd(18), JSON.stringify(r));
  await browser.close();
}
server.close();
