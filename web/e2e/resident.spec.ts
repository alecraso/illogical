// M4b's "done when", from a phone: open a shell on a sandbox with no daemon
// in it; make a daemon resident in it (copied in, kept running as a sprite
// service, reached through the home daemon's provider tunnel); start
// something in a pane there, let the sandbox go cold (on wisp, a real
// reboot), reopen, and the layout and scrollback are back.
//
// "Cold" on wisp happens after `--warm-ttl` (1h) of being suspended. The
// test does the same thing at once with wispd's operator endpoints (the
// ones its web UI uses): suspend, then drop the memory snapshot ("cool").
//
// Needs wispd and its token, and `just static`; skips without them. With
// ARUGULA_E2E_CLAUDE=1 the pane runs Claude Code (installed in the
// sandbox first), given the token in ~/.config/arugula/claude-oauth-token
// on its stdin with echo off: never on a command line or the sandbox's disk.

import { spawn, type ChildProcess } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";
import { devices, expect, test, type Page } from "@playwright/test";
import { ready, type as typeIn } from "./helpers";
import { ANY, daemonPort } from "./ports";
import { labs } from "./labs";

const WISP = process.env.ARUGULA_WISP_URL ?? "http://127.0.0.1:7788";
const STATIC = "../target/x86_64-unknown-linux-musl/release";
const read = (path: string) => {
  try {
    return readFileSync(path, "utf8").trim();
  } catch {
    return "";
  }
};
const wispToken = read(process.env.ARUGULA_WISP_TOKEN_FILE ?? `${homedir()}/.local/share/wisp/token`);
const claude = process.env.ARUGULA_E2E_CLAUDE === "1";
const missing = !wispToken ? "no wisp token" : !existsSync(`${STATIC}/arugulad`) ? "no static build (just static)" : "";

const sprite = `arugula-m4b-e2e-${Date.now().toString(36)}`;
const auth = { Authorization: `Bearer ${wispToken}` };
let base = "";
let home: ChildProcess | undefined;
let state = "";

async function wisp(method: string, path: string, body?: string) {
  const headers = body ? { ...auth, "Content-Type": "application/json" } : auth;
  return fetch(`${WISP}/v1/sprites${path}`, { method, headers, body });
}

const status = async () => ((await (await wisp("GET", `/${sprite}`)).json()) as { status: string }).status;

/** What wisp's warm TTL does, now: suspend, then drop the snapshot. */
async function goCold() {
  const login = await fetch(`${WISP}/ui/login`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ token: wispToken }),
  });
  expect(login.status).toBe(204);
  const cookie = login.headers.get("set-cookie")!.split(";")[0];
  const ui = (what: string) =>
    fetch(`${WISP}/ui/api/sprites/${sprite}/${what}`, { method: "POST", headers: { Cookie: cookie, "X-Sandpit-UI": "1" } });
  expect((await ui("suspend")).status).toBe(200);
  expect((await ui("cool")).status).toBe(200);
  expect(await status()).toBe("cold");
}

/** The resident daemon's API, through the home daemon's tunnel. */
const tunnel = (path: string, body?: unknown) =>
  fetch(`${base}/tunnel/${sprite}${path}`, {
    method: body === undefined ? "GET" : "POST",
    headers: { "Content-Type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });

test.describe.configure({ mode: "serial" });

test.beforeAll(async () => {
  if (missing) return;
  state = mkdtempSync(join(tmpdir(), "arugula-e2e-m4b-"));
  home = spawn(
    "../target/debug/arugulad",
    ["--listen", ANY, "--name", "home", "--shell", "bash --norc --noprofile", "--no-manager-env"]
      .concat(["--state-dir", labs(state), "--static-dir", STATIC]),
    { stdio: "ignore" },
  );
  base = `http://127.0.0.1:${await daemonPort(state, home)}`;
  await expect.poll(() => fetch(`${base}/api/host`).then((r) => r.ok, () => false), { timeout: 15_000 }).toBe(true);
  expect((await wisp("POST", "", JSON.stringify({ name: sprite }))).ok).toBe(true);
});

test.afterAll(async () => {
  if (!missing) await wisp("DELETE", `/${sprite}`).catch(() => {});
  home?.kill("SIGKILL");
  if (state) rmSync(state, { recursive: true, force: true });
});

const { viewport, userAgent, deviceScaleFactor, isMobile, hasTouch } = devices["Pixel 7"];
test.use({ viewport, userAgent, deviceScaleFactor, isMobile, hasTouch, baseURL: async ({}, use) => use(base) });

const connected = (page: Page) =>
  page.evaluate(() => !!window.__arugula?.client.connected && window.__arugula.client.state !== null);

async function sandboxes(page: Page) {
  await page.locator(".sheet-button").click();
  await page.getByRole("button", { name: "Sandboxes" }).click();
  const row = page.locator(`[data-sandbox="${sprite}"]`);
  // wisp may hold many sandboxes: filter, once the list is in.
  await expect(page.locator(".sandbox-list li").first()).toBeVisible({ timeout: 15_000 });
  const filter = page.locator(".sandbox-filter");
  if (await filter.isVisible()) await filter.fill(sprite);
  await expect(row).toBeVisible();
  return row;
}

const paneText = (page: Page, pane: number) => page.evaluate((p) => window.__arugula.text(p), pane);

test("a shell on a sandbox, with no daemon there, is disposable", async ({ page }) => {
  test.skip(!!missing, missing);
  test.setTimeout(60_000);
  await page.goto("/");
  await expect.poll(() => connected(page)).toBe(true);
  const row = await sandboxes(page);
  await expect(page.locator(".sandboxes .hint")).toContainText("disposable");
  await row.getByRole("button", { name: "Shell" }).click();
  await expect(page.locator(".host-badge.borrowed")).toContainText(`${sprite} · shell`);
  const pane = await page.evaluate(() => window.__arugula.client.active()!);
  await ready(page, pane);
  await typeIn(page, pane, "echo on-$(hostname)-$((6*7))\n");
  await expect.poll(() => paneText(page, pane), { timeout: 20_000 }).toContain(`on-${sprite}-42`);
  // Closing the pane leaves the sandbox alone (it isn't ours).
  await page.evaluate((p) => window.__arugula.client.intent({ op: "close_pane", pane: p }), pane);
  await expect.poll(() => page.evaluate((p) => !!window.__arugula.client.info(p), pane)).toBe(false);
  expect((await wisp("GET", `/${sprite}`)).status).toBe(200);
});

test("a resident daemon's layout and scrollback come back after the sandbox goes cold", async ({ page }) => {
  test.skip(!!missing, missing);
  test.setTimeout(claude ? 420_000 : 180_000);
  await page.goto("/");
  await expect.poll(() => connected(page)).toBe(true);

  // Make it resident: the phone switches to it, through the tunnel.
  const row = await sandboxes(page);
  await row.getByRole("button", { name: "Make resident" }).click();
  await expect.poll(() => page.evaluate(() => window.__arugula.hosts.current), { timeout: 60_000 }).toBe(sprite);
  await expect.poll(() => page.evaluate(() => window.__arugula.client.base)).toBe(`/tunnel/${sprite}`);
  await expect.poll(() => connected(page), { timeout: 30_000 }).toBe(true);
  await expect(page.locator(".host-crumb")).toHaveText(sprite);
  // (The host is named after the sandbox; the API calls below use it.)
  const hostName = sprite;

  // A second tab, so there's a layout to come back.
  await page.evaluate(() => {
    const c = window.__arugula.client;
    c.intent({ op: "new_tab", session: c.session!, from_pane: null });
  });
  await expect.poll(() => page.evaluate(() => window.__arugula.client.state!.panes.length)).toBe(2);
  const pane = await page.evaluate(() => window.__arugula.client.active()!);
  await ready(page, pane);
  await typeIn(page, pane, "echo before-cold-$((6*7)); uptime -s\n");
  await expect.poll(() => paneText(page, pane), { timeout: 20_000 }).toContain("before-cold-42");
  const bootLine = /^\d{4}-\d\d-\d\d \d\d:\d\d:\d\d$/gm;
  await expect.poll(async () => (await paneText(page, pane)).match(bootLine)?.length ?? 0).toBe(1);
  const bootBefore = (await paneText(page, pane)).match(bootLine)![0];

  if (claude) {
    const token = read(`${homedir()}/.config/arugula/claude-oauth-token`);
    test.skip(!token, "no ~/.config/arugula/claude-oauth-token");
    await typeIn(page, pane, "curl -fsSL https://claude.ai/install.sh | bash; echo installed-$((1+1))\n");
    await expect.poll(() => paneText(page, pane), { timeout: 240_000, intervals: [2000] }).toContain("installed-2");
    // The token goes in on stdin with echo off, from here (not typed by
    // the browser), and only into claude's environment.
    await typeIn(page, pane, "stty -echo; printf 'token? '; read -r T; stty echo; CLAUDE_CODE_OAUTH_TOKEN=$T ~/.local/bin/claude\n");
    await expect.poll(() => paneText(page, pane), { timeout: 20_000 }).toContain("token? ");
    const r = await fetch(`${base}/tunnel/${hostName}/api/panes/${pane}/send`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ text: token, enter: true }),
    });
    expect(r.ok).toBe(true);
    // Claude Code's TUI is up (whatever it says: the account may be at its limit).
    await expect
      .poll(() => page.evaluate((p) => window.__arugula.screen(p), pane), { timeout: 60_000 })
      .toMatch(/Welcome to Claude|text style|Claude Code v\d|Try "|limit/i);
    expect(await paneText(page, pane)).not.toContain(token.slice(0, 12));
    await page.screenshot({ path: "test-results/m4b-claude-before-cold.png" });
  }
  const before = await page.evaluate(() => window.__arugula.client.state!.panes.map((p) => p.id));
  // Let the checkpoint land (5s idle).
  await page.waitForTimeout(6000);

  // The page goes to the background: it lets go of the sandbox.
  await page.evaluate(() => {
    Object.defineProperty(document, "visibilityState", { value: "hidden", configurable: true });
    document.dispatchEvent(new Event("visibilitychange"));
  });
  await expect.poll(() => page.evaluate(() => window.__arugula.client.connected), { timeout: 15_000 }).toBe(false);
  // The swarm's summary connection lets go too: its reconnect would wake it.
  await expect
    .poll(() => page.evaluate((s) => window.__arugula.fleet.list.find((h) => h.name === s)?.state, sprite))
    .not.toBe("connected");

  await goCold();
  // The host list says so, from the provider (without waking it).
  await expect
    .poll(async () => {
      const list = (await (await fetch(`${base}/api/hosts`)).json()) as { hosts: { name: string; status?: string }[] };
      return list.hosts.find((h) => h.name === hostName)?.status;
    })
    .toBe("cold");
  expect(await status()).toBe("cold");

  // Back to the foreground: the tunnel wakes it (a cold boot), and the
  // resident daemon restores its panes from its own disk.
  await page.evaluate(() => {
    Object.defineProperty(document, "visibilityState", { value: "visible", configurable: true });
    document.dispatchEvent(new Event("visibilitychange"));
  });
  await expect.poll(() => connected(page), { timeout: 60_000 }).toBe(true);
  await expect.poll(() => page.evaluate(() => window.__arugula.client.state!.panes.map((p) => p.id))).toEqual(before);
  await ready(page, pane);
  await expect.poll(() => paneText(page, pane), { timeout: 20_000 }).toContain("before-cold-42");
  await expect.poll(() => paneText(page, pane)).toContain("── restored");
  // It really rebooted: a new boot time.
  await typeIn(page, pane, "uptime -s; echo after-$((7*7))\n");
  await expect.poll(() => paneText(page, pane), { timeout: 20_000 }).toContain("after-49");
  const boots = (await paneText(page, pane)).match(bootLine) ?? [];
  const tail = (await paneText(page, pane)).slice(-1500);
  expect(boots.at(-1), `booted ${bootBefore}, then ${boots.join(", ")}:\n${tail}`).not.toBe(bootBefore);
  // Claude Code's screen is in the restored scrollback.
  if (claude) expect(await paneText(page, pane)).toMatch(/text style|Claude Code v\d|limit/i);
  await page.screenshot({ path: "test-results/m4b-after-cold.png" });

  // Over the CLI's route too: the tunnel answers for the API.
  expect((await tunnel("/api/host")).ok).toBe(true);
});
