// M20: hosted sandboxes. Someone with only a browser starts a hosted VM:
// control makes a sprite, puts the daemon in it, and the browser that asked
// approves its join by itself. They work in it through control (which
// reaches it via the provider's proxy, end to end), split a second shell
// into it, and closing its last tab deletes the machine. A quota stops the
// next one. Needs wispd (the Sprites API) and its token on this host, and
// `just static` built; elsewhere these skip.

import { spawn, type ChildProcess } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { createServer, type Server } from "node:http";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test, type Page } from "@playwright/test";
import { ready, run, text, closeContexts } from "./helpers";
import { controlPort, listen } from "./ports";

test.afterAll(closeContexts);

const HOST = "127.0.0.1";
let base = "";
const WISP = process.env.ILLOGICAL_WISP_URL ?? "http://127.0.0.1:7788";
const BINARY = "../target/x86_64-unknown-linux-musl/release/illogicald";
const token = (() => {
  try {
    return readFileSync(process.env.ILLOGICAL_WISP_TOKEN_FILE ?? `${homedir()}/.local/share/wisp/token`, "utf8").trim();
  } catch {
    return "";
  }
})();
const procs: ChildProcess[] = [];
const dirs: string[] = [];
let gh: Server;

test.describe.configure({ mode: "serial" });
test.use({ baseURL: async ({}, use) => use(base) });
test.skip(!token || !existsSync(BINARY), "needs wispd, its token, and `just static`");

const wisp = (method: string, path: string) => fetch(`${WISP}/v1/sprites${path}`, { method, headers: { Authorization: `Bearer ${token}` } });

test.beforeAll(async () => {
  gh = createServer((req, res) => {
    const u = new URL(req.url!, "http://github");
    if (u.pathname === "/login/oauth/authorize") {
      const back = new URL(u.searchParams.get("redirect_uri")!);
      back.searchParams.set("code", "c");
      back.searchParams.set("state", u.searchParams.get("state")!);
      res.writeHead(302, { location: back.href }).end();
    } else if (u.pathname === "/login/oauth/access_token") {
      res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify({ access_token: "t" }));
    } else if (u.pathname === "/user") {
      res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify({ id: 99, login: "browseronly" }));
    } else res.writeHead(404).end();
  });
  const github = `http://127.0.0.1:${await listen(gh)}`;
  const d = mkdtempSync(join(tmpdir(), "illogical-e2e-sbx-"));
  dirs.push(d);
  procs.push(
    spawn(
      "../target/debug/illogical-control",
      [
        ...["--listen", `${HOST}:0`, "--public-url", `http://${HOST}:0`, "--db", join(d, "control.db"), "--static-dir", "dist"],
        ...["--github-client-id", "id", "--github-client-secret", "s"],
        ...["--github-url", github, "--github-api", github],
        ...["--sprites-url", WISP, "--sandbox-binary", BINARY, "--sandbox-accounts", "*", "--sandbox-quota", "2"],
      ],
      {
        stdio: process.env.E2E_CONTROL_LOG ? ["ignore", "inherit", "inherit"] : "ignore",
        env: { ...process.env, SPRITES_TOKEN: token, RUST_LOG: "illogical_control=debug" },
      },
    ),
  );
  base = `http://${HOST}:${await controlPort(join(d, "control.db"), procs.at(-1))}`;
  for (let i = 0; i < 100; i++) {
    try {
      if ((await fetch(`${base}/control.json`)).ok) break;
    } catch {
      // not yet
    }
    await new Promise((r) => setTimeout(r, 100));
  }
});

test.afterAll(async () => {
  for (const p of procs) p.kill("SIGKILL");
  gh?.close();
  // Anything a failed test left behind.
  const list = await (await wisp("GET", "?prefix=ilc-")).json().catch(() => ({ sprites: [] }));
  for (const s of list.sprites ?? []) await wisp("DELETE", `/${s.name}`);
  for (const d of dirs) rmSync(d, { recursive: true, force: true });
});

let page: Page;
let sandbox = "";

test("someone with only a browser starts a hosted VM and works in it", async ({ browser }) => {
  test.slow();
  page = await (await browser.newContext()).newPage();
  await page.goto("/");
  await page.locator("[data-signin=github]").click();
  await page.locator("[data-stored-codes]").check();
  await page.locator("[data-saved-codes]").click();
  await page.locator("[data-start-vm]").click();
  // It joins, this browser approves it, and the page switches to it.
  await expect.poll(() => page.evaluate(() => window.__illogical?.client.connected), { timeout: 120_000, intervals: [1000] }).toBe(true);
  sandbox = await page.evaluate(() => window.__illogical.control!.daemons[0].sandbox!);
  expect(sandbox).toMatch(/^ilc-/);
  expect(await page.evaluate(() => window.__illogical.client.path)).toBe("relayed");
  const first = await page.evaluate(() => window.__illogical.client.state!.panes[0].id);
  await ready(page, first);
  await run(page, first, "echo vm-$(hostname)-$((6*7))", "-42");
  expect(await text(page, first)).toContain(`vm-${sandbox}-42`);
  // A second shell, split into the same VM.
  await page.evaluate((p) => window.__illogical.client.intent({ op: "split", pane: p, edge: "right" }), first);
  await expect.poll(() => page.evaluate(() => window.__illogical.client.state!.panes.length)).toBe(2);
  const second = await page.evaluate((p) => window.__illogical.client.state!.panes.find((x) => x.id !== p)!.id, first);
  await page.evaluate((p) => window.__illogical.client.setActive(p), second);
  await ready(page, second);
  await run(page, second, "echo also-$(hostname)", `also-${sandbox}`);
});

test("a quota stops the next one", async () => {
  await page.evaluate(() => window.__illogical.control!.startSandbox());
  const third = await page.evaluate(() => window.__illogical.control!.startSandbox().then(() => "made", (e: Error) => e.message));
  expect(third).toContain("the most for now");
  // Tidy the second one away.
  await expect.poll(() => page.evaluate(() => window.__illogical.control!.sandboxes.length), { timeout: 30_000 }).toBe(2);
  const other = await page.evaluate((s) => window.__illogical.control!.sandboxes.find((x) => x.id !== s)!.id, sandbox);
  await page.evaluate((id) => window.__illogical.control!.deleteSandbox(id), other);
});

test("closing its last tab deletes the machine", async () => {
  expect((await wisp("GET", `/${sandbox}`)).status).toBe(200);
  const tab = await page.evaluate(() => window.__illogical.client.state!.tabs[0].id);
  await page.evaluate((t) => window.__illogical.client.intent({ op: "close_tab", tab: t }), tab);
  await expect.poll(async () => (await wisp("GET", `/${sandbox}`)).status, { timeout: 60_000, intervals: [1000] }).toBe(404);
  await expect.poll(() => page.evaluate(() => window.__illogical.control!.daemons.length), { timeout: 20_000 }).toBe(0);
});
