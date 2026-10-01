// illogical service worker: shows push notifications (a pane needs you)
// and opens that pane when one is tapped. Nothing is cached: the app needs
// the daemon anyway.

self.addEventListener("install", () => self.skipWaiting());
self.addEventListener("activate", (e) => e.waitUntil(self.clients.claim()));

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
