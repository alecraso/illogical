// Web Push: let the daemon notify this device when a pane needs you.

/** `insecure`: plain http, where browsers have no push. `install`: iOS in
 * a Safari tab, which has push only once added to the Home Screen (#96). */
export type PushState = "unsupported" | "insecure" | "install" | "denied" | "on" | "off";

const supported = () => "serviceWorker" in navigator && "PushManager" in window && "Notification" in window;

/** iOS or iPadOS (which says it's a Mac, but has touch). */
export const ios = () => /iPhone|iPad|iPod/.test(navigator.userAgent) || (/Macintosh/.test(navigator.userAgent) && navigator.maxTouchPoints > 1);

/** Opened from the Home Screen, not a browser tab. */
export const standalone = () =>
  matchMedia("(display-mode: standalone)").matches || (navigator as { standalone?: boolean }).standalone === true;

/** Register the service worker (also what makes the app installable). A
 * notification through control (M21) says which daemon it's from. */
export async function registerWorker(onOpenPane: (pane: number, daemon?: string, thread?: string) => void) {
  if (!("serviceWorker" in navigator)) return;
  try {
    await navigator.serviceWorker.register("/sw.js");
    navigator.serviceWorker.addEventListener("message", (e) => {
      if (e.data?.type === "open-pane" && typeof e.data.pane === "number") onOpenPane(e.data.pane, e.data.daemon, e.data.thread);
      // #104: control's page looks for what waits (its prompt shows).
      if (e.data?.type === "control-refresh") dispatchEvent(new Event("arugula:control-refresh"));
      // M26: at its card on the swarm's rail.
      if (e.data?.type === "open-card" && typeof e.data.pane === "number") {
        location.hash = `swarm=${e.data.daemon ? `${e.data.daemon}.` : ""}${e.data.pane}`;
      }
    });
  } catch {
    // Not a secure context (plain http on the tailnet): no worker, no push.
  }
}

export async function pushState(): Promise<PushState> {
  if (!isSecureContext) return "insecure";
  if (!supported()) return ios() && !standalone() ? "install" : "unsupported";
  if (Notification.permission === "denied") return "denied";
  const reg = await navigator.serviceWorker.getRegistration();
  const sub = await reg?.pushManager.getSubscription();
  return sub ? "on" : "off";
}

function key(b64url: string): Uint8Array<ArrayBuffer> {
  const b64 = b64url.replace(/-/g, "+").replace(/_/g, "/") + "=".repeat((4 - (b64url.length % 4)) % 4);
  return Uint8Array.from(atob(b64), (c) => c.charCodeAt(0));
}

/** In control mode, subscribing goes to control instead (M21). */
let backend: { enable(): Promise<PushState>; disable(): Promise<PushState> } | null = null;
export function setPushBackend(b: typeof backend) {
  backend = b;
}

export async function enablePush(): Promise<PushState> {
  if (backend) return backend.enable();
  if (!supported()) return "unsupported";
  if ((await Notification.requestPermission()) !== "granted") return "denied";
  const reg = (await navigator.serviceWorker.getRegistration()) ?? (await navigator.serviceWorker.register("/sw.js"));
  await navigator.serviceWorker.ready;
  const res = await fetch("/api/push/key");
  if (!res.ok) throw new Error("this daemon has push turned off");
  const { key: k } = (await res.json()) as { key: string };
  const sub = await reg.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: key(k) });
  const saved = await fetch("/api/push/subscribe", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(sub.toJSON()),
  });
  if (!saved.ok) throw new Error(`subscribing failed: ${saved.status}`);
  return "on";
}

/** Control mode (M21): subscribe once with control's key, and sign the
 * subscription with this device's key so control can't swap it. */
export async function enableControlPush(vapid: string, sign: (sub: { endpoint: string; p256dh: string; auth: string }) => Promise<unknown>): Promise<PushState> {
  if (!supported()) return "unsupported";
  if ((await Notification.requestPermission()) !== "granted") return "denied";
  const reg = (await navigator.serviceWorker.getRegistration()) ?? (await navigator.serviceWorker.register("/sw.js"));
  await navigator.serviceWorker.ready;
  const sub = await reg.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: key(vapid) });
  const j = sub.toJSON() as { endpoint: string; keys: { p256dh: string; auth: string } };
  await sign({ endpoint: j.endpoint, p256dh: j.keys.p256dh, auth: j.keys.auth });
  return "on";
}

export async function disablePush(): Promise<PushState> {
  if (backend) return backend.disable();
  const reg = await navigator.serviceWorker.getRegistration();
  await (await reg?.pushManager.getSubscription())?.unsubscribe();
  return "off";
}
