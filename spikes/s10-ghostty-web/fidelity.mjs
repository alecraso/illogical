// Q2: feed each S1 fixture into xterm.js 6 and ghostty-web in real browsers
// and compare with the daemon engine (gen/: plain_text() and its visible grid).
//   node fidelity.mjs            all browsers, engines, fixtures
//   SHOTS=1 node fidelity.mjs    also screenshots into work/shots/
import { chromium, webkit, devices } from "playwright";
import { readFileSync, writeFileSync, mkdirSync } from "node:fs";
import { serve } from "./server.mjs";

const here = new URL(".", import.meta.url).pathname;
const data = (f) => readFileSync(`${here}work/data/${f}`);
const FIXTURES = ["seq", "modes", "nvim", "nvim_resize", "less", "top", "resize"];
const BIG_SCROLLBACK = 64 * 1024 * 1024; // the daemon's byte budget
const RUNS = [
  // [engine, extra options, label]
  ["xterm", { scrollback: 100000 }, "xterm"],
  ["gw", {}, "gw 0.4.0"],
  ["gw-next", {}, "gw next"],
  ["gw-next", { scrollback: BIG_SCROLLBACK }, "gw next, 64MiB sb"],
];
const BROWSERS = [
  ["chromium", chromium, { viewport: { width: 1280, height: 900 } }],
  ["chromium Pixel 7", chromium, devices["Pixel 7"]],
  ["webkit", webkit, { viewport: { width: 1280, height: 900 } }],
  ["webkit iPhone 15", webkit, devices["iPhone 15"]],
];

const norm = (s) => s.split("\n").map((l) => l.trimEnd()).join("\n").replace(/\n+$/, "");

function textDiff(want, got) {
  if (want === got) return null;
  const a = want.split("\n"), b = got.split("\n");
  let i = 0;
  while (i < a.length && a[i] === b[i]) i++;
  // Lines lost at the top (scrollback limit)?
  const tail = b.length < a.length && a.slice(a.length - b.length).join("\n") === got;
  if (tail) return `top ${a.length - b.length} of ${a.length} lines missing (scrollback)`;
  return `${a.length} vs ${b.length} lines; first diff line ${i}: want ${JSON.stringify(a[i])} got ${JSON.stringify(b[i])}`;
}

function resolver(pal) {
  return (c, isBg) => (c === null ? (isBg ? pal.bg : pal.fg) : typeof c === "number" ? pal.palette[c] : c);
}

const FIELDS = ["text", "width", "bold", "italic", "faint", "underline", "inverse", "strike", "fg", "bg"];
function cellDiff(ref, got, resolve) {
  const counts = {}, first = {};
  for (let y = 0; y < Math.max(ref.length, got.length); y++) {
    const rr = ref[y] ?? [], gr = got[y] ?? [];
    for (let x = 0; x < Math.max(rr.length, gr.length); x++) {
      const r = rr[x], g = gr[x];
      if (!r || !g) { counts.size = (counts.size ?? 0) + 1; continue; }
      for (let k = 0; k < FIELDS.length; k++) {
        let want = r[k], have = g[k];
        if (k === 0) { want ||= " "; have ||= " "; if (r[1] === 0) continue; }
        if (k === 1) { if (want === 0 && have === 0) continue; }
        if (k >= 8 && resolve) want = resolve(want, k === 9);
        if (k >= 8 && !resolve && want === null && have === null) continue;
        if (want !== have) {
          counts[FIELDS[k]] = (counts[FIELDS[k]] ?? 0) + 1;
          first[FIELDS[k]] ??= `(${x},${y}) want ${JSON.stringify(want)} got ${JSON.stringify(have)}`;
        }
      }
    }
  }
  return Object.keys(counts).length ? Object.entries(counts).map(([k, n]) => `${k}×${n} e.g. ${first[k] ?? ""}`) : null;
}

const server = await serve();
const base = `http://127.0.0.1:${server.address().port}/page.html`;
const results = [];
const shots = process.env.SHOTS;
if (shots) mkdirSync(`${here}work/shots`, { recursive: true });

for (const [bname, type, ctxOpts] of BROWSERS) {
  const browser = await type.launch();
  const ctx = await browser.newContext(ctxOpts);
  const page = await ctx.newPage();
  page.on("pageerror", (e) => console.log(`  [${bname}] pageerror: ${e.message}`));
  await page.goto(base);
  await page.waitForFunction(() => window.S10_READY);
  const palettes = {};
  for (const e of ["gw", "gw-next"]) palettes[e] = await page.evaluate((e) => S10.gwPalette(e), e);
  for (const [engine, opts, label] of RUNS) {
    for (const mode of ["raw", "snap"]) {
      for (const name of FIXTURES) {
        const ref = { plain: norm(data(`${name}.plain`).toString()), ...JSON.parse(data(`${name}.cells.json`)) };
        let r, err;
        // A fresh page (and wasm instance) per fixture unless REUSE is set:
        // ghostty-web leaks stale cells from freed terminals into new ones.
        if (!process.env.REUSE) {
          await page.goto(base);
          await page.waitForFunction(() => window.S10_READY);
        }
        try {
          r = await page.evaluate(([e, n, m, o]) => S10.fidelity(e, n, m, o), [engine, name, mode, opts]);
        } catch (e) {
          err = e.message.split("\n")[0];
        }
        const diffs = [];
        if (err) diffs.push(`error: ${err}`);
        else {
          if (r.alt !== ref.alt) diffs.push(`alt: want ${ref.alt} got ${r.alt}`);
          if (String(r.cursor) !== String(ref.cursor)) diffs.push(`cursor: want ${ref.cursor} got ${r.cursor}`);
          const td = textDiff(ref.plain, norm(r.text));
          if (td) diffs.push(`text(buffer API): ${td}`);
          if (r.gtext !== undefined) {
            const gd = textDiff(ref.plain, norm(r.gtext));
            if (gd) diffs.push(`text(graphemes): ${gd}`);
          }
          const cd = cellDiff(ref.rows, r.cells, engine === "xterm" ? null : resolver(palettes[engine]));
          if (cd) diffs.push(...cd.map((d) => `cells: ${d}`));
        }
        results.push({ browser: bname, engine: label, mode, name, ok: !diffs.length, diffs, ms: r?.ms, dpr: r?.dpr });
        console.log(`${bname.padEnd(17)} ${label.padEnd(18)} ${mode.padEnd(4)} ${name.padEnd(12)} ${diffs.length ? "DIFF" : "OK"}`);
        for (const d of diffs) console.log("      " + d);
        if (shots && mode === "snap" && ["nvim", "modes", "top"].includes(name) && ["xterm", "gw next"].includes(label)) {
          await page.locator("#term").screenshot({ path: `${here}work/shots/${bname.replace(/ /g, "_")}-${label.split(",")[0].replace(/ /g, "_")}-${name}.png` });
        }
      }
    }
  }
  await browser.close();
}
server.close();
writeFileSync(`${here}work/fidelity${process.env.REUSE ? "-reused" : ""}.json`, JSON.stringify(results, null, 1));
