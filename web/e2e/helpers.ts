import { mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, type Browser, type Locator, type Page } from "@playwright/test";
import type { Client } from "../src/client";
import type { HostDirectory } from "../src/hosts";
import type { ControlSession } from "../src/control";
import type { PaneId, TabView } from "../src/proto";

declare global {
  interface Window {
    __arugula: {
      client: Client;
      hosts: HostDirectory;
      remotes: { client(host: string): Client | undefined };
      control: ControlSession | null;
      fleet: import("../src/fleet").Fleet;
      huddle: import("../src/call").Huddle | null;
      summaries(): Client;
      text(pane: PaneId): string;
      screen(pane: PaneId): string;
      size(pane: PaneId): [number, number] | null;
      offset(pane: PaneId): number | null;
      selection(pane: PaneId): string;
    };
  }
}

/** The run shares one browser, and Playwright closes only the contexts of
 * its own `context` fixture: one a spec opens with `browser.newContext()`
 * (or a helper does for it) would keep its pages drawing, polling and
 * reconnecting through every later spec, which then run on a busier
 * machine than they do alone (#258). Every spec that uses `browser` closes
 * what's left when it ends: `test.afterAll(closeContexts)`. */
export async function closeContexts({ browser }: { browser: Browser }) {
  await Promise.all(browser.contexts().map((c) => c.close()));
}

export async function open(page: Page) {
  await page.goto("/");
  await expect.poll(() => page.evaluate(() => window.__arugula?.client.connected)).toBe(true);
  await expect.poll(() => page.evaluate(() => window.__arugula.client.state !== null)).toBe(true);
}

/** Back to one session with one fresh pane, whatever earlier tests left. */
export async function reset(page: Page) {
  await open(page);
  const old = await page.evaluate(() => {
    const c = window.__arugula.client;
    const before = c.state!.panes.map((p) => p.id);
    for (const s of c.state!.sessions) c.intent({ op: "close_session", session: s.id });
    c.intent({ op: "new_session", name: null, from_pane: null });
    return before;
  });
  // Wait for the new session's pane, not a stale view of an old one.
  const fresh = () =>
    page.evaluate((old) => {
      const ids = window.__arugula.client.state!.panes.map((p) => p.id);
      return ids.length === 1 && !old.includes(ids[0]) ? ids[0] : null;
    }, old);
  await expect.poll(fresh).not.toBeNull();
  await expect.poll(() => panes(page)).toEqual([expect.any(Number)]);
  await ready(page, (await fresh())!);
}

/** Wait until a pane has drawn something (its snapshot arrived). */
export async function ready(page: Page, pane: PaneId) {
  try {
    await expect.poll(() => page.evaluate((p) => window.__arugula.offset(p), pane)).not.toBeNull();
  } catch (e) {
    console.log(
      "NOT READY",
      pane,
      await page.evaluate((p) => {
        const c = window.__arugula.client;
        const e = c.panes.get(p);
        return JSON.stringify({ me: c.clientId, connected: c.connected, has: !!e, offset: e?.offset, epoch: e?.epoch, info: c.state?.panes.find((x) => x.id === p), text: e?.view.text().slice(0, 80) });
      }, pane),
    );
    throw e;
  }
}

/** Panes of the shown tab, in layout order. */
export const panes = (page: Page) =>
  page.evaluate(() => window.__arugula.client.tabView()?.layout.panes.map(([id]) => id) ?? []);
export const tab = (page: Page) => page.evaluate(() => window.__arugula.client.tabView() as TabView);
export const active = (page: Page) => page.evaluate(() => window.__arugula.client.active()!);
export const text = (page: Page, pane: PaneId) => page.evaluate((p) => window.__arugula.text(p), pane);
export const screen = (page: Page, pane: PaneId) =>
  page.evaluate((p) => window.__arugula.screen(p).split("\n").map((l) => l.trimEnd()).join("\n"), pane);
export const size = (page: Page, pane: PaneId) => page.evaluate((p) => window.__arugula.size(p), pane);
export const tabsInSession = (page: Page) =>
  page.evaluate(() => {
    const c = window.__arugula.client;
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

/** A seeded line's common fields: its type, uuid and parent. */
export type Base = (type: string, uuid: string, parentUuid: string | null) => Record<string, unknown>;

/** M33: a Claude Code terminal session in `claude` (a Claude directory),
 * in a folder of its own: "remember WORD", a reply, a command and its
 * output, then `more`. Its id and folder. */
export function seedConversation(claude: string, word: string, title: string, more: (base: Base) => object[] = () => []): { id: string; cwd: string } {
  const cwd = mkdtempSync(join(tmpdir(), `arugula-e2e-conv-${word}-`));
  const id = crypto.randomUUID();
  const dir = join(claude, "projects", cwd.replace(/[/.]/g, "-"));
  mkdirSync(dir, { recursive: true });
  const base: Base = (type, uuid, parentUuid) => ({
    type, uuid, parentUuid, sessionId: id, cwd, gitBranch: "main", entrypoint: "cli", version: "2.1.288",
    timestamp: new Date().toISOString(), isSidechain: false,
  });
  const lines = [
    { ...base("user", "u1", null), message: { role: "user", content: `remember ${word}` } },
    { ...base("assistant", "a1", "u1"), message: { id: "m1", role: "assistant", model: "claude-haiku-4-5-20251001", content: [{ type: "text", text: "Noted." }] } },
    { ...base("assistant", "a2", "a1"), message: { id: "m1", role: "assistant", model: "claude-haiku-4-5-20251001", content: [{ type: "tool_use", id: `toolu_${word}`, name: "Bash", input: { command: `echo ${word}` } }] } },
    { ...base("user", "u2", "a2"), message: { role: "user", content: [{ type: "tool_result", tool_use_id: `toolu_${word}`, content: word }] } },
    ...more(base),
    { type: "ai-title", aiTitle: title, sessionId: id },
  ];
  writeFileSync(join(dir, `${id}.jsonl`), lines.map((l) => JSON.stringify(l)).join("\n") + "\n");
  return { id, cwd };
}

/** Opens one of the control overlay's panels ("devices", "account",
 * "teams", ...) once the overlay listens for the event: sent before that,
 * it's lost. Checking and sending in one evaluate means the overlay can't
 * unmount in between. */
export const controlPanel = (page: Page, panel: string) =>
  page.waitForFunction(
    (p) => {
      if (!document.documentElement.hasAttribute("data-control-panels")) return false;
      dispatchEvent(new CustomEvent("arugula:control-panel", { detail: p }));
      return true;
    },
    panel,
    { timeout: 20_000 },
  );

/** M70: paste a file onto a pane's terminal, as a browser does when an
 * image is on the clipboard. */
export async function pasteFile(page: Page, pane: PaneId, bytes: Buffer, name: string, type: string) {
  await paneEl(page, pane)
    .locator(".term-host")
    .first()
    .evaluate(
      (host, [b64, name, type]) => {
        const data = Uint8Array.from(atob(b64), (c) => c.charCodeAt(0));
        const dt = new DataTransfer();
        dt.items.add(new File([data], name, { type }));
        host.dispatchEvent(new ClipboardEvent("paste", { clipboardData: dt, bubbles: true, cancelable: true }));
      },
      [bytes.toString("base64"), name, type] as const,
    );
}

/** M70: the path of an upload as it shows on a pane's screen once pasted
 * (wrapped lines joined). */
export async function uploadedPath(page: Page, pane: PaneId, ext = "png"): Promise<string> {
  const re = new RegExp(`/\\S*?arugula-uploads/\\d+/[0-9a-f]{16}\\.${ext}`);
  let path = "";
  await expect
    .poll(async () => {
      path = re.exec((await text(page, pane)).replace(/\n/g, ""))?.[0] ?? "";
      return path;
    })
    .not.toBe("");
  return path;
}
