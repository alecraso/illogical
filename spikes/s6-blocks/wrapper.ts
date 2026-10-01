// Stand-in for the app page: frames ?src= in a sandboxed iframe the way a
// browser block would (allow-scripts allow-forms, no allow-same-origin).
// usage: bun wrapper.ts <port>
const port = Number(process.argv[2] ?? 18174);
Bun.serve({
  hostname: "127.0.0.1",
  port,
  fetch(req) {
    const src = new URL(req.url).searchParams.get("src") ?? "about:blank";
    const sandbox = process.env.SANDBOX ?? "allow-scripts allow-forms";
    const html = `<!doctype html><html><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">
<title>app</title><style>
html,body{margin:0;height:100%;font:14px sans-serif}
header{height:44px;display:flex;align-items:center;padding:0 12px;background:#222;color:#eee}
iframe{display:block;border:0;width:100%;height:calc(100% - 44px)}
</style></head><body><header>illogical tab: browser block</header>
<iframe sandbox="${sandbox}" src="${src.replace(/"/g, "&quot;")}"></iframe></body></html>`;
    return new Response(html, { headers: { "content-type": "text/html; charset=utf-8" } });
  },
});
console.log(`wrapper on http://127.0.0.1:${port}`);
