// Web Push: let the daemon notify this device when a pane needs you.

export type PushState = "unsupported" | "denied" | "on" | "off";

const supported = () => "serviceWorker" in navigator && "PushManager" in window && "Notification" in window;

/** Register the service worker (also what makes the app installable). */
export async function registerWorker(onOpenPane: (pane: number) => void) {
  if (!("serviceWorker" in navigator)) return;
  try {
    await navigator.serviceWorker.register("/sw.js");
    navigator.serviceWorker.addEventListener("message", (e) => {
      if (e.data?.type === "open-pane" && typeof e.data.pane === "number") onOpenPane(e.data.pane);
    });
  } catch {
    // Not a secure context (plain http on the tailnet): no worker, no push.
  }
}

export async function pushState(): Promise<PushState> {
  if (!supported()) return "unsupported";
  if (Notification.permission === "denied") return "denied";
  const reg = await navigator.serviceWorker.getRegistration();
  const sub = await reg?.pushManager.getSubscription();
  return sub ? "on" : "off";
}

function key(b64url: string): Uint8Array<ArrayBuffer> {
  const b64 = b64url.replace(/-/g, "+").replace(/_/g, "/") + "=".repeat((4 - (b64url.length % 4)) % 4);
  return Uint8Array.from(atob(b64), (c) => c.charCodeAt(0));
}

export async function enablePush(): Promise<PushState> {
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

export async function disablePush(): Promise<PushState> {
  const reg = await navigator.serviceWorker.getRegistration();
  await (await reg?.pushManager.getSubscription())?.unsubscribe();
  return "off";
}
