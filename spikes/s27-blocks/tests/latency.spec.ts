// What the worker, Noise and the relay add per request, against today's
// path (the daemon's own block site, no worker): sequential small
// requests, a burst of parallel ones (a dev server's module graph), a
// 10 MB download, and a block's first load. Loopback only, so these are
// the overhead of the path, not of a network; the machine's load is
// recorded next to each number.

import { expect, openBlock, openDirect, type Opened, record, test } from "./stack.ts";

const N = Number(process.env.S27_N ?? 300);

async function measure(b: Opened) {
  return b.frame.evaluate(async (n) => {
    const q = (xs: number[], p: number) => [...xs].sort((a, b) => a - b)[Math.min(xs.length - 1, Math.floor(p * xs.length))];
    const round = (x: number) => Math.round(x * 100) / 100;
    for (let i = 0; i < 20; i++) await (await fetch("/small", { cache: "no-store" })).text();
    const seq: number[] = [];
    for (let i = 0; i < n; i++) {
      const t = performance.now();
      await (await fetch(`/small?${i}`, { cache: "no-store" })).text();
      seq.push(performance.now() - t);
    }
    let t = performance.now();
    await Promise.all(Array.from({ length: 200 }, (_, i) => fetch(`/small?p${i}`, { cache: "no-store" }).then((r) => r.text())));
    const parallel200 = performance.now() - t;
    t = performance.now();
    const big = await (await fetch("/big?n=10000000", { cache: "no-store" })).arrayBuffer();
    const bigMs = performance.now() - t;
    return {
      p50: round(q(seq, 0.5)),
      p90: round(q(seq, 0.9)),
      p99: round(q(seq, 0.99)),
      parallel200Ms: round(parallel200),
      mbPerS: round(big.byteLength / 1e6 / (bigMs / 1000)),
    };
  }, N);
}

/** From the frame's creation to its page's DOMContentLoaded (the page as
 * the block shows it: through the worker, after the bootstrap's reload). */
async function firstLoad(b: Opened, t0: number): Promise<number> {
  const done = await b.frame.evaluate(() => {
    const nav = performance.getEntriesByType("navigation")[0] as PerformanceNavigationTiming;
    return performance.timeOrigin + nav.domContentLoadedEventEnd;
  });
  return Math.round(done - t0);
}

test("latency: relayed, direct Noise, and today's block site", async ({ parent, stack, browserName }) => {
  test.setTimeout(300_000);
  let t = await parent.evaluate(() => Date.now());
  const relayed = await openBlock(parent, stack);
  const firstLoadRelayed = await firstLoad(relayed, t);
  // The worker straight to the daemon (as over a tailnet), no relay. In
  // Playwright's WebKit a worker's WebSocket to another host than its own
  // fails (see README), so this one is Chromium only.
  t = await parent.evaluate(() => Date.now());
  const noise =
    browserName === "webkit" ? null : await openBlock(parent, stack, { route: `wss://daemon.test:${stack.directPort}/e2e` });
  const firstLoadNoise = noise ? await firstLoad(noise, t) : null;
  t = await parent.evaluate(() => Date.now());
  const today = await openDirect(parent, stack);
  const firstLoadToday = await firstLoad(today, t);
  const out = {
    relayed: { ...(await measure(relayed)), firstLoadMs: firstLoadRelayed },
    directNoise: noise ? { ...(await measure(noise)), firstLoadMs: firstLoadNoise } : null,
    today: { ...(await measure(today)), firstLoadMs: firstLoadToday },
    n: N,
  };
  await record("latency", browserName, out);
  expect(out.relayed.p50).toBeGreaterThan(0);
});
