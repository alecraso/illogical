// M65: "Invite over ssh…" on a pane's menu gives an ssh command, and the
// system's own ssh client, run with it, sees the pane and can't type.

import { spawn, type ChildProcess } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { devices, expect, test, type Page } from "@playwright/test";
import { active, menu, open, paneEl, ready, run, text } from "./helpers";
import { ANY, daemonPort } from "./ports";

let url = "";
let daemon: ChildProcess | undefined;
let state = "";

test.use({ baseURL: async ({}, use) => use(url) });
test.describe.configure({ mode: "serial" });

// The ssh invite isn't in the menu by default; this test turns it on.
test.beforeEach(({ page }) => page.addInitScript(() => localStorage.setItem("illogical.more", "1")));

test.beforeAll(async () => {
  state = mkdtempSync(join(tmpdir(), "illogical-e2e-guest-ssh-"));
  daemon = spawn(
    "../target/debug/illogicald",
    [
      ...["--listen", ANY, "--state-dir", state],
      ...["--shell", "bash --norc --noprofile", "--no-manager-env", "--tailscale-socket", "/nonexistent/tailscaled.sock"],
      ...["--guest-ssh", "127.0.0.1:0", "--guest-ssh-host", "127.0.0.1"],
    ],
    { stdio: "ignore" },
  );
  const port = await daemonPort(state, daemon);
  url = `http://127.0.0.1:${port}`;
  await expect.poll(async () => (await fetch(`${url}/api/host`).catch(() => null))?.ok ?? false).toBe(true);
});

test.afterAll(async () => {
  if (daemon && daemon.exitCode === null) {
    const gone = new Promise((r) => daemon!.once("exit", r));
    daemon.kill();
    await gone;
  }
  rmSync(state, { recursive: true, force: true, maxRetries: 5 });
});

/** The command a guest would paste, run by `sh` with ssh's config and
 * prompts kept out of it and a remote terminal forced. */
function guest(command: string) {
  const hermetic = command.replace(/^ssh /, "ssh -F /dev/null -o BatchMode=yes -tt ");
  const p = spawn("/bin/sh", ["-c", hermetic], { stdio: ["pipe", "pipe", "pipe"] });
  let out = "";
  p.stdout.on("data", (b: Buffer) => (out += b.toString()));
  p.stderr.on("data", (b: Buffer) => (out += b.toString()));
  return { p, out: () => out };
}

async function invite(page: Page) {
  await open(page);
  const pane = await active(page);
  await ready(page, pane);
  const marker = `guest-${Math.floor(Math.random() * 1e6)}`;
  await run(page, pane, `echo ${marker}`, marker);
  await menu(page, paneEl(page, pane), "Invite over ssh…");
  const input = page.locator(".prompt-backdrop input");
  await expect(input).toHaveValue(/^ssh -p \d+ .*KnownHostsCommand=.* g[0-9a-f]{32}@127\.0\.0\.1$/);
  const command = await input.inputValue();
  await page.keyboard.press("Escape");
  return { pane, marker, command };
}

async function watchesAndCantType(page: Page) {
  const { pane, marker, command } = await invite(page);
  const g = guest(command);
  try {
    await expect.poll(g.out, { timeout: 15_000 }).toContain(marker);
    g.p.stdin.write("echo typed-by-guest\r");
    await expect.poll(g.out).toContain("read-only");
    await page.waitForTimeout(500);
    expect(await text(page, pane)).not.toContain("typed-by-guest");
    const list = (await (await fetch(`${url}/api/guests`)).json()) as { pane: number; sessions: number }[];
    expect(list).toEqual([expect.objectContaining({ pane, sessions: 1 })]);
    // Leaving with Ctrl-] ends ssh.
    g.p.stdin.write("\x1d");
    await expect.poll(() => g.p.exitCode, { timeout: 10_000 }).toBe(0);
  } finally {
    g.p.kill();
    for (const i of (await (await fetch(`${url}/api/guests`)).json()) as { id: number }[]) {
      await fetch(`${url}/api/guests/${i.id}`, { method: "DELETE" });
    }
  }
}

test("a pane's menu doesn't offer the ssh invite unless asked for", async ({ page }) => {
  await page.addInitScript(() => localStorage.removeItem("illogical.more"));
  await open(page);
  const pane = await active(page);
  await ready(page, pane);
  await paneEl(page, pane).click({ button: "right", position: { x: 60, y: 60 } });
  await expect(page.getByRole("menuitem", { name: "Share read-only link…" })).toBeVisible();
  await expect(page.getByRole("menuitem", { name: "Invite over ssh…" })).toHaveCount(0);
});

test("a pane's menu makes an ssh invite a stock ssh client can watch with", async ({ page }) => {
  await watchesAndCantType(page);
});

test.describe("on a phone", () => {
  const { viewport, userAgent, deviceScaleFactor, isMobile, hasTouch } = devices["Pixel 7"];
  test.use({ viewport, userAgent, deviceScaleFactor, isMobile, hasTouch });
  test("the same menu entry is there", async ({ page }) => {
    await watchesAndCantType(page);
  });
});
