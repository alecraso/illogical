// What happens to a block over its life: control's page reloaded (hard,
// in Chromium), a crashed worker, a worker or its storage cleared, a worker
// stopped for being idle, grants expiring under a block left open, control
// restarting under it, and a parent page whose clock is a day off.

import { type Page } from "@playwright/test";
import { admin, controlStats, expect, type Opened, openBlock, openDirect, record, type Stack, test, trustParent, workerStats } from "./stack.ts";

/** A WebSocket echo in the block's page (through the shim), kept on
 * `window.echo`; `say` sends and waits for the echo. */
async function openEcho(b: Opened) {
  await b.frame.evaluate(
    () =>
      new Promise<void>((res, rej) => {
        const ws = new WebSocket(`wss://${location.host}/ws`);
        (window as any).echo = ws;
        ws.onopen = () => res();
        ws.onerror = () => rej(new Error("echo socket failed"));
      }),
  );
}

function say(b: Opened, text: string): Promise<string> {
  return b.frame.evaluate(
    (text) =>
      new Promise<string>((res) => {
        const ws = (window as any).echo as WebSocket;
        if (ws.readyState !== WebSocket.OPEN) return res(`closed:${ws.readyState}`);
        ws.onmessage = (e) => res(e.data);
        ws.send(text);
        setTimeout(() => res("timeout"), 5000);
      }),
    text,
  );
}

async function get(b: Opened, path = "/small"): Promise<{ status: number; ms: number; why?: string }> {
  const r = await b.frame.evaluate(async (path) => {
    const t = performance.now();
    const r = await fetch(path, { cache: "no-store" });
    const body = await r.text();
    return { status: r.status, ms: Math.round(performance.now() - t), why: r.ok ? undefined : body };
  }, path);
  if (r.why) console.log(`GET ${path}: ${r.status} ${r.why}`);
  return r;
}

async function reopen(page: Page, stack: Stack, key: string, block: string): Promise<Opened> {
  await page.goto(stack.origin + "/");
  await trustParent(page, stack);
  return openBlock(page, stack, { key, block });
}

test("control's page reloaded (hard, in Chromium): the block comes back on its origin", async ({ parent, stack, browserName }) => {
  const b = await openBlock(parent, stack);
  const key = new URL(b.origin).hostname.split(".")[0].slice(2);
  const boots0 = (await controlStats(stack)).boots;
  // A normal reload: the block's worker answers its first navigation.
  let again = await reopen(parent, stack, key, b.block);
  expect((await get(again)).status).toBe(200);
  const afterReload = (await controlStats(stack)).boots - boots0;
  let afterHard: number | null = null;
  if (browserName === "chromium") {
    const cdp = await parent.context().newCDPSession(parent);
    const boots1 = (await controlStats(stack)).boots;
    await cdp.send("Page.reload", { ignoreCache: true });
    await parent.waitForLoadState("load");
    await trustParent(parent, stack);
    again = await openBlock(parent, stack, { key, block: b.block });
    expect((await get(again)).status).toBe(200);
    afterHard = (await controlStats(stack)).boots - boots1;
  }
  await record("reload", browserName, { bootstrapLoadsOnReload: afterReload, bootstrapLoadsOnHardReload: afterHard });
  expect(afterReload).toBe(0);
});

test("a crashed worker (Chromium): the next request starts it again", async ({ parent, stack, browserName }) => {
  test.skip(browserName !== "chromium", "stopping a worker needs CDP");
  const b = await openBlock(parent, stack);
  await openEcho(b);
  const before = await workerStats(b.frame);
  const cdp = await parent.context().newCDPSession(parent);
  await cdp.send("ServiceWorker.enable");
  await cdp.send("ServiceWorker.stopAllWorkers");
  const first = await get(b);
  expect(first.status).toBe(200);
  const after = await workerStats(b.frame);
  expect(after.started).not.toBe(before.started);
  // The page's own channel (the shim's) didn't notice.
  expect(await say(b, "still here")).toBe("still here");
  await record("crashed-worker", browserName, { firstRequestMs: first.ms, reconnectMs: Math.round(after.lastConnectMs) });
});

test("a worker unregistered, or its storage cleared: the bootstrap page brings it back", async ({ parent, stack, browserName }) => {
  const b = await openBlock(parent, stack);
  const boots0 = (await controlStats(stack)).boots;
  await b.frame.evaluate(async () => {
    await (await navigator.serviceWorker.ready).unregister();
    setTimeout(() => location.reload(), 50);
  });
  await expect(b.frame.locator("h1")).toHaveText("block home", { timeout: 30_000 });
  expect((await get(b)).status).toBe(200);
  await b.frame.evaluate(async () => {
    await (await navigator.serviceWorker.ready).unregister();
    await new Promise((res) => {
      const r = indexedDB.deleteDatabase("s27");
      r.onsuccess = r.onerror = r.onblocked = res;
    });
    setTimeout(() => location.reload(), 50);
  });
  await expect(b.frame.locator("h1")).toHaveText("block home", { timeout: 30_000 });
  expect((await get(b)).status).toBe(200);
  const boots = (await controlStats(stack)).boots - boots0;
  // Whether the reload went through the bootstrap page depends on timing:
  // an unregistered worker can still answer while a page uses it. Either
  // way the block works; the count is kept.
  await record("worker-cleared", browserName, { bootstrapLoads: boots });
});

test("an idle worker is stopped by the browser; the block carries on", async ({ parent, stack, browserName }) => {
  const idle = Number(process.env.S27_IDLE_MS ?? 45_000);
  test.setTimeout(idle + 60_000);
  const b = await openBlock(parent, stack);
  const today = await openDirect(parent, stack);
  await openEcho(b);
  const before = await workerStats(b.frame);
  // Nothing for a while: no requests, no messages to the worker.
  await parent.waitForTimeout(idle);
  const first = await get(b);
  expect(first.status).toBe(200);
  const after = await workerStats(b.frame);
  // The shim's socket kept going through the quiet (its own channel).
  expect(await say(b, "after a nap")).toBe("after a nap");
  await record("idle-worker", browserName, {
    idleMs: idle,
    workerRestarted: after.started !== before.started,
    workerReconnects: after.connects - (after.started === before.started ? before.connects : 0),
    firstRequestMs: first.ms,
    secondRequestMs: (await get(b)).ms,
    todayFirstRequestMs: (await get(today)).ms,
  });
});

test("a block left open: grants expire and are renewed through the parent", async ({ parent, stack, browserName }) => {
  test.setTimeout(120_000);
  const ttlMs = 3000;
  const b = await openBlock(parent, stack, { ttlMs });
  const mints = () => parent.evaluate((o) => (window as any).s27.blocks.find((x: any) => x.origin === o).mints, b.origin);
  const m0 = await mints();
  const cycles = [];
  for (let i = 0; i < 4; i++) {
    await openEcho(b);
    await parent.waitForTimeout(ttlMs + 1000);
    // Open channels outlive their grant: requests and the socket carry on.
    expect((await get(b)).status).toBe(200);
    expect(await say(b, `cycle ${i}`)).toBe(`cycle ${i}`);
    // The daemon drops every channel (a restart): new ones need a new grant.
    await admin(stack, "/drop", {});
    const r = await get(b);
    expect(r.status).toBe(200);
    await expect.poll(() => say(b, "x"), { timeout: 5000 }).toMatch(/^closed:/);
    cycles.push(r.ms);
  }
  const renewals = (await mints()) - m0;
  expect(renewals).toBeGreaterThanOrEqual(4);
  expect(((await admin(stack, "/stats")).refusals as string[]).some((r) => r.includes("expired"))).toBe(true);
  await record("long-lived", browserName, { ttlMs, renewals, firstRequestAfterDropMs: cycles });
});

test("control restarts under an open block: the daemon redials, the block reconnects", async ({ parent, stack, browserName }) => {
  const b = await openBlock(parent, stack);
  await openEcho(b);
  const t = Date.now();
  await stack.restartControl();
  await expect.poll(async () => (await controlStats(stack)).daemons, { timeout: 10_000 }).toContain(stack.daemon.id);
  const daemonBack = Date.now() - t;
  // The socket through the old relay is gone; the app sees a close, as it
  // would for its dev server restarting, and a new socket works.
  expect(await say(b, "x")).toMatch(/^closed:/);
  await openEcho(b);
  expect(await say(b, "back")).toBe("back");
  expect((await get(b)).status).toBe(200);
  await record("control-restart", browserName, { daemonBackMs: daemonBack, blockBackMs: Date.now() - t });
});

test("a parent page whose clock is a day behind mints grants the daemon calls expired", async ({ parent, stack, browserName }) => {
  await parent.clock.setSystemTime(Date.now() - 25 * 3600_000);
  await parent.evaluate(() => 0);
  const s0 = await admin(stack, "/stats");
  await parent.evaluate((a) => (window as any).s27.openBlock(a), { daemon: stack.daemon, block: "skewed", ttlMs: 3600_000 });
  await admin(stack, "/block", { id: "skewed", port: stack.site });
  await expect
    .poll(async () => ((await admin(stack, "/stats")).refusals as string[]).slice(s0.refusals.length).join("\n"), { timeout: 30_000 })
    .toContain("expired");
  await record("clock-skew", browserName, { parentBehindHours: 25, loads: false });
});
