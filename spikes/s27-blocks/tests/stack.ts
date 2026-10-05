// The spike's stack for one worker: control, a daemon dialed in to its
// relay, and a plain test server standing in for a block's port. Tests get
// control's page (`parent`) with its device key trusted by the daemon.

import { test as base, expect, type Frame, type Page } from "@playwright/test";

export { admin, controlStats, freePort, MARKER, type Stack } from "./servers.ts";
import { admin, type Stack, startStack } from "./servers.ts";

let nextBlock = 1;

/** The daemon trusts this control page's device key (as enrollment would). */
export async function trustParent(page: Page, stack: Stack) {
  await page.waitForFunction(() => (window as any).s27?.ready);
  const sign = await page.evaluate(() => (window as any).s27.signPub as string);
  await admin(stack, "/trust", { sign });
}

export interface Opened {
  origin: string;
  frame: Frame;
  block: string;
}

/** Open a block on `port` in control's page; resolves once the page is
 * there through the worker (its <h1>, or `ready`). */
export async function openBlock(
  page: Page,
  stack: Stack,
  o: { port?: number; ttlMs?: number; route?: string; path?: string; ready?: string; key?: string; block?: string } = {},
): Promise<Opened> {
  const block = o.block ?? `blk${nextBlock++}`;
  await admin(stack, "/block", { id: block, port: o.port ?? stack.site });
  const origin = await page.evaluate(
    (a) => (window as any).s27.openBlock(a).origin as string,
    { daemon: stack.daemon, block, ttlMs: o.ttlMs, route: o.route, path: o.path, key: o.key },
  );
  const frame = await frameAt(page, origin);
  await frame.locator(o.ready ?? "h1").first().waitFor({ timeout: 60_000 });
  return { origin, frame, block };
}

export async function frameAt(page: Page, origin: string): Promise<Frame> {
  await expect.poll(() => page.frames().some((f) => f.url().startsWith(origin)), { timeout: 30_000 }).toBe(true);
  return page.frames().find((f) => f.url().startsWith(origin))!;
}

/** The worker's own counters, asked from a page it controls. */
export function workerStats(frame: Frame): Promise<any> {
  return frame.evaluate(
    () =>
      new Promise((res) => {
        const ch = new MessageChannel();
        ch.port1.onmessage = (e) => res(e.data);
        navigator.serviceWorker.controller!.postMessage({ s27: "stats" }, [ch.port2]);
      }),
  );
}

export const test = base.extend<{ parent: Page }, { stack: Stack }>({
  stack: [
    async ({}, use) => {
      const stack = await startStack();
      await use(stack);
      stack.stop();
    },
    { scope: "worker" },
  ],
  parent: async ({ page, stack }, use) => {
    await page.goto(stack.origin + "/");
    await trustParent(page, stack);
    await use(page);
  },
});

export { expect };

/** Keep a measurement (with the machine's load, which skews timings) in
 * .run/results.jsonl, and print it. */
export async function record(what: string, browser: string, data: Record<string, unknown>) {
  const { appendFileSync, mkdirSync } = await import("node:fs");
  const os = await import("node:os");
  const line = { what, browser, platform: process.platform, at: new Date().toISOString(), load: os.loadavg()[0].toFixed(1), cores: os.cpus().length, ...data };
  mkdirSync(".run", { recursive: true });
  appendFileSync(".run/results.jsonl", JSON.stringify(line) + "\n");
  console.log(JSON.stringify(line));
}

/** Open today's path for comparison: the daemon's own block site, framed
 * by control's page, no worker. */
export async function openDirect(page: Page, stack: Stack, o: { port?: number; ready?: string; path?: string } = {}): Promise<Opened> {
  const block = `blk${nextBlock++}`;
  await admin(stack, "/block", { id: block, port: o.port ?? stack.site });
  await page.evaluate((a) => (window as any).s27.openDirect(a.block, a.port, a.path), { block, port: stack.directPort, path: o.path });
  const origin = `https://b-${block}.direct.test:${stack.directPort}`;
  const frame = await frameAt(page, origin);
  await frame.locator(o.ready ?? "h1").first().waitFor({ timeout: 60_000 });
  return { origin, frame, block };
}
