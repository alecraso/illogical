// M4c: a sandbox that can only dial out. It dials the home daemon with a
// per-host token and is used from the home page through it (the browser
// never connects to the sandbox itself); a read-only share link shows a
// pane live and takes no input; and after the sandbox is deleted, its
// synced history is still searchable on the home daemon, as ciphertext on
// disk.

import { spawn, type ChildProcess } from "node:child_process";
import { mkdtempSync, readdirSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test, type Page } from "@playwright/test";
import { active, menu, paneEl, ready, run, text, closeContexts } from "./helpers";
import { ANY, daemonPort } from "./ports";
import { labs } from "./labs";

test.afterAll(closeContexts);

let HOME = 0;
let SANDBOX = 0;
let homeUrl = "";
const dirs: string[] = [];
const daemons = new Map<string, ChildProcess>();
let homeState = "";
let sandboxState = "";

test.use({ baseURL: async ({}, use) => use(homeUrl) });
test.describe.configure({ mode: "serial" });

function temp(what: string) {
  const d = mkdtempSync(join(tmpdir(), `arugula-e2e-dial-${what}-`));
  dirs.push(d);
  return d;
}

/** Start a daemon; its port. */
async function startDaemon(name: string, state: string, extra: string[] = []) {
  const d = spawn(
    "../target/debug/arugulad",
    [
      ...["--listen", ANY, "--name", name, "--state-dir", labs(state)],
      ...["--shell", "bash --norc --noprofile", "--no-manager-env", "--tailscale-socket", "/nonexistent/tailscaled.sock"],
      ...extra,
    ],
    { stdio: "ignore" },
  );
  daemons.set(name, d);
  const port = await daemonPort(state, d);
  for (let i = 0; i < 100; i++) {
    try {
      if ((await fetch(`http://127.0.0.1:${port}/api/host`)).ok) return port;
    } catch {
      // not up yet
    }
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error(`daemon ${name} did not start`);
}

async function json<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await fetch(homeUrl + path, init);
  if (!res.ok) throw new Error(`${path}: ${res.status} ${await res.text()}`);
  return (await res.json()) as T;
}

test.beforeAll(async () => {
  homeState = temp("home");
  HOME = await startDaemon("home", homeState);
  homeUrl = `http://127.0.0.1:${HOME}`;
  // A token for the sandbox, minted by home; only its hash stays there.
  const { token } = await json<{ token: string }>("/api/hosts/sbx/token", { method: "POST" });
  const tokenFile = join(temp("token"), "token");
  writeFileSync(tokenFile, token, { mode: 0o600 });
  // The sandbox listens on loopback only for its own CLI; nothing here
  // ever connects to that port. It dials home.
  sandboxState = temp("sbx");
  SANDBOX = await startDaemon("sbx", sandboxState, [
    ...["--peer", `ws://127.0.0.1:${HOME}`, "--token", tokenFile],
    ...["--sync", "--sync-live", "--sync-every", "1"],
  ]);
  await expect.poll(async () => (await fetch(`${homeUrl}/h/sbx/api/host`)).status, { timeout: 15_000 }).toBe(200);
});

test.afterAll(() => {
  for (const d of daemons.values()) d.kill("SIGKILL");
  for (const d of dirs) rmSync(d, { recursive: true, force: true });
});

const connected = (page: Page) =>
  page.evaluate(() => !!window.__arugula?.client.connected && window.__arugula.client.state !== null);
const base = (page: Page) => page.evaluate(() => window.__arugula?.client.base);

async function switchTo(page: Page, name: string) {
  await page.locator(".host-button").click();
  await page.getByRole("menuitem", { name: new RegExp(`^\\s*(✓ )?${name}\\b`) }).click();
}

test("a sandbox that only dials out is listed and used through the home daemon", async ({ page }) => {
  // Everything the browser talks to.
  const reached: string[] = [];
  page.on("request", (r) => reached.push(r.url()));
  page.on("websocket", (ws) => reached.push(ws.url()));

  await page.goto("/");
  await expect.poll(() => connected(page)).toBe(true);
  const list = await json<{ hosts: { name: string; transport: string; urls: string[] }[] }>("/api/hosts");
  expect(list.hosts).toEqual([expect.objectContaining({ name: "sbx", transport: "dial_out", urls: [] })]);

  await switchTo(page, "sbx");
  await expect.poll(() => base(page)).toBe("/h/sbx");
  await expect.poll(() => connected(page)).toBe(true);
  await expect(page.locator(".host-button")).toHaveText(/sbx/);
  const pane = await active(page);
  await ready(page, pane);
  await run(page, pane, "echo via-home-$((6*7))", "via-home-42");
  // It is the sandbox's pane; home has only its own.
  const there = await json<{ id: number }[]>("/h/sbx/api/panes");
  expect(there.map((p) => p.id)).toContain(pane);
  expect(await json<unknown[]>("/api/panes")).toHaveLength(1);
  // A split there, from the mouse, through the tunnel.
  await menu(page, paneEl(page, pane), "Split right");
  await expect.poll(async () => (await json<unknown[]>("/h/sbx/api/panes")).length).toBe(2);

  // The page never went near the sandbox's own port.
  expect(reached.filter((u) => u.includes(`:${SANDBOX}`))).toEqual([]);
  expect(reached.some((u) => u === `ws://127.0.0.1:${HOME}/h/sbx/ws`)).toBe(true);

  // A reload comes back to it, through home again.
  await page.reload();
  await expect.poll(() => connected(page)).toBe(true);
  expect(await base(page)).toBe("/h/sbx");
  await ready(page, pane);
  await expect.poll(() => text(page, pane)).toContain("via-home-42");
});

test("a read-only share link shows a pane live and refuses input", async ({ page, context }) => {
  await page.goto("/");
  await expect.poll(() => connected(page)).toBe(true);
  await page.evaluate(() => window.__arugula.hosts.select("home"));
  await expect.poll(() => base(page)).toBe("");
  await expect.poll(() => connected(page)).toBe(true);
  const pane = await active(page);
  await ready(page, pane);
  await run(page, pane, "echo shared-$((2*21))", "shared-42");

  // From the pane's menu: a link, shown (and copied).
  await menu(page, paneEl(page, pane), "Share read-only link…");
  const input = page.locator(".prompt-backdrop input");
  await expect(input).toHaveValue(/\/share\/ils_/);
  const url = await input.inputValue();
  await page.keyboard.press("Escape");
  const shares = await json<{ id: number; pane: number; token?: string }[]>("/api/shares");
  expect(shares).toEqual([expect.objectContaining({ pane })]);
  expect(shares[0].token).toBeUndefined();

  const viewer = await context.newPage();
  await viewer.goto(url);
  const shown = () => viewer.evaluate(() => (window as unknown as { __share: { text(): string } }).__share.text());
  const ended = () => viewer.evaluate(() => (window as unknown as { __share: { ended: string | null } }).__share.ended);
  await expect.poll(shown).toContain("shared-42");
  await expect(viewer.locator("#share-state")).toHaveText(/Read-only/);

  // Live: output from the owner's side shows up.
  await run(page, pane, "echo live-$((3*3))", "live-9");
  await expect.poll(shown).toContain("live-9");

  // Typing in the viewer goes nowhere.
  await viewer.locator("#share-term").click();
  await viewer.keyboard.type("echo typed-in-viewer\n");
  await page.waitForTimeout(1000);
  expect(await text(page, pane)).not.toContain("typed-in-viewer");
  const capture = await (await fetch(`${homeUrl}/api/panes/${pane}/capture?scope=scrollback`)).text();
  expect(capture).not.toContain("typed-in-viewer");
  expect(await ended()).toBeNull();

  // Revoked: the viewer is cut off and the link is dead.
  const res = await fetch(`${homeUrl}/api/shares/${shares[0].id}`, { method: "DELETE" });
  expect(res.ok).toBe(true);
  await expect.poll(ended).toMatch(/expired or was revoked/);
  expect((await fetch(url)).status).toBe(404);
});

test("after the sandbox is deleted, its history is searchable at home and ciphertext on disk", async () => {
  type Hit = { host?: string; pane: number; line: string };
  const search = () => json<Hit[]>(`/api/search?re=${encodeURIComponent("via-home-[0-9]+")}&host=sbx`);
  // Synced as it grew (--sync-live).
  await expect.poll(async () => (await search()).length, { timeout: 15_000 }).toBeGreaterThan(0);

  // The sandbox goes away for good: killed, its disk gone, off the list.
  const sbx = daemons.get("sbx")!;
  const exited = new Promise((r) => sbx.once("exit", r));
  sbx.kill("SIGKILL");
  await exited;
  rmSync(sandboxState, { recursive: true, force: true });
  expect((await fetch(`${homeUrl}/api/hosts/sbx`, { method: "DELETE" })).ok).toBe(true);
  expect((await fetch(`${homeUrl}/h/sbx/api/host`)).status).toBe(502);

  const hits = await search();
  expect(hits[0]).toEqual(expect.objectContaining({ host: "sbx", line: expect.stringContaining("via-home-42") }));
  const history = await json<{ text: string | null; host?: string }[]>("/api/history?host=sbx");
  expect(history.some((c) => c.host === "sbx" && (c.text ?? "").includes("via-home"))).toBe(true);
  const tail = await (await fetch(`${homeUrl}/api/panes/${hits[0].pane}/tail?host=sbx&text=1`)).text();
  expect(tail).toContain("via-home-42");

  // On disk: sealed files, and a private key.
  const files: string[] = [];
  const walk = (d: string) => {
    for (const e of readdirSync(d, { withFileTypes: true })) {
      if (e.isDirectory()) walk(join(d, e.name));
      else files.push(join(d, e.name));
    }
  };
  walk(join(homeState, "synced", "sbx"));
  expect(files.some((f) => f.endsWith(".enc"))).toBe(true);
  for (const f of files) expect(readFileSync(f).includes("via-home")).toBe(false);
  expect(statSync(join(homeState, "synced", "key")).mode & 0o777).toBe(0o600);
});
