// S18 Q3: the page makes this browser's device keys (as the app does, into
// IndexedDB) and registers the prototype service worker.
import { loadKeys } from "../../../web/src/e2e/keys.ts";

declare global {
  interface Window {
    s18: { keys: string; results: unknown[]; ready: Promise<void> };
  }
}

const results: unknown[] = [];
navigator.serviceWorker.addEventListener("message", (e) => {
  if (e.data?.s18) results.push(e.data.s18);
});
window.s18 = {
  keys: "",
  results,
  ready: (async () => {
    const k = await loadKeys();
    window.s18.keys = k.id;
    await navigator.serviceWorker.register("/sw.js", { scope: "/" });
    await navigator.serviceWorker.ready;
  })(),
};
