// Run bench.html in a real browser through Playwright.
//   PLAYWRIGHT_DIR=/path/with/node_modules node run-browser.mjs [chrome|firefox|webkit] [variant]
import { createServer } from "node:http";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
const dir = process.env.PLAYWRIGHT_DIR || new URL("../../../web/", import.meta.url).pathname;
const require = createRequire(dir + "/package.json");
let pw;
try { pw = require("playwright-core"); } catch { pw = require("@playwright/test"); }
const [engine = "chrome", variant = "wasm-rustcrypto", sizes = "2,10,50", reps = "9"] = process.argv.slice(2);
const types = { ".js": "text/javascript", ".wasm": "application/wasm", ".html": "text/html" };
const srv = createServer((req, res) => {
  const p = new URL(req.url, "http://x").pathname;
  try {
    const body = readFileSync(new URL("." + p, import.meta.url));
    res.writeHead(200, { "content-type": types[p.slice(p.lastIndexOf("."))] || "application/octet-stream" });
    res.end(body);
  } catch { res.writeHead(404); res.end(); }
}).listen(7830, "127.0.0.1");
const browser = engine === "chrome" ? await pw.chromium.launch({ channel: "chrome" }) : await pw[engine].launch();
const page = await browser.newPage();
await page.goto(`http://127.0.0.1:7830/bench.html?variant=${variant}&sizes=${sizes}&reps=${reps}`);
await page.waitForFunction(() => window.s30, null, { timeout: 300000 });
const r = await page.evaluate(() => window.s30);
console.log(`# ${engine} ${variant}: load ${r.load_ms.toFixed(1)} ms, demo ok: ${r.demo_ok}\n# ${r.ua}`);
console.log("| N | operation | median ms | min ms | bytes |\n|---|---|---|---|---|");
for (const x of r.rows) console.log(`| ${x.n} | ${x.op} | ${x.median_ms.toFixed(2)} | ${x.min_ms.toFixed(2)} | ${x.bytes ?? ""} |`);
if (process.env.OUT) (await import("node:fs")).writeFileSync(process.env.OUT, JSON.stringify(r, null, 1));
await browser.close(); srv.close();
