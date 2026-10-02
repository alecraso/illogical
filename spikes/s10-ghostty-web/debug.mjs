// Small probes behind findings in the README (OSC 4, grapheme width mode,
// stale cells across terminals, empty-cell text, scrollback units).
import { chromium } from "playwright";
import { serve } from "./server.mjs";
const server = await serve();
const browser = await chromium.launch();
const page = await browser.newPage();
await page.goto(`http://127.0.0.1:${server.address().port}/page.html`);
await page.waitForFunction(() => window.S10_READY);
const out = await page.evaluate(async () => {
  const { create } = await import("./engines.mjs");
  const host = document.getElementById("term");
  const enc = (s) => new TextEncoder().encode(s);
  const r = {};
  for (const e of ["gw", "gw-next"]) {
    const t = await create(e, host, { cols: 40, rows: 5 });
    await t.write(enc("\x1b[31mA\x1b]4;1;rgb:ff/00/ff\x1b\\\x1b[31mB\x1b[0m"));
    const c = t.cells();
    r[e] = { osc4_before: c[0][0][8], osc4_after: c[0][1][8], mode2027: t.t.wasmTerm.getMode(2027) };
    await t.write(enc("\r\n\x1b[2Kx\x1b[10Cy"));
    r[e].emptyCells = JSON.stringify(t.t.buffer.active.getLine(1).translateToString(true));
    t.dispose();
    host.replaceChildren();
    // A fresh terminal after a freed one: are its blank rows blank?
    const u = await create(e, host, { cols: 40, rows: 5 });
    await u.write(enc("hi"));
    r[e].staleAfterFree = u.text().split("\n").slice(1).filter(Boolean);
    u.dispose();
    host.replaceChildren();
    // Scrollback option: lines or bytes?
    const s = await create(e, host, { cols: 80, rows: 24, scrollback: 10000 });
    let txt = "";
    for (let i = 0; i < 5000; i++) txt += `line ${i}\r\n`;
    await s.write(enc(txt));
    r[e].scrollback10000keeps = s.scrollbackRows;
    s.dispose();
    host.replaceChildren();
  }
  return r;
});
console.log(JSON.stringify(out, null, 1));
await browser.close();
server.close();
