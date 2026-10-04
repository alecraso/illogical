// S17: what a follow-mode code view costs the page. Builds each entry with Vite (production,
// minified) into work/bundle/dist/<entry>, sums what it emits (JS, CSS, workers, fonts) raw,
// gzip and brotli, then loads each in headless Chrome and times first render and JS heap.
//
//   cd spikes/s17-editors/work/bundle && npm install   (bundle.sh does this)
//   node build.mjs > ../bundle.json

import { readdirSync, readFileSync, statSync, writeFileSync, mkdirSync } from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import { brotliCompressSync, gzipSync } from "node:zlib";
import { build } from "vite";

const here = process.cwd();
const entries = ["monaco-full", "monaco-min", "cm6", "shiki"];
const SAMPLE = readFileSync("/home/me/dev/jhgaylor/illogical/crates/daemon/src/browser.rs", "utf8").slice(0, 20000);

const walk = (d) => readdirSync(d).flatMap((f) => (statSync(path.join(d, f)).isDirectory() ? walk(path.join(d, f)) : [path.join(d, f)]));
const out = {};
for (const e of entries) {
  const root = path.join(here, "dist", e);
  mkdirSync(root, { recursive: true });
  const html = `<!doctype html><html><head><meta charset="utf-8"></head><body style="margin:0;background:#1e1e2e">
<div id="code" style="width:1200px;height:800px"></div>
<script>window.SAMPLE = ${JSON.stringify(SAMPLE)};</script>
<script type="module" src="/${e}.js"></script></body></html>`;
  writeFileSync(path.join(here, `${e}.html`), html);
  await build({
    root: here,
    logLevel: "warn",
    build: { outDir: root, emptyOutDir: true, rollupOptions: { input: path.join(here, `${e}.html`) }, chunkSizeWarningLimit: 100000 },
  });
  const files = walk(root).filter((f) => !f.endsWith(".html"));
  const sum = { files: files.length, raw: 0, gzip: 0, brotli: 0, by_ext: {} };
  for (const f of files) {
    const b = readFileSync(f);
    sum.raw += b.length;
    sum.gzip += gzipSync(b, { level: 9 }).length;
    sum.brotli += brotliCompressSync(b).length;
    const ext = path.extname(f);
    sum.by_ext[ext] = (sum.by_ext[ext] ?? 0) + b.length;
  }
  for (const k of ["raw", "gzip", "brotli"]) sum[k + "_kb"] = Math.round(sum[k] / 1024);
  out[e] = sum;
  console.error(e, sum.raw_kb, sum.gzip_kb, sum.brotli_kb);
}

// First render and heap, served by vite preview-like static server.
const require = createRequire("/home/me/dev/jhgaylor/illogical/web/package.json");
const { chromium } = require("@playwright/test");
const http = await import("node:http");
const browser = await chromium.launch({ channel: "chrome" });
for (const e of entries) {
  const root = path.join(here, "dist", e);
  const server = http.createServer((req, res) => {
    const p = path.join(root, decodeURIComponent(req.url.split("?")[0]) === "/" ? `${e}.html` : decodeURIComponent(req.url.split("?")[0]));
    try {
      const b = readFileSync(p);
      const type = p.endsWith(".js") ? "text/javascript" : p.endsWith(".css") ? "text/css" : p.endsWith(".html") ? "text/html" : p.endsWith(".ttf") ? "font/ttf" : "application/octet-stream";
      res.writeHead(200, { "content-type": type });
      res.end(b);
    } catch {
      res.writeHead(404);
      res.end();
    }
  });
  await new Promise((r) => server.listen(0, "127.0.0.1", r));
  const port = server.address().port;
  const times = [];
  let heap = 0;
  for (let i = 0; i < 5; i++) {
    const ctx = await browser.newContext({ viewport: { width: 1280, height: 900 } });
    const page = await ctx.newPage();
    const t0 = Date.now();
    await page.goto(`http://127.0.0.1:${port}/`);
    await page.waitForFunction(() => window.view && document.querySelector("#code").innerText.replace(/\s/g, " ").includes("Browser blocks") /* Monaco draws spaces as nbsp */, null, { timeout: 30000 });
    times.push(Date.now() - t0);
    const cdp = await ctx.newCDPSession(page);
    await cdp.send("HeapProfiler.collectGarbage");
    heap = (await cdp.send("Runtime.getHeapUsage")).usedSize;
    await ctx.close();
  }
  server.close();
  times.sort((a, b) => a - b);
  out[e].render_ms_median = times[2];
  out[e].render_ms_all = times;
  out[e].heap_mb = Math.round((heap / 1048576) * 10) / 10;
}
await browser.close();
console.log(JSON.stringify(out, null, 1));
