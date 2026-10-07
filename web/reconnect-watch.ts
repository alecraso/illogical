// A headless web client for tests that take a daemon away under it and
// bring it back (#26's "the phone reconnects on its own", #214 section 4).
// It signs in with a sign-in link, shows one pane, and prints a JSON line
// each time what it sees changes:
//
//   {"connected":true,"marker":true,"restored":false,"loads":1}
//
// `marker`: the pane's text has MARKER; `restored`: it has the restore
// marker; `loads`: page loads so far (a reconnect on its own keeps it at 1).
// Nothing is clicked or reloaded after the first load. It runs until killed
// or its stdin closes.
//
//   node --experimental-strip-types --no-warnings reconnect-watch.ts SIGNIN_URL PANE MARKER [VIA]
//
// VIA (host:port) is where the browser really connects for SIGNIN_URL's
// host and port, such as an ssh forward: the daemon only answers to its own
// host names, so the page keeps the link's (localhost:7681) and Chromium
// maps it to the forward.
//
// Uses Playwright's Chromium (`pnpm exec playwright install chromium`).

import { chromium } from "@playwright/test";

const [url, paneArg, marker, via] = process.argv.slice(2);
if (!url || !paneArg || !marker) {
  console.error("usage: reconnect-watch.ts SIGNIN_URL PANE MARKER");
  process.exit(2);
}
const pane = Number(paneArg);

const browser = await chromium.launch({
  args: via ? [`--host-resolver-rules=MAP ${new URL(url).host} ${via}`] : [],
});
const quit = async () => {
  await browser.close().catch(() => {});
  process.exit(0);
};
process.stdin.on("end", quit).resume();
process.on("SIGTERM", quit);

const page = await browser.newPage({ viewport: { width: 1000, height: 640 } });
let loads = 0;
page.on("load", () => loads++);
await page.goto(url);

type Seen = { connected: boolean; marker: boolean; restored: boolean; loads: number };
let last = "";
for (;;) {
  const seen: Seen = await page
    .evaluate(
      ([p, m]) => {
        const w = window as unknown as {
          __arugula?: { client: { connected: boolean; info(p: number): unknown; setActive(p: number): void }; text(p: number): string };
        };
        const i = w.__arugula;
        if (!i) return { connected: false, marker: false, restored: false, loads: 0 };
        // Show the pane once the client knows it (the page never changes
        // what it shows after that).
        if (i.client.connected && i.client.info(p as number)) i.client.setActive(p as number);
        const t = i.text(p as number);
        return { connected: i.client.connected, marker: t.includes(m as string), restored: t.includes("── restored"), loads: 0 };
      },
      [pane, marker] as const,
    )
    .catch(() => ({ connected: false, marker: false, restored: false, loads: 0 }));
  seen.loads = loads;
  const line = JSON.stringify(seen);
  if (line !== last) {
    console.log(line);
    last = line;
  }
  await new Promise((r) => setTimeout(r, 200));
}
