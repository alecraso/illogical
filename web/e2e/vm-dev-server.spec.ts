// M6a's "done when", on a real VM: in a VM tab, `npm run dev` runs in one
// block and its app in a browser block beside it; hot reload works on the
// desktop and a phone; a script in the app can't reach Arugula; closing
// the tab deletes the machine and both blocks. Needs wispd and its token,
// and the internet in the VM (Node from nodejs.org, Vite from npm);
// elsewhere these skip.

import { spawn, type ChildProcess } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";
import { devices, expect, test, type Frame, type Page } from "@playwright/test";
import { menu, open, paneEl, text, type as typeIn, closeContexts } from "./helpers";
import type { PaneId } from "../src/proto";
import { ANY, blockPort, daemonPort } from "./ports";
import { labs } from "./labs";

test.afterAll(closeContexts);

let BLOCKS = 0;
let APP = "";
const NODE = "v22.20.0";
const WISP = process.env.ARUGULA_WISP_URL ?? "http://127.0.0.1:7788";
const token = (() => {
  try {
    return readFileSync(process.env.ARUGULA_WISP_TOKEN_FILE ?? `${homedir()}/.local/share/wisp/token`, "utf8").trim();
  } catch {
    return "";
  }
})();
let state = "";
test.use({ baseURL: async ({}, use) => use(APP) });
test.describe.configure({ mode: "serial" });

const wisp = (method: string, path: string, body?: string) =>
  fetch(`${WISP}/v1/sprites${path}`, { method, headers: { Authorization: `Bearer ${token}` }, body });
/** Write a file in the VM, as an editor (or an agent) would. */
async function put(sprite: string, path: string, content: string) {
  const r = await wisp("PUT", `/${sprite}/fs/write?path=${encodeURIComponent(path)}&mkdirParents=true`, content);
  if (!r.ok) throw new Error(`write ${path}: ${r.status} ${await r.text()}`);
}

const app: Record<string, string> = {
  "package.json": JSON.stringify({ name: "app", private: true, type: "module", scripts: { dev: "vite" } }),
  "index.html": `<!doctype html><html><head><title>VM app</title><meta name="viewport" content="width=device-width"></head>
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

let daemon: ChildProcess | undefined;
test.beforeAll(async () => {
  if (!token) return;
  state = mkdtempSync(join(tmpdir(), "ilg-e2e-vmdev-"));
  daemon = spawn(
    "../target/debug/arugulad",
    ["--listen", ANY, "--block-listen", ANY, "--shell", "bash --norc --noprofile"],
    { stdio: "ignore", env: { ...process.env, ARUGULA_STATE_DIR: labs(state), ARUGULA_WISP_URL: WISP } },
  );
  APP = `http://127.0.0.1:${await daemonPort(state, daemon)}`;
  BLOCKS = await blockPort(state, daemon);
  for (let i = 0; i < 100; i++) {
    try {
      if ((await fetch(`${APP}/`)).ok) return;
    } catch {
      // not up yet
    }
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error("daemon did not start");
});
test.afterAll(async () => {
  daemon?.kill("SIGKILL");
  // Anything a failed test left behind: only this daemon's machines.
  if (!state) return;
  let id: string | null = null;
  try {
    id = readFileSync(join(state, "daemon-id"), "utf8").trim();
  } catch {
    // never started
  }
  if (id && token) {
    const list = await (await wisp("GET", `?prefix=illogical-eph-${id}-`)).json();
    for (const s of list.sprites ?? []) await wisp("DELETE", `/${s.name}`);
  }
  rmSync(state, { recursive: true, force: true });
});

const panesOf = (page: Page) => page.evaluate(() => window.__arugula.client.state!.panes);
const machines = (page: Page) => page.evaluate(() => window.__arugula.client.state!.machines);

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

let sprite = "";
let term: PaneId = 0;
let block: PaneId = 0;
let tab = 0;

test("npm run dev in a VM tab, its app beside it, hot reload, no way into Arugula", async ({ page }) => {
  test.skip(!token, "no wisp token on this host");
  test.setTimeout(300_000);
  await open(page);
  await page.evaluate(() => window.__arugula.client.newVm({ session: window.__arugula.client.session!, tab: true }));
  await expect.poll(async () => (await machines(page)).length).toBe(1);
  const [m] = await machines(page);
  sprite = m.sprite;
  await expect.poll(async () => (await panesOf(page)).find((p) => p.host === m.id)?.id ?? null).not.toBeNull();
  term = (await panesOf(page)).find((p) => p.host === m.id)!.id;
  tab = await page.evaluate((p) => window.__arugula.client.tabOfPane(p)!.id, term);
  await expect.poll(() => page.evaluate((p) => window.__arugula.client.info(p)?.cwd ?? null, term), { timeout: 60_000 }).toBe("/home/sprite");

  // The app's source, then Node, Vite and `npm run dev` in the VM tab's terminal.
  for (const [name, content] of Object.entries(app)) await put(sprite, `/home/sprite/app/${name}`, content);
  await typeIn(
    page,
    term,
    `curl -fsSL https://nodejs.org/dist/${NODE}/node-${NODE}-linux-x64.tar.xz | tar -xJ -C ~ && export PATH=~/node-${NODE}-linux-x64/bin:$PATH && cd ~/app && npm i --no-audit --no-fund vite && npm run dev -- --port 5173 --strictPort\n`,
  );
  await expect.poll(() => text(page, term), { timeout: 180_000 }).toMatch(/ready in/);

  // Its app in a browser block beside it, on the tab's machine.
  const others = (await panesOf(page)).map((p) => p.id);
  await menu(page, paneEl(page, term), "Open a port on this machine…");
  await page.locator(".prompt input").fill("5173");
  await page.keyboard.press("Enter");
  await expect.poll(async () => (await panesOf(page)).find((p) => !others.includes(p.id))?.id ?? null).not.toBeNull();
  block = (await panesOf(page)).find((p) => !others.includes(p.id))!.id;
  expect((await panesOf(page)).find((p) => p.id === block)!.host).toBe(m.id);
  expect(await page.evaluate((b) => window.__arugula.client.tabOfPane(b)!.id, block)).toBe(tab);
  const view = page.frameLocator(`[data-pane="${block}"] iframe`);
  await expect(view.locator("#h")).toHaveText("hello", { timeout: 30_000 });
  await expect(paneEl(page, block).locator(".host-tag")).toHaveText("VM");

  const frame = await frameOf(page);
  expect(await frame.evaluate(() => location.origin)).toMatch(new RegExp(`^http://b-${block}-[a-z0-9]{20}\\.localhost:${BLOCKS}$`));

  // Hot reload, through the Sprites proxy: no full reload.
  await frame.evaluate(() => ((window as unknown as { marker: number }).marker = 42));
  await put(sprite, "/home/sprite/app/label.js", `export const label = "hot";`);
  await expect(view.locator("#h")).toHaveText("hot", { timeout: 15_000 });
  await put(sprite, "/home/sprite/app/style.css", "h1 { color: rgb(0, 0, 255); }");
  await expect.poll(() => frame.evaluate(() => getComputedStyle(document.querySelector("#h")!).color)).toBe("rgb(0, 0, 255)");
  expect(await frame.evaluate(() => (window as unknown as { marker?: number }).marker)).toBe(42);

  // The app can't reach arugula.
  const before = (await panesOf(page)).length;
  const tried = await frame.evaluate(async (app) => {
    const out: Record<string, string> = {};
    try {
      await fetch(`${app}/api/panes`);
      out.read = "read";
    } catch {
      out.read = "blocked";
    }
    try {
      const body = JSON.stringify({ command: "true" });
      out.run = String((await fetch(`${app}/api/run`, { method: "POST", headers: { "Content-Type": "application/json" }, body })).status);
    } catch {
      out.run = "blocked";
    }
    try {
      await fetch(`${app}/api/run`, { method: "POST", mode: "no-cors", body: JSON.stringify({ command: "true" }) });
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
  }, APP);
  expect(tried).toEqual({ read: "blocked", run: "blocked", ws: "refused", parent: "blocked" });
  await page.waitForTimeout(500);
  expect((await panesOf(page)).length).toBe(before);
});

test.describe("phone", () => {
  const { viewport, userAgent, deviceScaleFactor, isMobile, hasTouch } = devices["Pixel 7"];
  test.use({ viewport, userAgent, deviceScaleFactor, isMobile, hasTouch });

  test("the VM's app on a phone: taps and hot reload", async ({ page }) => {
    test.skip(!token || !block, "no VM app (no wisp token, or the desktop test failed)");
    test.setTimeout(60_000);
    await open(page);
    await page.evaluate(
      ({ tab, block }) => {
        const c = window.__arugula.client;
        c.selectTab(tab);
        c.setActive(block);
      },
      { tab, block },
    );
    const view = page.frameLocator(`[data-pane="${block}"] iframe`);
    await expect(view.locator("#h")).toHaveText("hot", { timeout: 15_000 });
    expect((await paneEl(page, block).locator("iframe").boundingBox())!.width).toBeGreaterThan(380);
    await view.locator("#b").tap();
    await view.locator("#b").tap();
    await expect(view.locator("#b")).toHaveText("2");
    await put(sprite, "/home/sprite/app/label.js", `export const label = "phone";`);
    await expect(view.locator("#h")).toHaveText("phone", { timeout: 15_000 });
    await expect(view.locator("#b")).toHaveText("2");
  });
});

test("closing the tab deletes the machine and both blocks", async ({ page }) => {
  test.skip(!token || !block, "no VM app (no wisp token, or the desktop test failed)");
  test.setTimeout(60_000);
  await open(page);
  const url = (await page.evaluate((b) => window.__arugula.client.blocks.get(b)?.state, block)) as { url: string };
  await page.evaluate((tab) => window.__arugula.client.intent({ op: "close_tab", tab }), tab);
  await expect.poll(async () => (await panesOf(page)).filter((p) => p.id === term || p.id === block).length).toBe(0);
  await expect.poll(async () => (await machines(page)).length).toBe(0);
  await expect.poll(async () => (await wisp("GET", `/${sprite}`)).status, { timeout: 30_000 }).toBe(404);
  expect((await page.request.get(url.url)).status()).toBe(404);
});
