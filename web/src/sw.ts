// Arugula service worker: shows push notifications (a pane needs you)
// and opens that pane when one is tapped. An approval (an agent block's
// permission request, or Claude Code's in a terminal, M29) comes with Allow
// and Deny; a question with one or two answers comes with them (M6c); a
// failure or a finished command with Dismiss (M24). Those are answered from
// here, without opening the app: on the daemon's own page with a request to
// it, and through Arugula control over an end-to-end channel the worker
// opens itself, with this device's key and the daemon's Noise key from the
// directory the page checked (S18). It also keeps the last copy of the page
// itself, used only when the daemon that serves it doesn't answer: the page
// can then still reach the other hosts on its saved list (M4a). Nothing else
// is cached. Control's own notices (a new device or a team request
// waiting, #104) open its page, which shows the prompt.
//
// Built by itself into dist/sw.js as a classic worker (vite.sw.config.ts).

import { E2ESocket } from "./e2e/channel.ts";
import type { ActRequest } from "./proto";
import { existingKeys, loadWorkerDirectory } from "./e2e/keys.ts";
import { tapThread, tapUrl } from "./tap.ts";

/** The parts of a service worker's global scope used here (the project's
 * types are the page's). */
interface Worker {
  registration: ServiceWorkerRegistration;
  clients: {
    claim(): Promise<void>;
    matchAll(o?: { type?: string; includeUncontrolled?: boolean }): Promise<(Client & { focus?(): Promise<unknown> })[]>;
    openWindow(url: string): Promise<unknown>;
  };
  location: Location;
  skipWaiting(): Promise<void>;
  addEventListener(type: string, fn: (e: never) => void): void;
}
interface Client {
  postMessage(m: unknown): void;
}
interface Waiting {
  waitUntil(p: Promise<unknown>): void;
}
interface PushEvent extends Waiting {
  data: { json(): unknown; text(): string } | null;
}
interface ClickEvent extends Waiting {
  action: string;
  notification: Notification;
}
interface FetchEvent {
  request: Request;
  respondWith(r: Promise<Response>): void;
}

const sw = self as unknown as Worker;
const SHELL = "arugula-shell-v1";

// The cache the worker kept before the rename (#505): this one replaces it.
const OLD_SHELL = "illogical-shell-v1";

sw.addEventListener("install", () => void sw.skipWaiting());
sw.addEventListener("activate", (e: Waiting) =>
  e.waitUntil(Promise.all([sw.clients.claim(), caches.delete(OLD_SHELL).catch(() => false)])),
);

sw.addEventListener("fetch", (event: FetchEvent) => {
  const req = event.request;
  const url = new URL(req.url);
  const page = req.mode === "navigate" && url.pathname === "/";
  const asset = url.pathname.startsWith("/assets/") || url.pathname === "/icon.svg";
  if (req.method !== "GET" || url.origin !== sw.location.origin || !(page || asset)) return;
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

interface Msg {
  title?: string;
  body?: string;
  tag?: string;
  pane?: number;
  /** Through control (M21): the daemon it's from. */
  daemon?: string;
  approve?: { id: string; title?: string };
  ask?: { id: string; field: string; options: string[] };
  reason?: { kind: string; actions: string[] };
  /** Control's own (#104): a device or a person waits for approval. */
  control?: boolean;
  /** M61: an @mention in this thread (`pane-7`, `session-2`). */
  thread?: string;
}

sw.addEventListener("push", (event: PushEvent) => {
  let msg: Msg = { title: "Arugula", body: "" };
  try {
    msg = event.data?.json() as Msg;
  } catch {
    // not JSON: show what came
    if (event.data) msg.body = event.data.text();
  }
  const approve = msg.approve && typeof msg.approve.id === "string" ? msg.approve : null;
  const ask = msg.ask && typeof msg.ask.id === "string" && Array.isArray(msg.ask.options) ? msg.ask : null;
  const reason = msg.reason && Array.isArray(msg.reason.actions) ? msg.reason : null;
  const actions = approve
    ? [
        { action: "approve", title: "Allow" },
        { action: "deny", title: "Deny" },
      ]
    : ask
      ? ask.options.slice(0, 2).map((o, i) => ({ action: `answer-${i}`, title: o }))
      : reason && reason.actions.includes("rerun")
        ? [
            { action: "rerun", title: "Rerun" },
            { action: "dismiss", title: "Dismiss" },
          ]
        : reason && reason.actions.includes("dismiss")
          ? [{ action: "dismiss", title: "Dismiss" }]
          : [];
  event.waitUntil(
    sw.registration.showNotification(msg.title || "Arugula", {
      body: msg.body || "",
      tag: msg.tag || "arugula",
      renotify: true,
      icon: "/icon.svg",
      requireInteraction: !!(approve || ask),
      actions,
      data: { pane: msg.pane, daemon: msg.daemon, thread: msg.thread, approve, ask, reason, control: msg.control === true },
    } as NotificationOptions),
  );
});

/** `POST /api/attention/act` to the daemon a notification came from: this
 * page's own, or one reached through control over a channel of our own. */
async function act(daemon: string | undefined, body: ActRequest): Promise<boolean> {
  try {
    if (!daemon) {
      const res = await fetch("/api/attention/act", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(body),
      });
      return res.ok;
    }
    const [keys, dir] = await Promise.all([existingKeys(), loadWorkerDirectory()]);
    const d = dir.find((x) => x.id === daemon);
    if (!keys || !d) return false;
    const urls = [
      ...d.direct.map((u) => ({ url: `${u.replace(/^http/, "ws").replace(/\/$/, "")}/e2e`, timeoutMs: 1500 })),
      { url: d.relay, timeoutMs: 10_000 },
    ];
    const sock = await E2ESocket.connect(urls, { id: d.id, noise: d.noise }, keys);
    try {
      sock.start();
      const res = await sock.request("POST", "/api/attention/act", body);
      return res.ok;
    } finally {
      sock.close();
    }
  } catch {
    return false;
  }
}

/** Couldn't answer: say so, and let a tap open the pane. */
async function failed(pane: number, daemon: string | undefined, what: string) {
  await sw.registration.showNotification("Couldn't answer that", { body: what, tag: `pane-${pane}`, data: { pane, daemon } });
}

sw.addEventListener("notificationclick", (event: ClickEvent) => {
  event.notification.close();
  const data = (event.notification.data || {}) as Msg & { approve: Msg["approve"] | null; ask: Msg["ask"] | null };
  const pane = data.pane;
  if ((event.action === "approve" || event.action === "deny") && pane && data.approve) {
    const action = event.action === "approve" ? "allow" : "deny";
    const approve = data.approve;
    event.waitUntil(
      (async () => {
        // Already answered, or the daemon is unreachable: say so.
        if (!(await act(data.daemon, { action, pane, id: approve.id }))) await failed(pane, data.daemon, approve.title || "");
      })(),
    );
    return;
  }
  const picked = /^answer-(\d)$/.exec(event.action || "");
  if (picked && pane && data.ask) {
    const ask = data.ask;
    const choice = ask.options[Number(picked[1])];
    event.waitUntil(
      (async () => {
        const body = { action: "answer", pane, id: ask.id, content: { [ask.field]: choice } } satisfies ActRequest;
        if (!(await act(data.daemon, body))) await failed(pane, data.daemon, choice);
      })(),
    );
    return;
  }
  // A failed command, typed into its pane again (M11).
  if (event.action === "rerun" && pane) {
    event.waitUntil(
      (async () => {
        if (!(await act(data.daemon, { action: "rerun", pane }))) await failed(pane, data.daemon, "Rerun");
      })(),
    );
    return;
  }
  if (event.action === "dismiss" && pane) {
    event.waitUntil(act(data.daemon, { action: "dismiss", pane }));
    return;
  }
  // Control's notice (#104): its page shows the prompt, with the
  // fingerprint, once it looks again.
  if (data.control) {
    event.waitUntil(
      (async () => {
        const wins = await sw.clients.matchAll({ type: "window", includeUncontrolled: true });
        for (const w of wins) {
          if (w.focus) {
            w.postMessage({ type: "control-refresh" });
            return w.focus();
          }
        }
        return sw.clients.openWindow("/");
      })(),
    );
    return;
  }
  // A tap: open the pane (its card shows over it), or the card, or the
  // thread (tapUrl).
  const thread = tapThread(data.thread);
  const url = tapUrl(data);
  event.waitUntil(
    (async () => {
      const wins = await sw.clients.matchAll({ type: "window", includeUncontrolled: true });
      for (const w of wins) {
        if (w.focus) {
          w.postMessage({ type: data.reason ? "open-card" : "open-pane", pane, daemon: data.daemon, thread });
          return w.focus();
        }
      }
      return sw.clients.openWindow(url);
    })(),
  );
});
