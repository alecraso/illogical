// S6 bridge: a local TCP port whose every connection becomes one Sprites
// proxy WebSocket to a port inside a sprite. Bytes are passed through raw, so
// HTTP, keep-alive and WebSocket upgrades (Vite HMR) all just work.
// env: SPRITES_API_URL, SPRITE_TOKEN
// usage: bun proxy.ts <sprite> <sprite-port> <listen-port>
// It logs the header *names* of each connection's first request (never values
// of identity headers), so we can see what `tailscale serve` adds.
// REWRITE=1: rewrite Host and Origin to localhost:<port> and strip
// Tailscale-*/X-Forwarded-* so the dev server needs no config.
const BASE = process.env.SPRITES_API_URL ?? "http://127.0.0.1:7788";
const TOKEN = process.env.SPRITE_TOKEN ?? "";
const [sprite, port, listen] = process.argv.slice(2);
const url = `${BASE.replace(/^http/, "ws")}/v1/sprites/${sprite}/proxy`;
let n = 0;
const t0 = performance.now();
const ms = () => Math.round(performance.now() - t0);

type Conn = { id: number; ws?: WebSocket; ready: boolean; pending: Uint8Array[]; first: boolean };

Bun.listen<Conn>({
  hostname: "127.0.0.1",
  port: Number(listen),
  socket: {
    open(sock) {
      const c: Conn = { id: ++n, ready: false, pending: [], first: true };
      sock.data = c;
      const opened = ms();
      const ws = new WebSocket(url, { headers: { Authorization: `Bearer ${TOKEN}` } } as any);
      ws.binaryType = "arraybuffer";
      c.ws = ws;
      ws.onopen = () => ws.send(JSON.stringify({ host: "localhost", port: Number(port) }));
      ws.onmessage = (m) => {
        if (typeof m.data === "string") { // the one control frame
          if (!c.ready) {
            c.ready = true;
            if (process.env.VERBOSE) console.log(`[${ms()}] #${c.id} connected in ${ms() - opened}ms ${m.data}`);
            for (const p of c.pending) ws.send(p);
            c.pending = [];
          }
          return;
        }
        sock.write(new Uint8Array(m.data as ArrayBuffer));
      };
      ws.onclose = () => sock.end();
      ws.onerror = () => sock.end();
    },
    data(sock, buf) {
      const c = sock.data;
      if (c.first) {
        c.first = false;
        const head = new TextDecoder().decode(buf.subarray(0, 4096)).split("\r\n\r\n")[0].split("\r\n");
        const names = head.slice(1).map((l) => l.split(":")[0]);
        const host = head.find((l) => /^host:/i.test(l)) ?? "";
        const origin = head.find((l) => /^origin:/i.test(l)) ?? "";
        console.log(`[${ms()}] #${c.id} ${head[0]} | ${host} | ${origin} | headers: ${names.join(",")}`);
      }
      let copy = new Uint8Array(buf);
      if (process.env.REWRITE) {
        // Spike-grade: rewrite Host/Origin in any chunk that carries request
        // headers, and drop serve's identity headers. A real proxy parses HTTP.
        const text = new TextDecoder("latin1").decode(copy);
        const end = text.indexOf("\r\n\r\n");
        if (end > 0 && /^[A-Z]+ \S+ HTTP\/1\.1\r\n/.test(text)) {
          const local = `localhost:${port}`;
          const head = text.slice(0, end)
            .replace(/\r\nHost: [^\r]*/i, `\r\nHost: ${local}`)
            .replace(/\r\nOrigin: (?!null)[^\r]*/i, `\r\nOrigin: http://${local}`)
            .replace(/\r\n(Tailscale-[^:]*|X-Forwarded-[^:]*): [^\r]*/gi, "");
          // single-byte decode, so `end` is also the byte offset; keep the body bytes as they were
          const h = new TextEncoder().encode(head);
          const out = new Uint8Array(h.length + copy.length - end);
          out.set(h); out.set(copy.subarray(end), h.length);
          copy = out;
        }
      }
      if (c.ready) c.ws!.send(copy); else c.pending.push(copy);
    },
    close(sock) { sock.data?.ws?.close(); },
  },
});
console.log(`listening 127.0.0.1:${listen} -> ${sprite}:${port}`);
