// The bootstrap page: what control serves for any navigation on a block's
// origin that the worker didn't answer (the first load, a hard reload, a
// cleared worker). It registers the worker, gets the block's credentials
// from the parent page, gives them to the worker and loads the page again,
// now through the worker.

import { askParent, giveWorker } from "./page.ts";

declare global {
  interface Window {
    S27_CONTROL: string;
  }
}

const state = document.getElementById("s27-state")!;
const say = (t: string) => (state.textContent = t);

async function boot() {
  // A page that keeps coming back here isn't getting through the worker.
  const now = Date.now();
  const recent = JSON.parse(sessionStorage.getItem("s27-boots") ?? "[]").filter((t: number) => now - t < 10_000);
  recent.push(now);
  sessionStorage.setItem("s27-boots", JSON.stringify(recent));
  if (recent.length > 4) throw new Error("the block's worker isn't answering its pages");

  const reg = await navigator.serviceWorker.register("/.s27/sw.js", { scope: "/" });
  const creds = await askParent(window.S27_CONTROL, "hello");
  await giveWorker(creds);
  await new Promise<void>((res) => {
    const w = reg.active;
    if (w?.state === "activated") return res();
    const sw = (reg.installing ?? reg.waiting ?? w)!;
    sw.addEventListener("statechange", () => sw.state === "activated" && res());
  });
  say("Loading…");
  location.replace(location.href);
}

boot().catch((e) => say(`This block can't load: ${e.message ?? e}`));
