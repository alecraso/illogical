// M70 past one hop: a file pasted into a pane lands on the host that pane
// runs on, whichever way the page reaches it. A sandbox that only dials
// out (through the home daemon, `/h/NAME`), and a pane from another host
// in the home layout (its own daemon, reached directly). Each daemon has a
// temp directory of its own, so where a file lands says which host took it.
// (Through control's relay: control.spec.ts.)

import { spawn, type ChildProcess } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test, type Page } from "@playwright/test";
import { active, menu, open, paneEl, pasteFile, ready, uploadedPath } from "./helpers";
import { ANY, daemonPort } from "./ports";
import { labs } from "./labs";

let homeUrl = "";
const dirs: string[] = [];
const daemons: ChildProcess[] = [];
/** Each daemon's TMPDIR, where its uploads go. */
const tmpOf = new Map<string, string>();

test.use({ baseURL: async ({}, use) => use(homeUrl) });
test.describe.configure({ mode: "serial" });

function temp(what: string) {
  const d = mkdtempSync(join(tmpdir(), `arugula-e2e-uphosts-${what}-`));
  dirs.push(d);
  return d;
}

async function startDaemon(name: string, extra: string[] = []) {
  const state = temp(name);
  tmpOf.set(name, temp(`${name}-tmp`));
  const env = { ...process.env, TMPDIR: tmpOf.get(name)! };
  delete env.XDG_RUNTIME_DIR;
  const d = spawn(
    "../target/debug/arugulad",
    [
      ...["--listen", ANY, "--name", name, "--state-dir", labs(state)],
      ...["--shell", "bash --norc --noprofile", "--no-manager-env", "--tailscale-socket", "/nonexistent/tailscaled.sock"],
      ...extra,
    ],
    { stdio: "ignore", env },
  );
  daemons.push(d);
  const port = await daemonPort(state, d);
  for (let i = 0; i < 100; i++) {
    try {
      if ((await fetch(`http://127.0.0.1:${port}/api/host`)).ok) return `http://127.0.0.1:${port}`;
    } catch {
      // not up yet
    }
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error(`daemon ${name} did not start`);
}

test.beforeAll(async () => {
  homeUrl = await startDaemon("home");
  // A sandbox that dials home with a token home minted.
  const { token } = (await (await fetch(`${homeUrl}/api/hosts/sbx/token`, { method: "POST" })).json()) as { token: string };
  const tokenFile = join(temp("token"), "token");
  writeFileSync(tokenFile, token, { mode: 0o600 });
  await startDaemon("sbx", ["--peer", homeUrl.replace("http", "ws"), "--token", tokenFile]);
  await expect.poll(async () => (await fetch(`${homeUrl}/h/sbx/api/host`)).status, { timeout: 15_000 }).toBe(200);
  // Another host the page reaches directly.
  const otherUrl = await startDaemon("other", ["--allow-origin", homeUrl]);
  const res = await fetch(`${homeUrl}/api/hosts`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ name: "other", urls: [otherUrl] }),
  });
  expect(res.ok).toBe(true);
});

test.afterAll(() => {
  for (const d of daemons) d.kill("SIGKILL");
  for (const d of dirs) rmSync(d, { recursive: true, force: true });
});

/** Some bytes of every value, past one chunk. */
const png = Buffer.from(Array.from({ length: 1_500_000 }, (_, i) => (i * 7) % 256));

async function landsOn(page: Page, pane: number, host: string) {
  await pasteFile(page, pane, png, "shot.png", "image/png");
  const path = await uploadedPath(page, pane);
  expect(path.startsWith(join(tmpOf.get(host)!, "arugula-uploads"))).toBe(true);
  expect(readFileSync(path)).toEqual(png);
}

test("through a sandbox that only dials out, the file lands on the sandbox", async ({ page }) => {
  await open(page);
  await page.evaluate(() => window.__arugula.hosts.select("sbx"));
  await expect.poll(() => page.evaluate(() => window.__arugula.client.base)).toBe("/h/sbx");
  await expect.poll(() => page.evaluate(() => !!window.__arugula.client.connected && window.__arugula.client.state !== null)).toBe(true);
  const pane = await active(page);
  await ready(page, pane);
  await landsOn(page, pane, "sbx");
});

test("in a pane from another host, the file lands on that host", async ({ page }) => {
  await open(page);
  await page.evaluate(() => window.__arugula.hosts.select("home"));
  await expect.poll(() => page.evaluate(() => window.__arugula.client.base)).toBe("");
  await expect.poll(() => page.evaluate(() => window.__arugula.client.connected)).toBe(true);
  const local = await active(page);
  await ready(page, local);
  await menu(page, paneEl(page, local), "Split right on other");
  const remote = () => page.evaluate(() => window.__arugula.client.state!.panes.find((p) => p.type === "remote")?.id ?? null);
  await expect.poll(remote).not.toBeNull();
  const block = (await remote())!;
  await expect.poll(() => paneEl(page, block).locator(".block-remote").getAttribute("data-state")).toBe("live");
  await landsOn(page, block, "other");
});
