// S30's test server: serves this directory, collects probe reports
// (POST /report -> reports/<client>-<time>.json) and relays call
// signaling between the peers in a room (/ws?room=NAME). It stands in for
// the session's daemon, which would relay the same messages over Noise.
import http from "node:http";
import fs from "node:fs";
import path from "node:path";
import { WebSocketServer } from "ws";

const dir = path.dirname(new URL(import.meta.url).pathname);
const port = Number(process.env.PORT || 8799);
fs.mkdirSync(path.join(dir, "reports"), { recursive: true });
const types = { ".html": "text/html", ".js": "text/javascript", ".mjs": "text/javascript", ".json": "application/json" };

const server = http.createServer((req, res) => {
  const u = new URL(req.url, "http://x");
  if (req.method === "POST" && u.pathname === "/live") {
    // One stats line per second from call.html?live=1, for watching a call.
    let body = "";
    req.on("data", (c) => (body += c));
    req.on("end", () => {
      let name = "unknown";
      try { name = (JSON.parse(body).client || "unknown").replace(/[^\w.-]/g, "_"); } catch {}
      fs.appendFileSync(path.join(dir, "reports", `live-${name}.jsonl`), body + "\n");
      res.end("ok");
    });
    return;
  }
  if (req.method === "POST" && u.pathname === "/report") {
    let body = "";
    req.on("data", (c) => (body += c));
    req.on("end", () => {
      let name = "unknown";
      try { name = (JSON.parse(body).client || "unknown").replace(/[^\w.-]/g, "_"); } catch {}
      const f = path.join(dir, "reports", `${name}-${Date.now()}.json`);
      fs.writeFileSync(f, body);
      console.log("report", f);
      res.end("ok");
    });
    return;
  }
  // /p: the phone's call, with its settings (TURN_IP: where coturn listens).
  if (u.pathname === "/p") {
    res.writeHead(302, { location: `/call.html?room=phone1&live=1&client=iphone&relay=1&turn=turn:${process.env.TURN_IP || "127.0.0.1"}:3478&user=s30&pass=s30secret` });
    return res.end();
  }
  const f = path.join(dir, path.normalize(u.pathname === "/" ? "/call.html" : u.pathname));
  if (!f.startsWith(dir) || !fs.existsSync(f) || fs.statSync(f).isDirectory()) { res.statusCode = 404; return res.end(); }
  res.setHeader("content-type", types[path.extname(f)] || "application/octet-stream");
  fs.createReadStream(f).pipe(res);
});

const rooms = new Map();
const wss = new WebSocketServer({ server, path: "/ws" });
wss.on("connection", (ws, req) => {
  const room = new URL(req.url, "http://x").searchParams.get("room") || "default";
  if (!rooms.has(room)) rooms.set(room, new Set());
  const peers = rooms.get(room);
  peers.add(ws);
  console.log("join", room, peers.size);
  for (const p of peers) if (p !== ws) p.send(JSON.stringify({ type: "peer" }));
  ws.on("message", (m) => {
    let text = m.toString();
    // A room named tamper-* plays a hostile signaling server: it swaps
    // the DTLS fingerprint for one of its own, as a MITM would.
    if (room.startsWith("tamper")) text = text.replace(/(a=fingerprint:sha-256 )([0-9A-F]{2})/g, (_, a, b) => a + (b === "00" ? "11" : "00"));
    for (const p of peers) if (p !== ws) p.send(text);
  });
  ws.on("close", () => { peers.delete(ws); for (const p of peers) p.send(JSON.stringify({ type: "bye" })); });
});
server.listen(port, process.env.HOST || "127.0.0.1", () => console.log("listening", port));
