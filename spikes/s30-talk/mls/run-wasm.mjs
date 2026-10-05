// Run the wasm build in node: node run-wasm.mjs [variant] [demo|scenarios|bench] [sizes] [reps]
//   variant: wasm-rustcrypto (default), wasm-fast-rustcrypto, wasm-libcrux, wasm-fast-libcrux
import { createRequire } from "node:module";
import { performance } from "node:perf_hooks";
const require = createRequire(import.meta.url);
const [variant = "wasm-rustcrypto", what = "bench", sizes = "2,10,50", reps = "9"] = process.argv.slice(2);
const t0 = performance.now();
const m = require(`./pkg/${variant}/node/s30_mls.js`);
console.log(`# ${variant}: loaded and instantiated in ${(performance.now() - t0).toFixed(1)} ms (node ${process.version})`);
if (what === "demo") console.log(m.demo());
else if (what === "scenarios") console.log(m.scenarios());
else {
  const rows = JSON.parse(m.bench(sizes, Number(reps)));
  console.log("| N | operation | median ms | min ms | bytes |\n|---|---|---|---|---|");
  for (const r of rows) console.log(`| ${r.n} | ${r.op} | ${r.median_ms.toFixed(2)} | ${r.min_ms.toFixed(2)} | ${r.bytes ?? ""} |`);
  if (process.env.OUT) require("node:fs").writeFileSync(process.env.OUT, JSON.stringify(rows, null, 1));
}
