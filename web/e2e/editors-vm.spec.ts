// M27 on a VM: "Open in editor" on a VM tab's terminal runs code-server in
// the VM (a sprite service, which downloads the release there the first
// time) and shows it in a block beside the tab's other blocks, through the
// Sprites proxy; `arugula edit --machine mN FILE:LINE` opens a file there.
// Needs wispd and its token, and the internet in the VM; elsewhere these
// skip.

import { execFile, spawn, type ChildProcess } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { promisify } from "node:util";
import { expect, test, type Page } from "@playwright/test";
import { menu, open, paneEl } from "./helpers";
import type { PaneId } from "../src/proto";
import { ANY, blockPort, daemonPort } from "./ports";
import { labs } from "./labs";

let PORT = 0;
let BLOCKS = 0;
let APP = "";
const WISP = process.env.ARUGULA_WISP_URL ?? "http://127.0.0.1:7788";
const token = (() => {
  try {
    return readFileSync(process.env.ARUGULA_WISP_TOKEN_FILE ?? `${homedir()}/.local/share/wisp/token`, "utf8").trim();
  } catch {
    return "";
  }
})();
test.use({ baseURL: async ({}, use) => use(APP) });
test.describe.configure({ mode: "serial" });

const wisp = (method: string, path: string, body?: string) =>
  fetch(`${WISP}/v1/sprites${path}`, { method, headers: { Authorization: `Bearer ${token}` }, body });
async function put(sprite: string, path: string, content: string) {
  const r = await wisp("PUT", `/${sprite}/fs/write?path=${encodeURIComponent(path)}&mkdirParents=true`, content);
  if (!r.ok) throw new Error(`write ${path}: ${r.status} ${await r.text()}`);
}

let state = "";
let daemon: ChildProcess | undefined;
test.beforeAll(async () => {
  if (!token) return;
  state = mkdtempSync(join(tmpdir(), "ilg-e2e-edvm-"));
  daemon = spawn(
    "../target/debug/arugulad",
    ["--listen", ANY, "--block-listen", ANY, "--shell", "bash --norc --noprofile"],
    { stdio: "ignore", env: { ...process.env, ARUGULA_STATE_DIR: labs(state), ARUGULA_WISP_URL: WISP } },
  );
  PORT = await daemonPort(state, daemon);
  BLOCKS = await blockPort(state, daemon);
  APP = `http://127.0.0.1:${PORT}`;
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
  if (!state) return;
  let id: string | null = null;
  try {
    id = readFileSync(join(state, "daemon-id"), "utf8").trim();
  } catch {
    // never started
  }
  if (id && token && !process.env.KEEP_VM) {
    const list = await (await wisp("GET", `?prefix=illogical-eph-${id}-`)).json();
    for (const s of list.sprites ?? []) await wisp("DELETE", `/${s.name}`);
  }
  rmSync(state, { recursive: true, force: true });
});

const panesOf = (page: Page) => page.evaluate(() => window.__arugula.client.state!.panes);

test("VS Code in a VM tab, and a file there at its line", async ({ page }) => {
  test.skip(!token, "no wisp token on this host");
  test.setTimeout(600_000);
  await open(page);
  await page.evaluate(() => window.__arugula.client.newVm({ session: window.__arugula.client.session!, tab: true }));
  await expect.poll(() => page.evaluate(() => window.__arugula.client.state!.machines.length)).toBe(1);
  const m = await page.evaluate(() => window.__arugula.client.state!.machines[0]);
  await expect.poll(async () => (await panesOf(page)).find((p) => p.host === m.id)?.id ?? null).not.toBeNull();
  const term: PaneId = (await panesOf(page)).find((p) => p.host === m.id)!.id;
  await expect.poll(() => page.evaluate((p) => window.__arugula.client.info(p)?.cwd ?? null, term), { timeout: 60_000 }).toBe("/home/sprite");
  await put(m.sprite, "/home/sprite/proj/.git/HEAD", "ref: refs/heads/main\n");
  await put(m.sprite, "/home/sprite/proj/main.rs", "fn main() {\n    println!(\"hello from a sprite\");\n}\n");

  // From the terminal's menu: on the tab's machine, in its directory.
  const others = (await panesOf(page)).map((p) => p.id);
  await menu(page, paneEl(page, term), "Open in editor");
  await expect.poll(async () => (await panesOf(page)).find((p) => !others.includes(p.id))?.id ?? null).not.toBeNull();
  const block = (await panesOf(page)).find((p) => !others.includes(p.id))!;
  expect(block.type).toBe("editor");
  expect(block.host).toBe(m.id);
  await page.evaluate((b) => window.__arugula.client.setActive(b), block.id);
  const f = page.frameLocator(`[data-pane="${block.id}"] iframe`);
  await expect(f.locator(".monaco-workbench")).toBeVisible({ timeout: 540_000 });
  await expect(f.locator(".explorer-folders-view .monaco-list-row", { hasText: "proj" }).first()).toBeVisible({ timeout: 60_000 });
  // Arugula's theme there too.
  await expect.poll(() => f.locator(".monaco-workbench .part.sidebar").evaluate((e) => getComputedStyle(e).backgroundColor)).toBe("rgb(24, 24, 37)");

  // A file on that machine, at its line.
  const sock = join(state, "sock");
  const { stdout } = await promisify(execFile)(resolve("../target/debug/arugula"), [
    ...["--socket", sock, "--json", "edit", "--machine", `m${m.id}`, "/home/sprite/proj/main.rs:2"],
  ]);
  const id = JSON.parse(stdout).block as PaneId;
  await expect.poll(() => page.evaluate((b) => !!window.__arugula.client.info(b), id)).toBe(true);
  // Found on the VM: the file's project is the folder.
  await expect.poll(() => page.evaluate((b) => window.__arugula.client.info(b)?.project?.name, id), { timeout: 30_000 }).toBe("proj");
  expect(await page.evaluate((b) => window.__arugula.client.info(b)?.file, id)).toBe("main.rs");
  await page.evaluate((b) => window.__arugula.client.setActive(b), id);
  const g = page.frameLocator(`[data-pane="${id}"] iframe`);
  await expect(g.locator(".monaco-editor .view-lines", { hasText: "hello from a sprite" }).first()).toBeVisible({ timeout: 60_000 });
  await expect(g.locator(".tab.active", { hasText: "main.rs" })).toBeVisible();
});
