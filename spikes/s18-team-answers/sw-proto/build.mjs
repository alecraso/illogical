// Bundle the prototype's page and service worker (IIFE: a classic worker,
// like the app's public/sw.js) into work/sw-dist/. Run from web/ so vite
// resolves: node ../spikes/s18-team-answers/sw-proto/build.mjs
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { createRequire } from "node:module";
import { writeFileSync } from "node:fs";

const here = dirname(fileURLToPath(import.meta.url));
const web = join(here, "..", "..", "..", "web");
const { build } = await import(pathToFileURL(createRequire(join(web, "package.json")).resolve("vite")).href);
const out = join(here, "..", "work", "sw-dist");
for (const [name, file, empty] of [["sw", "sw.ts", true], ["page", "page.ts", false]]) {
  await build({
    configFile: false,
    publicDir: false,
    logLevel: "warn",
    build: {
      outDir: out,
      emptyOutDir: empty,
      target: "es2022",
      minify: false,
      lib: { entry: join(here, file), formats: ["iife"], name: `s18_${name}`, fileName: () => `${name}.js` },
    },
  });
}
writeFileSync(join(out, "index.html"), `<!doctype html><meta charset="utf-8"><title>S18</title><script src="/page.js"></script>`);
console.log("built", out);
