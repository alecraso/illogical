import { expect, type Locator, type Page } from "@playwright/test";
import type { Client } from "../src/client";
import type { HostDirectory } from "../src/hosts";
import type { ControlSession } from "../src/control";
import type { PaneId, TabView } from "../src/proto";

declare global {
  interface Window {
    __illogical: {
      client: Client;
      hosts: HostDirectory;
      control: ControlSession | null;
      text(pane: PaneId): string;
      screen(pane: PaneId): string;
      size(pane: PaneId): [number, number] | null;
      offset(pane: PaneId): number | null;
      selection(pane: PaneId): string;
    };
  }
}

export async function open(page: Page) {
  await page.goto("/");
  await expect.poll(() => page.evaluate(() => window.__illogical?.client.connected)).toBe(true);
  await expect.poll(() => page.evaluate(() => window.__illogical.client.state !== null)).toBe(true);
}

/** Back to one session with one fresh pane, whatever earlier tests left. */
export async function reset(page: Page) {
  await open(page);
  const old = await page.evaluate(() => {
    const c = window.__illogical.client;
    const before = c.state!.panes.map((p) => p.id);
    for (const s of c.state!.sessions) c.intent({ op: "close_session", session: s.id });
    c.intent({ op: "new_session", name: null, from_pane: null });
    return before;
  });
  // Wait for the new session's pane, not a stale view of an old one.
  const fresh = () =>
    page.evaluate((old) => {
      const ids = window.__illogical.client.state!.panes.map((p) => p.id);
      return ids.length === 1 && !old.includes(ids[0]) ? ids[0] : null;
    }, old);
  await expect.poll(fresh).not.toBeNull();
  await expect.poll(() => panes(page)).toEqual([expect.any(Number)]);
  await ready(page, (await fresh())!);
}

/** Wait until a pane has drawn something (its snapshot arrived). */
export async function ready(page: Page, pane: PaneId) {
  try {
    await expect.poll(() => page.evaluate((p) => window.__illogical.offset(p), pane)).not.toBeNull();
  } catch (e) {
    console.log(
      "NOT READY",
      pane,
      await page.evaluate((p) => {
        const c = window.__illogical.client;
        const e = c.panes.get(p);
        return JSON.stringify({ me: c.clientId, connected: c.connected, has: !!e, offset: e?.offset, epoch: e?.epoch, info: c.state?.panes.find((x) => x.id === p), text: e?.view.text().slice(0, 80) });
      }, pane),
    );
    throw e;
  }
}

/** Panes of the shown tab, in layout order. */
export const panes = (page: Page) =>
  page.evaluate(() => window.__illogical.client.tabView()?.layout.panes.map(([id]) => id) ?? []);
export const tab = (page: Page) => page.evaluate(() => window.__illogical.client.tabView() as TabView);
export const active = (page: Page) => page.evaluate(() => window.__illogical.client.active()!);
export const text = (page: Page, pane: PaneId) => page.evaluate((p) => window.__illogical.text(p), pane);
export const screen = (page: Page, pane: PaneId) =>
  page.evaluate((p) => window.__illogical.screen(p).split("\n").map((l) => l.trimEnd()).join("\n"), pane);
export const size = (page: Page, pane: PaneId) => page.evaluate((p) => window.__illogical.size(p), pane);
export const tabsInSession = (page: Page) =>
  page.evaluate(() => {
    const c = window.__illogical.client;
    return c.state!.sessions.find((s) => s.id === c.session)!.tabs;
  });

export const paneEl = (page: Page, pane: PaneId): Locator => page.locator(`[data-pane="${pane}"]`);

/** Type into a pane (clicking it first focuses it). */
export async function type(page: Page, pane: PaneId, s: string) {
  await paneEl(page, pane).click({ position: { x: 40, y: 40 } });
  await page.keyboard.type(s, { delay: 2 });
}

/** Run a command in a pane and wait for a marker only its output contains. */
export async function run(page: Page, pane: PaneId, cmd: string, marker: string) {
  await type(page, pane, `${cmd}\n`);
  await expect.poll(() => text(page, pane)).toContain(marker);
}

export async function menu(page: Page, target: Locator, item: string) {
  await target.click({ button: "right", position: { x: 60, y: 60 } });
  await page.getByRole("menuitem", { name: item }).click();
}

/** Drag with real pointer events, in steps, like a hand would. */
export async function dragTo(page: Page, from: Locator, to: { x: number; y: number }) {
  const b = (await from.boundingBox())!;
  await page.mouse.move(b.x + b.width / 2, b.y + b.height / 2);
  await page.mouse.down();
  await page.mouse.move(b.x + b.width / 2 + 10, b.y + b.height / 2 + 10, { steps: 3 });
  await page.mouse.move(to.x, to.y, { steps: 12 });
  await page.mouse.up();
}

/** A point inside an element, as fractions of its box. */
export async function at(el: Locator, fx: number, fy: number) {
  const b = (await el.boundingBox())!;
  return { x: b.x + b.width * fx, y: b.y + b.height * fy };
}
