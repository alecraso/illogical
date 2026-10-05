// What the bootstrap page and the shim share: asking the parent page for
// the block's credentials, and handing them to the worker.

import type { Creds } from "./chan.ts";

/** Ask control's page (the parent) for credentials. It answers only this
 * frame, and only for the block it made the frame for. */
export function askParent(control: string, kind: "hello" | "renew", timeoutMs = 10_000): Promise<Creds> {
  return new Promise((res, rej) => {
    const t = setTimeout(() => {
      removeEventListener("message", on);
      rej(new Error("the parent page didn't answer"));
    }, timeoutMs);
    const on = (e: MessageEvent) => {
      if (e.origin !== control || e.source !== parent || e.data?.s27 !== "creds") return;
      clearTimeout(t);
      removeEventListener("message", on);
      res(e.data.creds as Creds);
    };
    addEventListener("message", on);
    parent.postMessage({ s27: kind }, control);
  });
}

export async function giveWorker(creds: Creds): Promise<void> {
  const reg = await navigator.serviceWorker.ready;
  const sw = reg.active!;
  await new Promise<void>((res, rej) => {
    const ch = new MessageChannel();
    const t = setTimeout(() => rej(new Error("the worker didn't take the key")), 10_000);
    ch.port1.onmessage = () => {
      clearTimeout(t);
      res();
    };
    sw.postMessage({ s27: "creds", creds }, [ch.port2]);
  });
}
