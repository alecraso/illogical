// M7: the directory picker, on a desktop and a phone, for a local tab, a
// VM tab (its directories, through the provider) and a second daemon as
// another host (M4a). New sessions and machines have generated names.
// The VM parts need wispd and its token on this host; elsewhere they skip.

import { spawn, type ChildProcess } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";
import { devices, expect, test, type Page } from "@playwright/test";
import { paneEl, ready, text, type as typeIn } from "./helpers";
import { ANY, daemonPort } from "./ports";
import type { PaneId } from "../src/proto";

let homeUrl = "";
let otherUrl = "";
const WISP = process.env.ILLOGICAL_WISP_URL ?? "http://127.0.0.1:7788";
const token = (() => {
  try {
    return readFileSync(process.env.ILLOGICAL_WISP_TOKEN_FILE ?? `${homedir()}/.local/share/wisp/token`, "utf8").trim();
  } catch {
    return "";
  }
})();

const states: string[] = [];
const daemons = new Map<string, ChildProcess>();
// Directories to browse, on this host (made in beforeAll: a spec module is
// loaded more than once, #62).
let root = "";

test.use({ baseURL: async ({}, use) => use(homeUrl) });
test.describe.configure({ mode: "serial" });

/** Start a daemon; its URL. */
async function startDaemon(name: string, extra: string[] = []) {
  const state = mkdtempSync(join(tmpdir(), `ilg-e2e-m7-${name}-`));
  states.push(state);
  const d = spawn(
    "../target/debug/illogicald",
    [
      ...["--listen", ANY, "--name", name, "--state-dir", state],
      ...["--shell", "bash --norc --noprofile", "--no-manager-env", "--tailscale-socket", "/nonexistent/tailscaled.sock"],
      ...extra,
    ],
    { stdio: "ignore" },
  );
  daemons.set(name, d);
  const url = `http://127.0.0.1:${await daemonPort(state, d)}`;
  for (let i = 0; i < 100; i++) {
    try {
      if ((await fetch(`${url}/api/host`)).ok) return url;
    } catch {
      // not up yet
    }
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error(`daemon ${name} did not start`);
}

const wisp = (method: string, path: string) =>
  fetch(`${WISP}/v1/sprites${path}`, { method, headers: { Authorization: `Bearer ${token}` } });

test.beforeAll(async () => {
  // Resolved, as the shell's pwd is (macOS's temp dir is behind a symlink).
  root = realpathSync(mkdtempSync(join(tmpdir(), "ilg-m7-dirs-")));
  for (const d of ["alpha/beta", "alpha/delta", "gamma", "with space"]) mkdirSync(join(root, d), { recursive: true });
  homeUrl = await startDaemon("home");
  otherUrl = await startDaemon("other", ["--allow-origin", homeUrl]);
  const res = await fetch(`${homeUrl}/api/hosts`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ name: "other", urls: [otherUrl] }),
  });
  expect(res.ok).toBe(true);
});

test.afterAll(async () => {
  for (const d of daemons.values()) d.kill("SIGKILL");
  // Machines a failed test left behind (this daemon's only).
  for (const state of states) {
    let id = "";
    try {
      id = readFileSync(join(state, "daemon-id"), "utf8").trim();
    } catch {
      // never made one
    }
    if (id && token) {
      const list = await (await wisp("GET", `?prefix=illogical-eph-${id}-`)).json();
      for (const s of list.sprites ?? []) await wisp("DELETE", `/${s.name}`);
    }
    rmSync(state, { recursive: true, force: true });
  }
  if (root) rmSync(root, { recursive: true, force: true });
});

const connected = (page: Page) =>
  page.evaluate(() => !!window.__illogical?.client.connected && window.__illogical.client.state !== null);
const panesOf = (page: Page) => page.evaluate(() => window.__illogical.client.state!.panes);
const activePane = (page: Page) => page.evaluate(() => window.__illogical.client.active()!);
const cwd = (page: Page, p: PaneId) => page.evaluate((p) => window.__illogical.client.info(p)?.cwd ?? null, p);
const pickerPath = (page: Page) => page.locator(".picker-path").getAttribute("data-path");
const row = (page: Page, path: string) => page.locator(`.picker-row[data-path="${path}"]`);

/** Show `name`'s host, from a clean page. */
async function onHost(page: Page, name: string) {
  await page.goto("/");
  await expect.poll(() => connected(page)).toBe(true);
  await page.evaluate((n) => window.__illogical.hosts.select(n), name);
  await expect.poll(() => page.evaluate(() => window.__illogical.client.base)).toBe(name === "home" ? "" : otherUrl);
  await expect.poll(() => connected(page)).toBe(true);
}

/** Type a line into a pane, showing it first (a phone shows one at a time). */
async function type(page: Page, p: PaneId, s: string) {
  await page.evaluate((p) => window.__illogical.client.setActive(p), p);
  await expect(paneEl(page, p)).toBeVisible();
  await typeIn(page, p, s);
}

/** The pane that appears next, after `act`. */
async function next(page: Page, act: () => Promise<unknown>): Promise<PaneId> {
  const before = (await panesOf(page)).map((p) => p.id);
  await act();
  let id: PaneId | undefined;
  await expect
    .poll(async () => {
      id = (await panesOf(page)).find((p) => !before.includes(p.id))?.id;
      return id ?? null;
    })
    .not.toBeNull();
  return id!;
}

test("desktop: browse from the pane's directory, new pane there, cd there", async ({ page }) => {
  test.setTimeout(60_000);
  await onHost(page, "home");
  // A generated session name, not a number.
  const name = await page.evaluate(() => window.__illogical.client.state!.sessions[0].name);
  expect(name).toMatch(/^[a-z]+ [a-z]+$/);
  await expect(page.locator(".session-button")).toContainText(name);

  const pane = await activePane(page);
  await ready(page, pane);
  await type(page, pane, `cd ${root}\n`);
  await expect.poll(() => cwd(page, pane)).toBe(root);

  // From the pane's menu: it starts where the pane is.
  await paneEl(page, pane).click({ button: "right", position: { x: 60, y: 60 } });
  await page.getByRole("menuitem", { name: "Go to directory…" }).click();
  await expect(page.locator(".picker")).toBeVisible();
  await expect.poll(() => pickerPath(page)).toBe(root);
  await expect(row(page, join(root, "alpha"))).toBeVisible();
  await expect(row(page, join(root, "with space"))).toBeVisible();
  // Fuzzy filter, then Enter goes in.
  await page.locator(".picker-filter").fill("alp");
  await expect(page.locator(".picker-row")).toHaveCount(1);
  await page.keyboard.press("Enter");
  await expect.poll(() => pickerPath(page)).toBe(join(root, "alpha"));
  await row(page, join(root, "alpha/beta")).click();
  await expect.poll(() => pickerPath(page)).toBe(join(root, "alpha/beta"));

  const made = await next(page, () => page.getByRole("button", { name: "New pane here" }).click());
  await expect(page.locator(".picker")).toHaveCount(0);
  await ready(page, made);
  await expect.poll(() => cwd(page, made)).toBe(join(root, "alpha/beta"));
  expect(await page.evaluate(() => window.__illogical.client.tabView()!.layout.panes.length)).toBe(2);

  // Ctrl+Shift+G on the first pane: cd its shell (a path with a space).
  await page.evaluate((p) => window.__illogical.client.setActive(p), pane);
  await paneEl(page, pane).click({ position: { x: 40, y: 40 } });
  await page.keyboard.press("Control+Shift+G");
  await expect(page.locator(".picker")).toBeVisible();
  await expect.poll(() => pickerPath(page)).toBe(root);
  await row(page, join(root, "with space")).click();
  await page.getByRole("button", { name: "cd there" }).click();
  await expect(page.locator(".picker")).toHaveCount(0);
  await expect.poll(() => cwd(page, pane)).toBe(join(root, "with space"));

  // Recent directories come first next time.
  await page.keyboard.press("Control+Shift+G");
  await expect(page.locator(".picker-row.recent").first()).toBeVisible();
  await expect(page.locator(`.picker-row.recent[data-path="${join(root, "alpha/beta")}"]`)).toBeVisible();
  await page.keyboard.press("Escape");

  // Busy: cd is refused, with why, and nothing is typed.
  await type(page, pane, "sleep 30\n");
  await expect.poll(() => page.evaluate((p) => window.__illogical.client.info(p)?.current !== null, pane)).toBe(true);
  await page.keyboard.press("Control+Shift+G");
  await expect.poll(() => pickerPath(page)).toBe(join(root, "with space"));
  // Backspace in an empty filter goes up.
  await page.keyboard.press("Backspace");
  await expect.poll(() => pickerPath(page)).toBe(root);
  const cd = await page.evaluate(
    async ([p, path]) => (await fetch(`/api/panes/${p}/cd`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ path }) })).status,
    [pane, root] as const,
  );
  expect(cd).toBe(400);
  await page.getByRole("button", { name: "cd there" }).click();
  await expect(page.locator(".picker-error")).toContainText("running something");
  await page.getByRole("button", { name: "Cancel" }).click();
  // Only the cd from before, nothing typed after `sleep`.
  expect((await text(page, pane)).split("sleep 30")[1]).not.toContain("cd -- ");

  // New tab here, from the + button's menu.
  const tabs = await page.evaluate(() => window.__illogical.client.state!.tabs.length);
  await page.locator(".new-tab").click({ button: "right" });
  await page.getByRole("menuitem", { name: "In a directory…" }).click();
  await expect.poll(() => pickerPath(page)).toBe(join(root, "with space"));
  await page.locator(".picker-row", { hasText: ".." }).first().click();
  await row(page, join(root, "gamma")).click();
  const inTab = await next(page, () => page.getByRole("button", { name: "New tab here" }).click());
  await expect.poll(() => page.evaluate(() => window.__illogical.client.state!.tabs.length)).toBe(tabs + 1);
  await expect.poll(() => cwd(page, inTab)).toBe(join(root, "gamma"));
});

test("desktop: the picker on another host (M4a) shows that host's directories", async ({ page }) => {
  await onHost(page, "other");
  const name = await page.evaluate(() => window.__illogical.client.state!.sessions[0].name);
  expect(name).toMatch(/^[a-z]+ [a-z]+$/);
  const pane = await activePane(page);
  await ready(page, pane);
  // Both daemons are on this machine, so the same directories exist; what
  // matters is that the other daemon answers and makes the pane.
  await type(page, pane, `cd ${join(root, "gamma")}\n`);
  await expect.poll(() => cwd(page, pane)).toBe(join(root, "gamma"));
  await page.keyboard.press("Control+Shift+G");
  await expect(page.locator(".picker-where")).toContainText("other");
  await expect.poll(() => pickerPath(page)).toBe(join(root, "gamma"));
  await page.locator(".picker-row", { hasText: ".." }).first().click();
  await row(page, join(root, "alpha")).click();
  const made = await next(page, () => page.getByRole("button", { name: "New pane here" }).click());
  await expect.poll(() => cwd(page, made)).toBe(join(root, "alpha"));
  // It's the other daemon's pane.
  const remote = (await (await fetch(`${otherUrl}/api/panes`)).json()) as { id: number; cwd: string }[];
  expect(remote.find((p) => p.id === made)?.cwd).toBe(join(root, "alpha"));
});

test.describe("phone", () => {
  const { viewport, userAgent, deviceScaleFactor, isMobile, hasTouch } = devices["Pixel 7"];
  test.use({ viewport, userAgent, deviceScaleFactor, isMobile, hasTouch });

  const sheet = (page: Page) => page.locator(".sheet-button").click();

  test("local: from the sheet, browse and open a pane there", async ({ page }) => {
    await onHost(page, "home");
    // A fresh session (the desktop test left a command running).
    const pane = await next(page, () =>
      page.evaluate(() => window.__illogical.client.intent({ op: "new_session", name: null, from_pane: null })),
    );
    await ready(page, pane);
    await type(page, pane, `cd ${root}\n`);
    await expect.poll(() => cwd(page, pane)).toBe(root);
    await sheet(page);
    await page.getByRole("button", { name: "Go to directory" }).click();
    await expect(page.locator(".picker.phone")).toBeVisible();
    // A sheet: the whole width.
    const box = (await page.locator(".picker.phone").boundingBox())!;
    expect(box.width).toBeGreaterThan(viewport.width - 2);
    await row(page, join(root, "alpha")).click();
    await row(page, join(root, "alpha/delta")).click();
    const made = await next(page, () => page.getByRole("button", { name: "New pane here" }).click());
    await expect.poll(() => cwd(page, made)).toBe(join(root, "alpha/delta"));
    await expect.poll(() => activePane(page)).toBe(made);
  });

  test("on a VM tab: browse the VM, new pane here, a shell in that directory", async ({ page }) => {
    test.skip(!token, "no wisp token on this host");
    test.setTimeout(120_000);
    await onHost(page, "home");
    const vm = await next(page, async () => {
      await sheet(page);
      await page.getByRole("button", { name: "New VM tab" }).click();
    });
    await expect.poll(() => cwd(page, vm), { timeout: 60_000 }).toBe("/home/sprite");
    // The machine has a generated name of its own; the sprite keeps its id.
    const m = await page.evaluate((p) => window.__illogical.client.machine(p)!, vm);
    expect(m.name).toMatch(/^[a-z]+ [a-z]+$/);
    expect(m.sprite).toMatch(/^illogical-eph-/);
    await type(page, vm, "mkdir -p ~/m7/inner ~/m7/other && echo made-$((6*7))\n");
    await expect.poll(() => text(page, vm)).toContain("made-42");

    await sheet(page);
    await page.getByRole("button", { name: "Go to directory" }).click();
    await expect(page.locator(".picker.phone")).toBeVisible();
    await expect(page.locator(".picker-where")).toContainText(m.name!);
    await expect.poll(() => pickerPath(page), { timeout: 20_000 }).toBe("/home/sprite");
    // Hidden directories after the rest; m7 is there.
    await row(page, "/home/sprite/m7").click();
    await expect.poll(() => pickerPath(page)).toBe("/home/sprite/m7");
    await row(page, "/home/sprite/m7/inner").click();
    await expect.poll(() => pickerPath(page)).toBe("/home/sprite/m7/inner");
    // A VM's directories are only in its tab.
    await expect(page.getByRole("button", { name: "New tab here" })).toBeDisabled();
    const made = await next(page, () => page.getByRole("button", { name: "New pane here" }).click());
    // On the same machine, in that directory.
    expect(await page.evaluate((p) => window.__illogical.client.info(p)?.host, made)).toBe(m.id);
    await expect.poll(() => cwd(page, made), { timeout: 30_000 }).toBe("/home/sprite/m7/inner");
    await type(page, made, "pwd; hostname\n");
    await expect.poll(() => text(page, made)).toContain("/home/sprite/m7/inner");
    expect(await text(page, made)).toContain(m.sprite);

    // cd there, on the VM's first pane.
    await page.evaluate((p) => window.__illogical.client.setActive(p), vm);
    await sheet(page);
    await page.getByRole("button", { name: "Go to directory" }).click();
    await expect.poll(() => pickerPath(page), { timeout: 20_000 }).toBe("/home/sprite");
    await row(page, "/home/sprite/m7").click();
    await row(page, "/home/sprite/m7/other").click();
    await page.getByRole("button", { name: "cd there" }).click();
    await expect(page.locator(".picker")).toHaveCount(0);
    await expect.poll(() => cwd(page, vm), { timeout: 20_000 }).toBe("/home/sprite/m7/other");

    // Close the tab: its machine goes.
    const tab = await page.evaluate((p) => window.__illogical.client.tabOfPane(p)!.id, vm);
    await page.evaluate((t) => window.__illogical.client.intent({ op: "close_tab", tab: t }), tab);
    await expect.poll(async () => (await wisp("GET", `/${m.sprite}`)).status, { timeout: 30_000 }).toBe(404);
  });
});
