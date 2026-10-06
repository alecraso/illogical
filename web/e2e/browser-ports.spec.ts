// M6a: browser blocks on a port, in the dev scheme (each block is
// `http://b-<id>-<key>.localhost:<port>` on a loopback listener). A real
// Vite dev server on this host shows in a block beside a terminal; hot
// reload works on the desktop and a phone; the page has its own origin and
// a script in it can't reach illogical; a server that dies is noticed, and
// the page comes back with it.

import { spawn, type ChildProcess } from "node:child_process";
import { existsSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import type { AddressInfo } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { devices, expect, test, type Frame, type Page } from "@playwright/test";
import { createServer, type ViteDevServer } from "vite";
import { menu, open, paneEl, panes, reset, closeContexts } from "./helpers";
import type { PaneId } from "../src/proto";
import { ANY, blockPort, daemonPort } from "./ports";
import { labs } from "./labs";

test.afterAll(closeContexts);

let BLOCKS = 0;
let VITE = 0;
let APP = "";
test.use({ baseURL: async ({}, use) => use(APP) });
test.describe.configure({ mode: "serial" });

let state = "";
let project = "";
let PWNED = "";

const files: Record<string, string> = {
  "index.html": `<!doctype html><html><head><title>Dev app</title><meta name="viewport" content="width=device-width"></head>
<body><h1 id="h">…</h1><button id="b">0</button><script type="module" src="/main.js"></script></body></html>`,
  "style.css": "h1 { color: rgb(255, 0, 0); }",
  "label.js": `export const label = "hello";`,
  "main.js": `import "./style.css";
import { label } from "./label.js";
const h = document.querySelector("#h");
h.textContent = label;
const b = document.querySelector("#b");
b.addEventListener("click", () => (b.textContent = String(Number(b.textContent) + 1)));
if (import.meta.hot) import.meta.hot.accept("./label.js", (m) => (h.textContent = m.label));`,
};
const write = (name: string, text: string) => writeFileSync(join(project, name), text);

let daemon: ChildProcess | undefined;
let vite: ViteDevServer | undefined;

async function startVite() {
  // Vite's defaults: it listens on "localhost", which may be ::1 only.
  // On a port of its choosing, then on the same one again.
  vite = await createServer({ root: project, configFile: false, logLevel: "silent", server: { port: VITE, strictPort: true } });
  await vite.listen();
  VITE ||= (vite.httpServer!.address() as AddressInfo).port;
}

test.beforeAll(async () => {
  state = mkdtempSync(join(tmpdir(), "ilg-e2e-ports-"));
  project = mkdtempSync(join(tmpdir(), "ilg-e2e-vite-"));
  PWNED = join(state, "pwned");
  for (const [name, text] of Object.entries(files)) write(name, text);
  daemon = spawn(
    "../target/debug/illogicald",
    ["--listen", ANY, "--block-listen", ANY, "--shell", "bash --norc --noprofile"],
    { stdio: "ignore", env: { ...process.env, ILLOGICAL_STATE_DIR: labs(state), ILLOGICAL_WISP_TOKEN_FILE: "/nonexistent" } },
  );
  APP = `http://127.0.0.1:${await daemonPort(state, daemon)}`;
  BLOCKS = await blockPort(state, daemon);
  for (let i = 0; i < 100; i++) {
    try {
      if ((await fetch(`${APP}/`)).ok) break;
    } catch {
      // not up yet
    }
    await new Promise((r) => setTimeout(r, 100));
  }
  await startVite();
});

test.afterAll(async () => {
  await vite?.close();
  daemon?.kill("SIGKILL");
  for (const d of [state, project]) if (d) rmSync(d, { recursive: true, force: true });
});

/** The block's frame, once it has loaded from the block's own name. */
async function frameOf(page: Page): Promise<Frame> {
  let frame: Frame | undefined;
  await expect
    .poll(() => {
      frame = page.frames().find((f) => new RegExp(`^http://b-\\d+-[a-z0-9]{20}\\.localhost:${BLOCKS}/`).test(f.url()));
      return frame?.url() ?? null;
    })
    .not.toBeNull();
  return frame!;
}

const blockState = (page: Page, b: PaneId) => page.evaluate((b) => window.__illogical.client.blocks.get(b)?.state as Record<string, unknown> | null, b);
const attention = (page: Page, b: PaneId) => page.evaluate((b) => window.__illogical.client.info(b)?.attention, b);

let block: PaneId = 0;

test("a dev server beside a terminal: its own origin, hot reload, no way into illogical", async ({ page }) => {
  await reset(page);
  const [term] = await panes(page);
  await menu(page, paneEl(page, term), "Open a port…");
  await page.locator(".prompt input").fill(String(VITE));
  await page.keyboard.press("Enter");
  await expect.poll(async () => (await panes(page)).length).toBe(2);
  block = (await panes(page)).find((p) => p !== term)!;
  expect(await page.evaluate((b) => window.__illogical.client.info(b)?.type, block)).toBe("browser");

  const view = page.frameLocator(`[data-pane="${block}"] iframe`);
  await expect(view.locator("#h")).toHaveText("hello");
  await expect(paneEl(page, block).locator(".browser-url input")).toHaveValue(`:${VITE}/`);
  await expect.poll(() => attention(page, block)).toBe("idle");
  // Named after the page.
  await page.evaluate((b) => window.__illogical.client.setActive(b), block);
  await expect(page.locator(".tab.selected .tab-label")).toHaveText("Dev app");

  // Its own origin: not the app's, and the frame is sandboxed to it.
  const frame = await frameOf(page);
  const origin = await frame.evaluate(() => location.origin);
  expect(origin).toMatch(new RegExp(`^http://b-${block}-[a-z0-9]{20}\\.localhost:${BLOCKS}$`));
  expect(await paneEl(page, block).locator("iframe").getAttribute("sandbox")).toBe("allow-scripts allow-forms allow-same-origin");
  // Storage works there (allow-same-origin is the block's origin).
  expect(await frame.evaluate(() => (localStorage.setItem("k", "v"), localStorage.getItem("k")))).toBe("v");

  // Hot reload: JS and CSS change without reloading the page.
  await frame.evaluate(() => ((window as unknown as { marker: number }).marker = 42));
  write("label.js", `export const label = "hot";`);
  await expect(view.locator("#h")).toHaveText("hot");
  write("style.css", "h1 { color: rgb(0, 0, 255); }");
  await expect.poll(() => frame.evaluate(() => getComputedStyle(document.querySelector("#h")!).color)).toBe("rgb(0, 0, 255)");
  expect(await frame.evaluate(() => (window as unknown as { marker?: number }).marker)).toBe(42);

  // A script in the page can't reach illogical: not its API (reading or
  // writing, however it's dressed up), not its socket, not the app page.
  const before = await page.evaluate(() => window.__illogical.client.state!.panes.length);
  const tried = await frame.evaluate(
    async ({ app, pwned }) => {
      const out: Record<string, string> = {};
      const run = (more: RequestInit) =>
        fetch(`${app}/api/run`, { method: "POST", body: JSON.stringify({ command: `touch ${pwned}` }), ...more });
      try {
        await fetch(`${app}/api/panes`);
        out.read = "read";
      } catch {
        out.read = "blocked";
      }
      try {
        out.json = String((await run({ headers: { "Content-Type": "application/json" } })).status);
      } catch {
        out.json = "blocked";
      }
      try {
        // A "simple" request is sent without asking; the daemon must refuse it.
        await run({ mode: "no-cors", headers: { "Content-Type": "text/plain" } });
      } catch {
        // blocked by the browser: just as good
      }
      out.ws = await new Promise<string>((res) => {
        const ws = new WebSocket(`${app.replace("http", "ws")}/ws`);
        ws.onopen = () => res("open");
        ws.onerror = () => res("refused");
        setTimeout(() => res("timeout"), 5000);
      });
      try {
        out.parent = String(parent.document.title);
      } catch {
        out.parent = "blocked";
      }
      return out;
    },
    { app: APP, pwned: PWNED },
  );
  expect(tried).toEqual({ read: "blocked", json: "blocked", ws: "refused", parent: "blocked" });
  await page.waitForTimeout(500);
  expect(existsSync(PWNED)).toBe(false);
  expect(await page.evaluate(() => window.__illogical.client.state!.panes.length)).toBe(before);

  // The server dies: the block says so and asks for you...
  await vite!.close();
  vite = undefined;
  await expect(paneEl(page, block).locator(".browser-card")).toContainText(`Nothing is answering on :${VITE}`, { timeout: 15_000 });
  await expect.poll(() => attention(page, block)).toBe("needs_input");
  // ...and when it's back, so is the page.
  await startVite();
  await expect(view.locator("#h")).toHaveText("hot", { timeout: 15_000 });
  await expect.poll(() => attention(page, block)).toBe("idle");
  expect((await blockState(page, block))?.error).toBeNull();
});

test.describe("phone", () => {
  const { viewport, userAgent, deviceScaleFactor, isMobile, hasTouch } = devices["Pixel 7"];
  test.use({ viewport, userAgent, deviceScaleFactor, isMobile, hasTouch });

  test("a port opened from the sheet, with hot reload and taps", async ({ page }) => {
    await open(page);
    // The terminal's tab; the desktop test left the block beside it.
    await page.evaluate((b) => window.__illogical.client.intent({ op: "close_pane", pane: b }), block);
    await expect.poll(() => page.evaluate(() => window.__illogical.client.state!.panes.length)).toBe(1);
    await page.locator(".sheet-button").click();
    await page.getByRole("button", { name: "Open port" }).click();
    await page.locator(".prompt input").fill(`${VITE}/`);
    await page.keyboard.press("Enter");
    await expect.poll(() => page.evaluate(() => window.__illogical.client.state!.panes.length)).toBe(2);
    const b = await page.evaluate(() => window.__illogical.client.state!.panes.find((p) => p.type === "browser")!.id);
    await page.evaluate((b) => window.__illogical.client.setActive(b), b);
    const view = page.frameLocator(`[data-pane="${b}"] iframe`);
    await expect(view.locator("#h")).toHaveText("hot");
    // The frame fills the phone's width.
    expect((await paneEl(page, b).locator("iframe").boundingBox())!.width).toBeGreaterThan(380);

    await view.locator("#b").tap();
    await view.locator("#b").tap();
    await expect(view.locator("#b")).toHaveText("2");
    write("label.js", `export const label = "phone";`);
    await expect(view.locator("#h")).toHaveText("phone");
    // Hot, not reloaded: the taps are still counted.
    await expect(view.locator("#b")).toHaveText("2");

    // Closing it takes its site with it.
    const url = (await blockState(page, b))!.url as string;
    await page.evaluate((b) => window.__illogical.client.intent({ op: "close_pane", pane: b }), b);
    await expect.poll(() => page.evaluate(() => window.__illogical.client.state!.panes.length)).toBe(1);
    const res = await page.request.get(url);
    expect(res.status()).toBe(404);
  });
});
