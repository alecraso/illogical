// Browser side of the S10 harness. Driven by fidelity.mjs and attach.mjs
// through window.S10.
import { create } from "./engines.mjs";
import { GhostSnp } from "./ghostsnp.mjs";

const host = document.getElementById("term");
const get = async (path, as = "bytes") => {
  const r = await fetch(path, { cache: "no-store" });
  if (!r.ok) throw new Error(`${path}: ${r.status}`);
  return as === "json" ? r.json() : new Uint8Array(await r.arrayBuffer());
};
const tick = () => new Promise((r) => setTimeout(r, 0));

let current;
function fresh() {
  current?.dispose();
  current = undefined;
  host.replaceChildren();
}

/** Feed one fixture and read back what the engine holds.
 * mode "raw": the recorded PTY bytes with their resizes;
 * mode "snap": the daemon's formatter VT snapshot (what the client gets). */
async function fidelity(engine, name, mode, opts = {}) {
  fresh();
  const meta = await get(`/data/${name}.meta.json`, "json");
  const t0 = performance.now();
  if (mode === "raw") {
    const bytes = await get(`/data/${name}.bin`);
    current = await create(engine, host, { cols: meta.cols, rows: meta.rows, ...opts });
    let pos = 0;
    for (const r of meta.resizes) {
      await current.write(bytes.subarray(pos, r.offset));
      current.resize(r.cols, r.rows);
      pos = r.offset;
    }
    await current.write(bytes.subarray(pos));
  } else {
    const bytes = await get(`/data/${name}.vt`);
    current = await create(engine, host, { cols: meta.final_cols, rows: meta.final_rows, ...opts });
    await current.write(bytes);
  }
  await current.painted();
  const ms = performance.now() - t0;
  return {
    ms,
    alt: current.alt,
    cursor: current.cursor,
    scrollbackRows: current.scrollbackRows,
    text: current.text(),
    gtext: current.gtext?.(),
    cells: current.cells(),
    dpr: devicePixelRatio,
  };
}

/** ghostty-web's effective palette and default colors, read back from cells. */
async function gwPalette(engine) {
  fresh();
  const t = await create(engine, host, { cols: 256, rows: 3 });
  let s = "X\r\n";
  for (let i = 0; i < 256; i++) s += `\x1b[38;5;${i}mX`;
  await t.write(new TextEncoder().encode(s));
  const cells = t.cells();
  t.dispose();
  return { fg: cells[0][0][8], bg: cells[0][0][9], palette: cells[1].map((c) => c[8]) };
}

/** Attach: fetch a snapshot (as the WebSocket frame would arrive), write it
 * to a fresh terminal like Client.onFrame does (reset + write), and time:
 *   net     bytes in hand
 *   parsed  write callback (all bytes processed)
 *   drawn   the final screen painted
 * plus the longest main-thread block seen meanwhile (input latency). */
async function attach(engine, name, { gzip, ...opts } = {}) {
  fresh();
  const meta = await get(`/data/${name}.meta.json`, "json");
  current = await create(engine, host, { cols: meta.final_cols, rows: meta.final_rows, ...opts });
  await current.painted();
  await tick();
  let longest = 0, last = performance.now(), run = true;
  const probe = () => { const n = performance.now(); longest = Math.max(longest, n - last); last = n; if (run) setTimeout(probe, 0); };
  const t0 = performance.now();
  let bytes = await get(`/data/${name}.vt${gzip ? ".gz" : ""}`);
  const wire = bytes.length;
  const net = performance.now() - t0;
  let inflate = 0;
  if (gzip) {
    // As permessage-deflate (or a compressed snapshot frame) would cost.
    const s = performance.now();
    bytes = new Uint8Array(await new Response(new Blob([bytes]).stream().pipeThrough(new DecompressionStream("gzip"))).arrayBuffer());
    inflate = performance.now() - s;
  }
  last = performance.now();
  probe();
  const t1 = performance.now();
  await current.write(bytes);
  const parsed = performance.now() - t1 + inflate;
  await current.painted();
  const drawn = performance.now() - t1 + inflate;
  run = false;
  const lastLine = current.text().split("\n").filter(Boolean).pop();
  return { bytes: wire, inflate, net, parsed, drawn, total: net + drawn, longestBlock: longest, scrollbackRows: current.scrollbackRows, lastLine };
}

/** GHOSTSNP in the browser via upstream libghostty-vt wasm (no renderer):
 * time to READY and to the end of history. */
async function ghostsnp(name, { optimize = "small" } = {}) {
  const g = await GhostSnp.load(optimize === "fast" ? "/wasm/ghostty-vt-fast.wasm" : "/wasm/ghostty-vt.wasm");
  const t0 = performance.now();
  const bytes = await get(`/data/${name}.ghostsnp`);
  const net = performance.now() - t0;
  const r = g.decode(bytes);
  r.net = net;
  r.bytes = bytes.length;
  r.plainMatches = r.plain !== undefined ? r.plain.trimEnd() === new TextDecoder().decode(await get(`/data/${name}.plain`)).trimEnd() : undefined;
  delete r.plain;
  return r;
}

window.S10 = { fidelity, gwPalette, attach, ghostsnp, shot: () => host.getBoundingClientRect().toJSON() };
window.S10_READY = true;
