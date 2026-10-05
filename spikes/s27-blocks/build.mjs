// Bundles control's static files for block origins (and its stand-in page)
// into dist/.
import { build } from "esbuild";
import { copyFileSync, mkdirSync } from "node:fs";

mkdirSync("dist", { recursive: true });
const common = { bundle: true, target: "es2022", logLevel: "warning", minify: false };
await Promise.all([
  build({ ...common, entryPoints: ["web/sw.ts"], outfile: "dist/sw.js", format: "iife" }),
  build({ ...common, entryPoints: ["web/boot.ts"], outfile: "dist/boot.js", format: "iife" }),
  build({ ...common, entryPoints: ["web/shim.ts"], outfile: "dist/shim.js", format: "iife" }),
  build({ ...common, entryPoints: ["web/parent.ts"], outfile: "dist/parent.js", format: "esm" }),
]);
copyFileSync("web/parent.html", "dist/parent.html");
