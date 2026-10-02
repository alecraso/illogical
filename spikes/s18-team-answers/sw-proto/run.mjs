// S18 Q3 runner: serve work/sw-dist, start the Rust responder (a daemon
// stand-in), open headless Chrome, and deliver pushes to the service worker
// through the DevTools protocol, cold (worker stopped first) and warm.
// From web/: node ../spikes/s18-team-answers/sw-proto/run.mjs
import { spawn } from "node:child_process";
import { createServer } from "node:http";
import { readFileSync, existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { createRequire } from "node:module";

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, "..", "..", "..");
const dist = join(here, "..", "work", "sw-dist");
const pw = await import(pathToFileURL(createRequire(join(root, "web", "package.json")).resolve("@playwright/test")).href);
const chromium = pw.chromium ?? pw.default.chromium;
const PORT = 7765;
const N = Number(process.env.N ?? 5);

const types = { ".js": "text/javascript", ".html": "text/html", ".json": "application/json" };
const server = createServer((req, res) => {
  const p = join(dist, req.url === "/" ? "index.html" : req.url.split("?")[0]);
  if (!existsSync(p)) return void res.writeHead(404).end();
  res.writeHead(200, { "content-type": types[p.slice(p.lastIndexOf("."))] ?? "application/octet-stream" }).end(readFileSync(p));
}).listen(PORT, "127.0.0.1");

const responder = spawn(join(root, "target", "debug", "examples", "interop"), ["responder", "127.0.0.1:0"]);
const [addr, id, noise] = await new Promise((r) => responder.stdout.once("data", (d) => r(d.toString().trim().split(" "))));

const browser = await chromium.launch({ channel: "chrome", headless: true });
const ctx = await browser.newContext();
await ctx.grantPermissions(["notifications"], { origin: `http://127.0.0.1:${PORT}` });
const page = await ctx.newPage();
await page.goto(`http://127.0.0.1:${PORT}/`);
await page.evaluate(() => window.s18.ready);

const cdp = await ctx.newCDPSession(page);
const regs = new Map();
cdp.on("ServiceWorker.workerRegistrationUpdated", (e) => e.registrations.forEach((r) => regs.set(r.scopeURL, r.registrationId)));
const versions = [];
cdp.on("ServiceWorker.workerVersionUpdated", (e) => versions.push(...e.versions));
await cdp.send("ServiceWorker.enable");
for (let i = 0; i < 50 && !regs.size; i++) await new Promise((r) => setTimeout(r, 100));
const regId = [...regs.values()][0];
const origin = `http://127.0.0.1:${PORT}`;
const data = (kind) => JSON.stringify({ kind, url: `ws://${addr}/`, daemon: { id, noise }, pane: 5, approve: "perm-1" });

async function once(kind) {
  if (kind === "cold") {
    await cdp.send("ServiceWorker.stopAllWorkers");
    await new Promise((r) => setTimeout(r, 300));
  }
  const before = await page.evaluate(() => window.s18.results.length);
  const t0 = Date.now();
  await cdp.send("ServiceWorker.deliverPushMessage", { origin, registrationId: regId, data: data(kind) });
  for (let i = 0; i < 100; i++) {
    const n = await page.evaluate(() => window.s18.results.length);
    if (n > before) break;
    await new Promise((r) => setTimeout(r, 20));
  }
  const r = await page.evaluate(() => window.s18.results.at(-1));
  return { ...r, wallMs: Date.now() - t0 };
}

const out = [];
for (let i = 0; i < N; i++) out.push(await once("cold"));
for (let i = 0; i < N; i++) out.push(await once("warm"));
for (const r of out) console.log(JSON.stringify(r));
const med = (k, kind) => {
  const v = out.filter((r) => r.kind === kind && typeof r[k] === "number").map((r) => r[k]).sort((a, b) => a - b);
  return v.length ? v[Math.floor(v.length / 2)] : null;
};
for (const kind of ["cold", "warm"])
  console.log(kind, "median:", ["keysMs", "chainMs", "channelMs", "answerMs", "wallMs"].map((k) => `${k}=${med(k, kind)}`).join(" "));
console.log("chrome", browser.version());
await browser.close();
responder.kill();
server.close();
