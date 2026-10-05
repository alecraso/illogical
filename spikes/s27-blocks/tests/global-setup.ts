// A CONNECT proxy for both browsers: every `*.test` name goes to
// 127.0.0.1 at the port asked for. That's how control.test, *.blocks.test
// (control's block domain, a site of its own) and the daemon's names
// resolve without touching /etc/hosts or the system's DNS.

import http from "node:http";
import net from "node:net";

export default async function () {
  const port = Number(process.env.S27_PROXY_PORT ?? 7753);
  const server = http.createServer((_, res) => res.writeHead(405).end());
  const open = new Set<net.Socket>();
  server.on("connect", (req, duplex, head) => {
    const client = duplex as net.Socket;
    open.add(client);
    client.on("close", () => open.delete(client));
    const [host, p] = (req.url ?? "").split(":");
    if (!host.endsWith(".test")) {
      client.end("HTTP/1.1 403 Forbidden\r\n\r\n");
      return;
    }
    const up = net.connect(Number(p) || 443, "127.0.0.1", () => {
      up.setNoDelay(true);
      client.setNoDelay(true);
      client.write("HTTP/1.1 200 Connection Established\r\n\r\n");
      if (head.length) up.write(head);
      up.pipe(client);
      client.pipe(up);
    });
    up.on("error", () => client.destroy());
    client.on("error", () => up.destroy());
  });
  await new Promise<void>((res, rej) => {
    server.once("error", rej);
    server.listen(port, "127.0.0.1", () => res());
  });
  return () =>
    new Promise<void>((res) => {
      for (const s of open) s.destroy();
      server.closeAllConnections();
      server.close(() => res());
    });
}
