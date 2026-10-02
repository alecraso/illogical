// Q3 + Q4: attach time. A fresh page per run fetches a snapshot, writes it
// into a fresh terminal (as Client.onFrame does) and times it. GHOSTSNP runs
// decode the same state with upstream libghostty-vt wasm (no renderer).
//   node attach.mjs [repeats]
import { chromium, webkit, devices } from "playwright";
import { readFileSync, writeFileSync } from "node:fs";
import { gzipSync } from "node:zlib";
import { serve } from "./server.mjs";

const here = new URL(".", import.meta.url).pathname;
const REPEATS = Number(process.argv[2] ?? 3);
const CASES = ["attach_small", "attach_10k", "attach_64k"];
const PHONE_NET = { offline: false, latency: 50, downloadThroughput: 10e6 / 8, uploadThroughput: 10e6 / 8 };
const CONFIGS = [
  { name: "desktop chromium", type: chromium, ctx: { viewport: { width: 1280, height: 900 } } },
  { name: "Pixel 7, CPU 4x", type: chromium, ctx: devices["Pixel 7"], cpu: 4 },
  { name: "Pixel 7, CPU 4x, 10Mbps/50ms", type: chromium, ctx: devices["Pixel 7"], cpu: 4, net: PHONE_NET },
  { name: "webkit iPhone 15 (no throttling)", type: webkit, ctx: devices["iPhone 15"] },
];
const RUNS = [
  ["xterm", { scrollback: 10000 }, "xterm, sb 10k lines (client today)"],
  ["xterm", { scrollback: 100000 }, "xterm, sb 100k lines"],
  ["xterm", { scrollback: 10000, gzip: true }, "xterm, sb 10k, gzip on the wire"],
  ["gw-next", { scrollback: 64 * 1024 * 1024 }, "gw next, sb 64MiB"],
];
const med = (xs) => [...xs].sort((a, b) => a - b)[Math.floor(xs.length / 2)];

// Compressed variants of the wire snapshot (level 6, like permessage-deflate defaults).
for (const c of CASES) writeFileSync(`${here}work/data/${c}.vt.gz`, gzipSync(readFileSync(`${here}work/data/${c}.vt`), { level: 6 }));

const server = await serve();
const base = `http://127.0.0.1:${server.address().port}/page.html`;
const results = [];

async function inPage(cfg, fn) {
  const browser = await cfg.type.launch();
  const ctx = await browser.newContext(cfg.ctx);
  const page = await ctx.newPage();
  await page.goto(base);
  await page.waitForFunction(() => window.S10_READY);
  if (cfg.cpu || cfg.net) {
    const cdp = await ctx.newCDPSession(page);
    if (cfg.cpu) await cdp.send("Emulation.setCPUThrottlingRate", { rate: cfg.cpu });
    if (cfg.net) {
      await cdp.send("Network.enable");
      await cdp.send("Network.emulateNetworkConditions", cfg.net);
    }
  }
  try {
    return await fn(page);
  } finally {
    await browser.close();
  }
}

for (const cfg of CONFIGS) {
  for (const name of CASES) {
    for (const [engine, opts, label] of process.env.GHOSTSNP_ONLY ? [] : RUNS) {
      const rs = [];
      for (let i = 0; i < REPEATS; i++) {
        rs.push(await inPage(cfg, (p) => p.evaluate(([e, n, o]) => S10.attach(e, n, o), [engine, name, opts])));
      }
      const r = { config: cfg.name, case: name, engine: label, bytes: rs[0].bytes, scrollbackRows: rs[0].scrollbackRows,
        lastLine: rs[0].lastLine };
      for (const k of ["net", "parsed", "drawn", "total", "longestBlock"]) r[k] = med(rs.map((x) => x[k]));
      results.push(r);
      console.log(`${cfg.name.padEnd(30)} ${name.padEnd(13)} ${label.padEnd(36)} ${String(r.bytes).padStart(8)} B  net ${r.net.toFixed(0).padStart(5)}  parsed ${r.parsed.toFixed(0).padStart(5)}  drawn ${r.drawn.toFixed(0).padStart(5)}  total ${r.total.toFixed(0).padStart(5)}  block ${r.longestBlock.toFixed(0).padStart(5)} ms  sb ${r.scrollbackRows}`);
    }
    for (const optimize of ["small", "fast"]) {
      const rs = [];
      for (let i = 0; i < REPEATS; i++) rs.push(await inPage(cfg, (p) => p.evaluate(([n, o]) => S10.ghostsnp(n, { optimize: o }), [name, optimize])));
      const r = { config: cfg.name, case: name, engine: `GHOSTSNP wasm ${optimize}`, bytes: rs[0].bytes, historyRows: rs[0].historyRows,
        scrollbackRows: rs[0].scrollbackRows, rowsAtReady: rs[0].rowsAtReady, plainMatches: rs.every((x) => x.plainMatches),
        screenAtReadyMatches: rs.every((x) => x.screenAtReadyMatches), pages: rs[0].pages };
      for (const k of ["net", "copied", "ready", "history", "longestPage", "done"]) r[k] = med(rs.map((x) => x[k]));
      results.push(r);
      console.log(`${cfg.name.padEnd(30)} ${name.padEnd(13)} ${r.engine.padEnd(36)} ${String(r.bytes).padStart(8)} B  net ${r.net.toFixed(0).padStart(5)}  READY ${r.ready.toFixed(2)}  history ${r.history.toFixed(1)} (${r.pages} pages, longest ${r.longestPage.toFixed(2)})  sb ${r.scrollbackRows} plain ${r.plainMatches} screen@READY ${r.screenAtReadyMatches}`);
    }
  }
}
server.close();
writeFileSync(`${here}work/attach${process.env.GHOSTSNP_ONLY ? "-ghostsnp" : ""}.json`, JSON.stringify(results, null, 1));
