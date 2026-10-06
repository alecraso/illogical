// M49: a daemon joined to illogical control doesn't list the account's other
// machines on its own page (that would make it a hub). Its host menu has
// "All your machines…", which opens control's page. A daemon that isn't
// joined has neither the link nor, with no other hosts, the menu.

import { spawn, type ChildProcess } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import type { AddressInfo } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test, type Page } from "@playwright/test";
import { Device } from "../fixtures/device.ts";
import { fakeGithub } from "../fixtures/fakes.ts";
import { ANY, controlPort, daemonPort } from "./ports";
import { labs } from "./labs";

let control = "";
let joinedUrl = "";
let aloneUrl = "";
const procs: ChildProcess[] = [];
const dirs: string[] = [];
let gh: ReturnType<typeof fakeGithub>;

test.describe.configure({ mode: "serial" });

function temp(what: string) {
  const d = mkdtempSync(join(tmpdir(), `illogical-e2e-hostmenu-${what}-`));
  dirs.push(d);
  return d;
}

async function up(url: string) {
  for (let i = 0; i < 100; i++) {
    try {
      if ((await fetch(url)).status < 500) return;
    } catch {
      // not yet
    }
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error(`${url} didn't come up`);
}

async function daemon(name: string, state: string): Promise<string> {
  const d = spawn(
    "../target/debug/illogicald",
    [
      ...["--listen", ANY, "--name", name, "--state-dir", labs(state)],
      ...["--shell", "bash --norc --noprofile", "--no-manager-env", "--tailscale-socket", "/nonexistent/sock", "--no-claude-ide"],
    ],
    { stdio: "ignore" },
  );
  procs.push(d);
  const url = `http://127.0.0.1:${await daemonPort(state, d)}`;
  await up(`${url}/api/host`);
  return url;
}

test.beforeAll(async () => {
  gh = fakeGithub(0);
  await new Promise((r) => gh.once("listening", r));
  const github = `http://127.0.0.1:${(gh.address() as AddressInfo).port}`;
  const db = join(temp("db"), "control.db");
  procs.push(
    spawn(
      "../target/debug/illogical-control",
      [
        ...["--listen", ANY, "--public-url", "http://127.0.0.1:0", "--db", db],
        ...["--github-client-id", "id", "--github-client-secret", "s"],
        ...["--github-url", github, "--github-api", github],
      ],
      { stdio: "ignore" },
    ),
  );
  control = `http://127.0.0.1:${await controlPort(db, procs.at(-1))}`;
  await up(`${control}/control.json`);

  // A machine joined by code, approved by the headless device.
  const me = await Device.signIn({ control, login: "jake" });
  const state = temp("joined");
  const joining = spawn("../target/debug/illogicald", ["join", control, "--name", "mini", "--state-dir", state, "--account", me.keys.id], {
    stdio: ["ignore", "pipe", "ignore"],
  });
  procs.push(joining);
  const code = await new Promise<string>((res) => {
    let out = "";
    joining.stdout!.on("data", (d) => {
      out += d;
      const m = out.match(/#join=([A-Z0-9]{5}-[A-Z0-9]{5})/);
      if (m) res(m[1]);
    });
  });
  const exited = new Promise<number | null>((r) => joining.on("exit", r));
  await me.approveJoin(code);
  expect(await exited).toBe(0);
  joinedUrl = await daemon("mini", state);
  aloneUrl = await daemon("alone", temp("alone"));
});

test.afterAll(() => {
  for (const p of procs) p.kill("SIGKILL");
  gh?.close();
  for (const d of dirs) rmSync(d, { recursive: true, force: true });
});

test("a joined daemon's host menu opens control's page for the other machines", async ({ page, context }) => {
  await page.goto(joinedUrl);
  await expect(page.locator(".host-button")).toHaveText(/mini/);
  await page.locator(".host-button").click();
  const all = page.getByRole("menuitem", { name: "All your machines…" });
  await expect(all).toBeVisible();
  const opened = context.waitForEvent("page");
  await all.click();
  const tab = await opened;
  await tab.waitForLoadState("domcontentloaded");
  expect(new URL(tab.url()).origin).toBe(control);
});

test("a daemon that isn't joined has no such link", async ({ page }) => {
  await page.goto(aloneUrl);
  await expect.poll(() => page.evaluate(() => !!window.__illogical?.client.connected)).toBe(true);
  // One host and no control: nothing to switch to, so no menu at all.
  await expect(page.locator(".host-button")).toHaveCount(0);
  await expect(page.getByRole("menuitem", { name: "All your machines…" })).toHaveCount(0);
});

const closeAll = (page: Page) =>
  page.evaluate(() => {
    const c = window.__illogical.client;
    for (const s of c.state!.sessions) c.intent({ op: "close_session", session: s.id });
  });
const sessions = (page: Page) => page.evaluate(() => window.__illogical.client.state?.sessions.length ?? -1);

test("with no sessions the bar and its host menu are still there, and after closing the last one", async ({ page }) => {
  await page.goto(joinedUrl);
  await expect.poll(() => page.evaluate(() => !!window.__illogical?.client.connected)).toBe(true);
  await expect.poll(() => sessions(page)).toBeGreaterThan(0);
  await closeAll(page);
  await expect.poll(() => sessions(page)).toBe(0);
  await expect(page.locator(".empty")).toContainText("No sessions.");
  await expect(page.locator("header.bar .host-button")).toHaveText(/mini/);
  // Nothing of a session in the bar, and no stale tab behind it.
  await expect(page.locator(".session-button, .tab")).toHaveCount(0);
  await page.locator(".host-button").click();
  await expect(page.getByRole("menuitem", { name: "All your machines…" })).toBeVisible();
  await page.keyboard.press("Escape");

  // A session made from the empty state brings its name and tab...
  await page.getByRole("button", { name: "New session" }).click();
  await expect(page.locator(".session-button")).toBeVisible();
  await expect(page.locator(".tab")).toHaveCount(1);
  // ...and closing that last one lands on the working bar again.
  await closeAll(page);
  await expect.poll(() => sessions(page)).toBe(0);
  await expect(page.locator(".empty")).toContainText("No sessions.");
  await expect(page.locator(".session-button, .tab")).toHaveCount(0);
  expect(await page.evaluate(() => [window.__illogical.client.session, window.__illogical.client.tab])).toEqual([null, null]);
  await page.locator(".host-button").click();
  await expect(page.getByRole("menuitem", { name: "All your machines…" })).toBeVisible();
  await page.keyboard.press("Escape");
  await page.getByRole("button", { name: "New session" }).click();
  await expect(page.locator(".tab")).toHaveCount(1);
});

test("on a phone, with no sessions, the header opens the sheet with the machines", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 760 });
  await page.goto(joinedUrl);
  await expect.poll(() => page.evaluate(() => !!window.__illogical?.client.connected)).toBe(true);
  await expect.poll(() => sessions(page)).toBeGreaterThan(0);
  await closeAll(page);
  await expect.poll(() => sessions(page)).toBe(0);
  await expect(page.locator(".empty")).toContainText("No sessions.");
  await page.locator(".phone-bar .sheet-button").click();
  await expect(page.locator(".sheet-hosts")).toContainText("mini");
  await page.locator(".sheet-backdrop").click({ position: { x: 380, y: 750 } });
  await page.getByRole("button", { name: "New session" }).click();
  await expect(page.locator(".phone-bar .crumb").first()).toBeVisible();
});
