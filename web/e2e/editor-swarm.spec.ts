// M28: your editor in the swarm. A standalone code-server with Arugula's
// extension installed from its VSIX stands in for VS Code over Remote-SSH
// (S17: the same server and extension host Remote-SSH runs; the extension
// reaches the daemon on the machine the files are on). It joins only when
// asked; the swarm shows it; a phone follows its cursor live across files;
// a breakpoint in its real debugger puts a card on the rail that Continue
// answers; Claude Code (a stand-in, `fake_claude.py`, in a terminal pane)
// proposes an edit that's accepted from the rail and lands in the file; and
// turning the workspace off removes it at once.

import { execFile, spawn, type ChildProcess } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { promisify } from "node:util";
import { devices, expect, test, type Page } from "@playwright/test";
import { ANY, daemonPort } from "./ports";
import { closeContexts } from "./helpers";
import { labs } from "./labs";

test.afterAll(closeContexts);

let APP = "";
let VSCODE = "";
const OWNER = "me@example.com";
test.use({ baseURL: async ({}, use) => use(APP) });
test.describe.configure({ mode: "serial" });

const sh = promisify(execFile);
const CLI = resolve("../target/debug/arugula");
let dir = "";
let state = "";
let proj = "";
let daemon: ChildProcess | undefined;
let code: ChildProcess | undefined;
const cli = (...args: string[]) => sh(CLI, ["--socket", join(state, "sock"), ...args], { cwd: proj });

async function up(url: string) {
  for (let i = 0; i < 300; i++) {
    try {
      if ((await fetch(url)).status < 500) return;
    } catch {
      // not yet
    }
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error(`${url} didn't come up`);
}

/** The code-server release editor blocks use: downloaded by the daemon the
 * first time (M27), so ask it for an editor block if it isn't here yet. */
async function codeServer(): Promise<string> {
  const cache = join(homedir(), ".cache/arugula/code-server");
  // Not a download still being unpacked (`.PID-…`, renamed when done).
  const find = () => (existsSync(cache) ? readdirSync(cache).filter((d) => !d.startsWith(".")).map((d) => join(cache, d, "bin/code-server")).find((p) => existsSync(p)) : undefined);
  if (!find()) {
    await fetch(`${APP}/api/blocks`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ type: "editor", config: { path: proj } }) });
    for (let i = 0; i < 5400 && !find(); i++) await new Promise((r) => setTimeout(r, 100));
  }
  return find()!;
}

test.beforeAll(async () => {
  test.setTimeout(600_000);
  dir = mkdtempSync(join(tmpdir(), "ilg-e2e-m28-"));
  state = join(dir, "state");
  proj = join(dir, "shop");
  mkdirSync(join(proj, ".git"), { recursive: true });
  mkdirSync(join(proj, "src"), { recursive: true });
  writeFileSync(join(proj, "src/prices.js"), Array.from({ length: 60 }, (_, i) => `const price${i} = ${i * 3}; // line ${i + 1}`).join("\n") + "\n");
  writeFileSync(join(proj, "src/cart.js"), "function total(items) {\n  return items.reduce((a, b) => a + b, 0);\n}\nmodule.exports = { total };\n");
  writeFileSync(join(proj, "app.js"), "let n = 0;\nfor (let i = 0; i < 3; i++) n += i;\ndebugger;\nconsole.log('done', n);\n");
  mkdirSync(join(proj, ".vscode"));
  writeFileSync(
    join(proj, ".vscode/launch.json"),
    JSON.stringify({ version: "0.2.0", configurations: [{ type: "node", request: "launch", name: "app", program: "${workspaceFolder}/app.js" }] }),
  );
  daemon = spawn(
    "../target/debug/arugulad",
    [
      ...["--listen", ANY, "--block-listen", ANY, "--state-dir", labs(state)],
      ...["--shell", "bash --norc --noprofile", "--no-manager-env", "--owner", OWNER, "--tailscale-socket", "/nonexistent/sock"],
    ],
    { stdio: "ignore", env: { ...process.env, ARUGULA_WISP_TOKEN_FILE: "/nonexistent", XDG_DATA_HOME: join(dir, "data") } },
  );
  APP = `http://127.0.0.1:${await daemonPort(state, daemon)}`;
  await up(`${APP}/api/host`);
  const bin = await codeServer();
  // The extension, from the daemon, into a code-server of its own: the
  // laptop's VS Code in a Remote-SSH window, as far as the extension can
  // tell (it runs on the files' machine and finds the daemon there).
  const vsix = join(dir, "arugula.vsix");
  await cli("editors", "vsix", "-o", vsix);
  const own = ["--config", join(dir, "cs/config.yaml"), "--user-data-dir", join(dir, "cs/user"), "--extensions-dir", join(dir, "cs/ext")];
  const env = { ...process.env, XDG_DATA_HOME: join(dir, "data"), ARUGULA_SOCK: join(state, "sock") };
  delete (env as Record<string, string | undefined>).ARUGULA_PANE;
  await sh(bin, [...own, "--install-extension", vsix], { env });
  mkdirSync(join(dir, "cs/user/User"), { recursive: true });
  writeFileSync(join(dir, "cs/user/User/settings.json"), JSON.stringify({ "workbench.startupEditor": "none", "chat.disableAIFeatures": true, "security.workspace.trust.enabled": false }));
  code = spawn(bin, [...own, "--auth", "none", "--bind-addr", ANY, "--disable-telemetry", "--disable-update-check", "--disable-workspace-trust", "--ignore-last-opened"], {
    stdio: ["ignore", "pipe", "ignore"],
    env,
  });
  // It says where it listens.
  VSCODE = await new Promise<string>((ok, fail) => {
    let out = "";
    code!.stdout!.on("data", (d) => {
      out += d;
      const m = /HTTP server listening on (http:\/\/127\.0\.0\.1:\d+)/.exec(out);
      if (m) ok(m[1]);
    });
    code!.once("exit", () => fail(new Error(`code-server exited: ${out}`)));
  });
  await up(`${VSCODE}/healthz`);
});

test.afterAll(async () => {
  for (const p of [code, daemon]) {
    if (p && p.exitCode === null) {
      const gone = new Promise((r) => p.once("exit", r));
      p.kill("SIGTERM");
      await gone;
    }
  }
  try {
    process.kill(Number(readFileSync(join(state, "editor/code-server.pid"), "utf8").trim()));
  } catch {
    // no block's server
  }
  rmSync(dir, { recursive: true, force: true });
});

/** A VS Code command, from its palette. */
async function command(vs: Page, name: string) {
  await vs.keyboard.press("F1");
  await vs.locator(".quick-input-widget input").fill(`>${name}`);
  await vs.locator(".quick-input-list .monaco-list-row", { hasText: name }).first().click();
}

async function openFile(vs: Page, rel: string, line: number) {
  // The palette without its ">" is Go to File.
  await vs.keyboard.press("F1");
  await vs.locator(".quick-input-widget input").fill(`${rel}:${line}`);
  await expect(vs.locator(".quick-input-list .monaco-list-row", { hasText: rel.split("/").pop()! }).first()).toBeVisible({ timeout: 10_000 });
  await vs.keyboard.press("Enter");
  await expect(vs.locator(".tab.active", { hasText: rel.split("/").pop()! })).toBeVisible({ timeout: 15_000 });
}

const editorsNow = async () =>
  JSON.parse((await cli("--json", "editors")).stdout) as { pane: number; editor: { app: string; followers: number; debug?: unknown }; file: string | null }[];
const tileKey = (page: Page) => page.evaluate(() => window.__arugula.fleet.panes.find((p) => p.info.type === "editor" && p.session === null)?.key ?? null);

let vs: Page;
let phone: Page;

test("VS Code joins the swarm only when asked, and the swarm shows it", async ({ browser }) => {
  test.setTimeout(120_000);
  vs = await (await browser.newContext({ viewport: { width: 1280, height: 800 } })).newPage();
  await vs.goto(`${VSCODE}/?folder=${encodeURIComponent(proj)}`);
  await expect(vs.locator(".monaco-workbench")).toBeVisible({ timeout: 60_000 });
  // Off until asked: the status bar offers it.
  const item = vs.locator(".statusbar-item", { hasText: "arugula" }).first();
  await expect(item).toBeVisible({ timeout: 30_000 });
  expect(await editorsNow()).toEqual([]);
  await command(vs, "arugula: Show this workspace in the swarm");
  await expect.poll(async () => (await editorsNow()).length, { timeout: 15_000 }).toBe(1);
  await openFile(vs, "src/prices.js", 20);
  await expect.poll(async () => (await editorsNow())[0]?.file).toBe("src/prices.js");
  expect((await editorsNow())[0].editor.app).toBe("code-server");

  // A phone's swarm: the editor is a tile in its project.
  const ctx = await browser.newContext({ ...devices["Pixel 7"], baseURL: APP });
  phone = await ctx.newPage();
  await phone.goto("/#swarm");
  await expect(phone.locator(".swarm")).toBeVisible();
  await expect.poll(() => tileKey(phone), { timeout: 15_000 }).not.toBeNull();
  const tile = await phone.evaluate(() => window.__arugula.fleet.panes.find((p) => p.info.type === "editor" && p.session === null)!);
  expect(tile.info.kind).toBe("editor");
  expect(tile.info.project?.name).toBe("shop");
  expect(tile.info.file).toBe("src/prices.js");
  expect(tile.info.title).toBe("prices.js — shop (VS Code)");
});

test("a phone follows its cursor live, across files", async () => {
  test.setTimeout(60_000);
  // Tap the tile: follow it.
  await phone.waitForTimeout(1500);
  const key = (await tileKey(phone))!;
  const pos = (await phone.evaluate((k) => (window.__arugula.swarm as { screenOf(k: string): { x: number; y: number } }).screenOf(k), key))!;
  await phone.mouse.click(pos.x, pos.y);
  await expect(phone.locator(".follow")).toBeVisible();
  await expect(phone.locator(".follow-file")).toContainText("src/prices.js", { timeout: 10_000 });
  await expect(phone.locator(".follow .cm-content")).toContainText("const price19 = 57; // line 20");
  // VS Code says someone follows.
  await expect(vs.locator(".statusbar-item", { hasText: "1 following" })).toBeVisible({ timeout: 10_000 });
  const cursorLine = () =>
    phone.evaluate(() => {
      const f = (window as unknown as { __follow?: { view: { state: { doc: { lineAt(n: number): { number: number } }; selection: { main: { head: number } } } } } }).__follow;
      return f ? f.view.state.doc.lineAt(f.view.state.selection.main.head).number : null;
    });
  await expect.poll(cursorLine).toBe(20);
  // It moves: the phone follows, quickly.
  await vs.keyboard.press("Control+g");
  await vs.keyboard.type("45");
  const t0 = Date.now();
  await vs.keyboard.press("Enter");
  await expect.poll(cursorLine, { intervals: [20] }).toBe(45);
  console.log(`cursor followed in ${Date.now() - t0} ms`);
  await expect(phone.locator(".follow .cm-content")).toContainText("// line 45");
  // Typing shows up too.
  await vs.keyboard.press("End");
  await vs.keyboard.type(" // edited live");
  await expect(phone.locator(".follow .cm-content")).toContainText("// line 45 // edited live", { timeout: 5000 });
  // Another file.
  await openFile(vs, "src/cart.js", 2);
  await expect(phone.locator(".follow-file")).toContainText("src/cart.js", { timeout: 5000 });
  await expect(phone.locator(".follow .cm-content")).toContainText("items.reduce");
  await expect.poll(cursorLine).toBe(2);
  // A selection shows as one.
  await vs.keyboard.press("Shift+ArrowDown");
  await expect
    .poll(() =>
      phone.evaluate(() => {
        const f = (window as unknown as { __follow?: { view: { state: { selection: { main: { empty: boolean } } } } } }).__follow;
        return f ? !f.view.state.selection.main.empty : false;
      }),
    )
    .toBe(true);
  // Stop following: VS Code's status bar says so.
  await phone.locator("[data-follow-close]").click();
  await expect(vs.locator(".statusbar-item", { hasText: "following" })).toHaveCount(0, { timeout: 10_000 });
  // Discard the edit, so the debugger test starts clean.
  await vs.keyboard.press("Escape");
});

test("a breakpoint puts a card on the rail, and Continue works", async () => {
  test.setTimeout(90_000);
  await openFile(vs, "app.js", 1);
  await vs.keyboard.press("F5");
  // The phone's rail: the debugger paused.
  const card = phone.locator('.swarm-card[data-kind="paused"]');
  await expect(card).toBeVisible({ timeout: 30_000 });
  await expect(card).toContainText("Debugger paused");
  await expect(card).toContainText("app.js:3");
  await card.locator("[data-continue]").click();
  // It runs on to the end: the card goes, and the program finished.
  await expect(card).toHaveCount(0, { timeout: 15_000 });
  await expect.poll(async () => (await editorsNow())[0]?.editor.debug ?? null, { timeout: 15_000 }).toBeNull();
  await expect(vs.locator(".repl")).toContainText("done 3");
});

test("Claude Code's diff from a terminal pane is accepted from the rail and lands in the file", async ({ page }) => {
  test.setTimeout(60_000);
  const fake = resolve("../crates/daemon/tests/fake_claude.py");
  const pane = Number((await cli("run", "--cwd", proj, "--", "python3", fake)).stdout.trim().replace("%", ""));
  await expect.poll(async () => (await fetch(`${APP}/api/panes/${pane}/capture`)).text()).toContain("connected");
  await fetch(`${APP}/api/panes/${pane}/send`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ text: `edit ${join(proj, "src/cart.js")} function total(items) {\\n  return items.reduce((a, b) => a + b.price, 0);\\n}\\nmodule.exports = { total };\\n`, enter: true }),
  });
  // On the desktop, beside the terminal: the pane's agent card.
  await page.goto("/");
  await expect.poll(() => page.evaluate(() => window.__arugula?.client.connected)).toBe(true);
  await page.evaluate((p) => window.__arugula.client.setActive(p), pane);
  const beside = page.locator(`[data-pane="${pane}"] .diff-card`);
  await expect(beside).toBeVisible({ timeout: 15_000 });
  await expect(beside).toContainText("cart.js");
  await expect(beside.locator(".diff-text .add")).toContainText("b.price");
  // On the phone's rail: accept it there.
  const card = phone.locator('.swarm-card[data-kind="diff"]');
  await expect(card).toBeVisible({ timeout: 10_000 });
  await expect(card.locator(".diff-text .del")).toContainText("a + b, 0");
  await card.locator("[data-accept]").click();
  await expect.poll(() => readFileSync(join(proj, "src/cart.js"), "utf8")).toContain("a + b.price, 0");
  await expect(card).toHaveCount(0);
  await expect(beside).toHaveCount(0);
  // ...and the pane says who did.
  await expect.poll(() => page.evaluate((p) => window.__arugula.client.info(p)?.answered?.how, pane)).toBe("accepted");
});

test("turning the workspace off removes it from the swarm at once", async () => {
  expect(await tileKey(phone)).not.toBeNull();
  const t0 = Date.now();
  await command(vs, "arugula: Take this workspace out of the swarm");
  await expect.poll(() => tileKey(phone), { intervals: [20] }).toBeNull();
  console.log(`gone from the swarm ${Date.now() - t0} ms after the command`);
  expect(await editorsNow()).toEqual([]);
  // Remembered for the folder: a reload stays out.
  await vs.reload();
  await expect(vs.locator(".monaco-workbench")).toBeVisible({ timeout: 60_000 });
  await expect(vs.locator(".statusbar-item", { hasText: "arugula" }).first()).toBeVisible({ timeout: 30_000 });
  await vs.waitForTimeout(1500);
  expect(await editorsNow()).toEqual([]);
});
