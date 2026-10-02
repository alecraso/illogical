// illogical service worker: shows push notifications (a pane needs you)
// and opens that pane when one is tapped. An agent block's permission
// request comes with Approve and Deny actions, and an agent's question with
// one or two answers comes with them as actions (M6c), answered from here
// without opening the app. It also keeps the last copy of the page itself, used
// only when the daemon that serves it doesn't answer: the page can then
// still reach the other hosts on its saved list (M4a). Nothing else is
// cached.

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
  const approve = msg.approve && typeof msg.approve.id === "string" ? msg.approve : null;
  const ask = msg.ask && typeof msg.ask.id === "string" && Array.isArray(msg.ask.options) ? msg.ask : null;
  const actions = approve
    ? [
        { action: "approve", title: "Approve" },
        { action: "deny", title: "Deny" },
      ]
    : ask
      ? ask.options.slice(0, 2).map((o, i) => ({ action: `answer-${i}`, title: o }))
      : [];
  event.waitUntil(
    self.registration.showNotification(msg.title || "illogical", {
      body: msg.body || "",
      tag: msg.tag || "illogical",
      renotify: true,
      icon: "/icon.svg",
      requireInteraction: !!(approve || ask),
      actions,
      data: { pane: msg.pane, daemon: msg.daemon, approve, ask },
    }),
  );
});

/** Answer an agent's permission request or question; true if the daemon
 * took it. */
async function answer(pane, method, args) {
  try {
    const res = await fetch(`/api/blocks/${pane}/call/${method}`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(args),
    });
    return res.ok;
  } catch {
    return false;
  }
}

self.addEventListener("notificationclick", (event) => {
  event.notification.close();
  const data = event.notification.data || {};
  const pane = data.pane;
  if ((event.action === "approve" || event.action === "deny") && pane && data.approve) {
    event.waitUntil(
      (async () => {
        if (!(await answer(pane, event.action, { id: data.approve.id }))) {
          // Already answered, or the daemon is unreachable: show the block.
          await self.registration.showNotification("Couldn't answer that", {
            body: data.approve.title || "",
            tag: `pane-${pane}`,
            data: { pane },
          });
        }
      })(),
    );
    return;
  }
  const picked = /^answer-(\d)$/.exec(event.action || "");
  if (picked && pane && data.ask) {
    const choice = data.ask.options[Number(picked[1])];
    event.waitUntil(
      (async () => {
        const args = { id: data.ask.id, content: { [data.ask.field]: choice } };
        if (!(await answer(pane, "answer", args))) {
          await self.registration.showNotification("Couldn't answer that", {
            body: choice,
            tag: `pane-${pane}`,
            data: { pane },
          });
        }
      })(),
    );
    return;
  }
  // Through control (M21) a notification names its daemon.
  const url = pane ? (data.daemon ? `/#pane=${data.daemon}.${pane}` : `/#pane=${pane}`) : "/";
  event.waitUntil(
    (async () => {
      const wins = await self.clients.matchAll({ type: "window", includeUncontrolled: true });
      for (const w of wins) {
        if ("focus" in w) {
          w.postMessage({ type: "open-pane", pane, daemon: data.daemon });
          return w.focus();
        }
      }
      return self.clients.openWindow(url);
    })(),
  );
});
