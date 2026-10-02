// Static server for the S10 pages. Loopback only, port 7791 by default.
//   /            this directory (page.html, engines.mjs, ...)
//   /nm/         work/node_modules (xterm.js 6, ghostty-web stable + next)
//   /data/       work/data (fixtures and snapshots from gen/)
//   /wasm/       upstream libghostty-vt wasm built from S5's pinned commit
import { createServer } from "node:http";
import { readFile, stat } from "node:fs/promises";
import { extname, join, normalize } from "node:path";

const here = new URL(".", import.meta.url).pathname;
const roots = [
  ["/nm/", join(here, "work/node_modules")],
  ["/data/", join(here, "work/data")],
  ["/wasm/", join(here, "work/ghostty-22d1317/zig-out/bin")],
  ["/", here],
];
const types = {
  ".html": "text/html", ".mjs": "text/javascript", ".js": "text/javascript", ".css": "text/css",
  ".wasm": "application/wasm", ".json": "application/json",
};

export function serve(port = Number(process.env.PORT ?? 7791)) {
  const server = createServer(async (req, res) => {
    const path = decodeURIComponent(new URL(req.url, "http://x").pathname);
    const [prefix, root] = roots.find(([p]) => path.startsWith(p));
    const file = normalize(join(root, path.slice(prefix.length)));
    if (!file.startsWith(root)) return res.writeHead(403).end();
    try {
      if (!(await stat(file)).isFile()) throw new Error();
      res.writeHead(200, { "content-type": types[extname(file)] ?? "application/octet-stream", "cache-control": "no-store" });
      res.end(await readFile(file));
    } catch {
      res.writeHead(404).end();
    }
  });
  return new Promise((r) => server.listen(port, "127.0.0.1", () => r(server)));
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const s = await serve();
  console.log(`http://127.0.0.1:${s.address().port}/page.html`);
}
