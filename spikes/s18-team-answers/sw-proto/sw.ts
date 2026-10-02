// S18 Q3: can a service worker answer a notification over an end-to-end
// channel, with the device key the page made (non-extractable, in
// IndexedDB)? On a push it:
//   1. loads the device keys from IndexedDB (what the page stored),
//   2. checks the daemon's certificate chain, from a /fixtures.json that
//      stands in for control's /api/devices,
//   3. opens a WebSocket from the worker and runs the Noise IK handshake
//      (to the Rust responder, standing in for relay + daemon),
//   4. sends one request (POST /api/blocks/<pane>/call/approve),
// and reports how long each step took to the page and in a notification.

import { loadKeys } from "../../../web/src/e2e/keys.ts";
import { evaluate } from "../../../web/src/e2e/cert.ts";
import { E2ESocket } from "../../../web/src/e2e/channel.ts";

declare const self: ServiceWorkerGlobalScope;

self.addEventListener("install", () => void self.skipWaiting());
self.addEventListener("activate", (e) => e.waitUntil(self.clients.claim()));

const t = () => performance.now();

self.addEventListener("push", (event) => {
  const t0 = t();
  const msg = event.data?.json() ?? {};
  event.waitUntil(
    (async () => {
      const r: Record<string, unknown> = { kind: msg.kind, workerAgeMs: Math.round(t0) };
      try {
        const keys = await loadKeys();
        r.keysMs = +(t() - t0).toFixed(1);
        r.keyExtractable = keys.noise.privateKey.extractable;
        const fx = await (await fetch("/fixtures.json")).json();
        const trusted = await evaluate(fx.trust, fx.certs, fx.revocations);
        r.chainMs = +(t() - t0).toFixed(1);
        r.daemonTrusted = trusted.has(fx.daemon.device);
        const sock = await E2ESocket.connect([{ url: msg.url, timeoutMs: 3000 }], msg.daemon, keys);
        r.channelMs = +(t() - t0).toFixed(1);
        const res = await sock.request("POST", `/api/blocks/${msg.pane}/call/approve`, { id: msg.approve });
        r.answerMs = +(t() - t0).toFixed(1);
        r.status = res.status;
        r.reply = res.json();
        sock.close();
      } catch (e) {
        r.error = String(e);
      }
      await self.registration.showNotification("S18", { body: JSON.stringify(r), tag: "s18" });
      for (const c of await self.clients.matchAll({ includeUncontrolled: true })) c.postMessage({ s18: r });
    })(),
  );
});
