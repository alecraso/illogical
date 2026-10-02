// Q3 check outside the browser (same wasm, Node): decode each S1 fixture's
// GHOSTSNP (written by the daemon's engine) and compare with plain_text().
import { readFileSync } from "node:fs";
import { GhostSnp } from "./ghostsnp.mjs";
const here = new URL(".", import.meta.url).pathname;
const { instance } = await WebAssembly.instantiate(readFileSync(`${here}work/ghostty-22d1317/zig-out/bin/ghostty-vt.wasm`), { env: { log: () => {} } });
const g = new GhostSnp(instance.exports);
for (const n of ["seq", "modes", "nvim", "nvim_resize", "less", "top", "resize"]) {
  const r = g.decode(readFileSync(`${here}work/data/${n}.ghostsnp`));
  const want = readFileSync(`${here}work/data/${n}.plain`, "utf8").split("\n").map((l) => l.trimEnd()).join("\n").trimEnd();
  const got = r.plain.split("\n").map((l) => l.trimEnd()).join("\n").trimEnd();
  console.log(n.padEnd(12), got === want ? "identical" : "DIFF", `READY ${r.ready.toFixed(2)} ms`);
}
