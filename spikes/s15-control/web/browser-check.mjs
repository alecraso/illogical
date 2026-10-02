// Load keys.html in a real browser twice (key survives reload?) and run
// the Noise handshake against the spike daemon from inside the page.
//   node browser-check.mjs PEER_HEX [chrome|firefox|webkit]
import { createServer } from "node:http";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
const require = createRequire(new URL("../../../web/package.json", import.meta.url));
const pw = require("@playwright/test");
const [peer, engine = "chrome"] = process.argv.slice(2);
const srv = createServer((req, res) => {
  const p = new URL(req.url, "http://x").pathname;
  try {
    const body = readFileSync(new URL("." + p, import.meta.url));
    res.writeHead(200, { "content-type": p.endsWith(".js") ? "text/javascript" : "text/html" });
    res.end(body);
  } catch { res.writeHead(404); res.end(); }
}).listen(7803, "127.0.0.1");
const browser = engine === "chrome" ? await pw.chromium.launch({ channel: "chrome" }) : await pw[engine].launch();
const page = await (await browser.newContext()).newPage();
const url = `http://127.0.0.1:7803/keys.html?daemon=ws://127.0.0.1:7802/&peer=${peer}`;
for (const load of ["first load", "reload"]) {
  await page.goto(url);
  await page.waitForFunction(() => window.s15?.done, null, { timeout: 15000 });
  const r = await page.evaluate(() => window.s15);
  delete r.userAgent;
  console.log(`[${engine} ${load}]`, JSON.stringify(r, null, 1));
}
await browser.close(); srv.close();
