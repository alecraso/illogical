// illogical service worker: shows push notifications (a pane needs you)
// and opens that pane when one is tapped. It also keeps the last copy of
// the page itself, used only when the daemon that serves it doesn't
// answer: the page can then still reach the other hosts on its saved list
// (M4a). Nothing else is cached.

const SHELL = "illogical-shell-v1";

self.addEventListener("install", () => self.skipWaiting());
self.addEventListener("activate", (e) => e.waitUntil(self.clients.claim()));

self.addEventListener("fetch", (event) => {
  const req = event.request;
  const url = new URL(req.url);
  const page = req.mode === "navigate" && url.pathname === "/";
  const asset = url.pathname.startsWith("/assets/") || url.pathname === "/icon.svg";
  if (req.method !== "GET" || url.origin !== self.location.origin || !(page || asset)) return;
  event.respondWith(
    (async () => {
      const cache = await caches.open(SHELL);
      const key = page ? "/" : req;
      try {
        const res = await fetch(req);
        if (res.ok) await cache.put(key, res.clone());
        return res;
      } catch (e) {
        const saved = await cache.match(key);
        if (saved) return saved;
        throw e;
      }
    })(),
  );
});

self.addEventListener("push", (event) => {
  let msg = { title: "illogical", body: "" };
  try {
    msg = event.data.json();
  } catch {
    // not JSON: show what came
    if (event.data) msg.body = event.data.text();
  }
  event.waitUntil(
    self.registration.showNotification(msg.title || "illogical", {
      body: msg.body || "",
      tag: msg.tag || "illogical",
      renotify: true,
      icon: "/icon.svg",
      data: { pane: msg.pane },
    }),
  );
});

self.addEventListener("notificationclick", (event) => {
  event.notification.close();
  const pane = event.notification.data && event.notification.data.pane;
  const url = pane ? `/#pane=${pane}` : "/";
  event.waitUntil(
    (async () => {
      const wins = await self.clients.matchAll({ type: "window", includeUncontrolled: true });
      for (const w of wins) {
        if ("focus" in w) {
          w.postMessage({ type: "open-pane", pane });
          return w.focus();
        }
      }
      return self.clients.openWindow(url);
    })(),
  );
});
