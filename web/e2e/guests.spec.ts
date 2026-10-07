// M14: safe write access. An editor (a guest) gets VMs, never this machine:
// their new tab is a VM tab and they work there; they can't drive the
// owner's local shell until the owner trusts them with it, and that ends by
// itself; a quota stops their next VM; a private pane is hidden from them.
// Needs wispd and its token on this host; elsewhere these skip.

import { spawn, type ChildProcess } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { homedir, hostname, tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test, type Page } from "@playwright/test";
import { ready, run, text, closeContexts } from "./helpers";
import { ANY, daemonPort } from "./ports";
import { labs } from "./labs";

test.afterAll(closeContexts);

let base = "";
const OWNER = "me@example.com";
const GUEST = "guest@example.com";
const WISP = process.env.ARUGULA_WISP_URL ?? "http://127.0.0.1:7788";
const token = (() => {
  try {
    return readFileSync(process.env.ARUGULA_WISP_TOKEN_FILE ?? `${homedir()}/.local/share/wisp/token`, "utf8").trim();
  } catch {
    return "";
  }
})();
let daemon: ChildProcess;
let dir: string;

test.describe.configure({ mode: "serial" });
test.use({ baseURL: async ({}, use) => use(base) });
test.skip(!token, "needs wispd and its token");

test.beforeAll(async () => {
  dir = mkdtempSync(join(tmpdir(), "arugula-e2e-guests-"));
  daemon = spawn(
    "../target/debug/arugulad",
    [
      ...["--listen", ANY, "--state-dir", labs(dir), "--owner", OWNER, "--guest-machines", "2"],
      ...["--shell", "bash --norc --noprofile", "--no-manager-env", "--tailscale-socket", "/nonexistent/sock"],
    ],
    { stdio: "ignore" },
  );
  base = `http://127.0.0.1:${await daemonPort(dir, daemon)}`;
  for (let i = 0; i < 100; i++) {
    try {
      if ((await fetch(`${base}/api/host`)).ok) return;
    } catch {
      // not yet
    }
    await new Promise((r) => setTimeout(r, 100));
  }
});

test.afterAll(async () => {
  daemon?.kill("SIGKILL");
  // VMs a failed test left behind.
  const wisp = (method: string, path: string) => fetch(`${WISP}/v1/sprites${path}`, { method, headers: { Authorization: `Bearer ${token}` } });
  try {
    const id = readFileSync(join(dir, "daemon-id"), "utf8").trim();
    const list = await (await wisp("GET", `?prefix=illogical-eph-${id}-`)).json();
    for (const s of list.sprites ?? []) await wisp("DELETE", `/${s.name}`);
  } catch {
    // nothing to clean
  }
  rmSync(dir, { recursive: true, force: true });
});

const api = (path: string, body?: unknown, headers: Record<string, string> = {}) =>
  fetch(base + path, {
    method: body === undefined ? "GET" : "POST",
    headers: { ...headers, ...(body === undefined ? {} : { "content-type": "application/json" }) },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
const asGuest = { "tailscale-user-login": GUEST };

let owner: Page;
let guest: Page;
let session = 0;
let local = 0;

async function openAs(page: Page) {
  await page.goto("/");
  await expect.poll(() => page.evaluate(() => window.__arugula?.client.connected)).toBe(true);
}

const panes = (page: Page) => page.evaluate(() => window.__arugula.client.state!.panes.map((p) => ({ id: p.id, host: p.host })));

test("a guest's new tab is a VM tab, and they work in it", async ({ browser }) => {
  owner = await (await browser.newContext()).newPage();
  await openAs(owner);
  [session, local] = await owner.evaluate(() => [window.__arugula.client.state!.sessions[0].id, window.__arugula.client.state!.panes[0].id]);
  await run(owner, local, "echo owner-$((6*7))", "owner-42");
  await api("/api/acl", { session, principal: `tailnet:${GUEST}`, role: "editor" });
  guest = await (await browser.newContext({ extraHTTPHeaders: asGuest })).newPage();
  await openAs(guest);
  await guest.evaluate((s) => window.__arugula.client.intent({ op: "new_tab", session: s, from_pane: null }), session);
  await expect.poll(async () => (await panes(guest)).length).toBe(2);
  const vm = (await panes(guest)).find((p) => p.id !== local)!;
  expect(vm.host).not.toBeNull();
  await guest.evaluate((p) => window.__arugula.client.setActive(p), vm.id);
  await ready(guest, vm.id);
  await run(guest, vm.id, "echo on-$(hostname)-$((6*7))", "-42");
  expect(await text(guest, vm.id)).not.toContain(`on-${hostname()}-42`);
});

test("the owner's local shell needs the owner's trust, which ends by itself", async () => {
  test.slow();
  await guest.evaluate((p) => window.__arugula.client.selectTab(window.__arugula.client.tabOfPane(p)!.id), local);
  await ready(guest, local);
  await guest.locator(`[data-pane="${local}"]`).click({ position: { x: 40, y: 40 } });
  await guest.keyboard.type("echo GUEST-ON-HOST\n");
  await expect(guest.getByText("runs on me@example.com's own machine")).toBeVisible();
  expect((await api(`/api/panes/${local}/send`, { text: "echo nope\n" }, asGuest)).status).toBe(403);
  // They ask; the owner is asked (and pushed a notification) and allows a
  // minute. Taking control from the owner, they can type.
  await guest.evaluate((p) => window.__arugula.client.paneOp(p, { op: "request_trust" }), local);
  await expect(owner.locator(`[data-trust-request="${local}"]`)).toBeVisible();
  await owner.locator(`[data-trust-request="${local}"] select`).selectOption("10");
  await owner.evaluate(
    ([p, who]) => {
      const c = window.__arugula.client;
      c.trustRequests = [];
      c.paneOp(p, { op: "grant_trust", to: who, minutes: 1 });
    },
    [local, `tailnet:${GUEST}`] as const,
  );
  await expect.poll(() => guest.evaluate((p) => window.__arugula.client.mayType(p), local)).toBe(true);
  await guest.evaluate((p) => window.__arugula.client.paneOp(p, { op: "take_control" }), local);
  await run(guest, local, "echo trusted-$((6*7))", "trusted-42");
  // A minute later it's over, by itself.
  await expect.poll(() => guest.evaluate((p) => window.__arugula.client.mayType(p), local), { timeout: 70_000, intervals: [2000] }).toBe(false);
  await guest.keyboard.type("echo AFTER-EXPIRY\n");
  await new Promise((r) => setTimeout(r, 500));
  expect(await text(owner, local)).not.toContain("AFTER-EXPIRY");
});

test("a quota stops the guest's next VM", async () => {
  await guest.evaluate((s) => window.__arugula.client.intent({ op: "new_tab", session: s, from_pane: null }), session);
  await expect.poll(async () => (await panes(guest)).length, { timeout: 30_000 }).toBe(3);
  // A third (the limit is 2 here) is refused, with the reason.
  await guest.evaluate((s) => window.__arugula.client.intent({ op: "new_tab", session: s, from_pane: null }), session);
  await expect(guest.getByText("the most a guest may have")).toBeVisible();
  expect((await panes(guest)).length).toBe(3);
});

test("a private pane is hidden from guests", async () => {
  await owner.evaluate((p) => window.__arugula.client.paneOp(p, { op: "set_private", on: true }), local);
  await expect.poll(() => guest.evaluate((p) => window.__arugula.client.info(p)?.private, local)).toBe(true);
  expect((await api(`/api/panes/${local}/capture`, undefined, asGuest)).status).toBe(404);
  await guest.evaluate((p) => window.__arugula.client.selectTab(window.__arugula.client.tabOfPane(p)!.id), local);
  await expect(guest.locator(`[data-pane="${local}"] .pane-private`)).toBeVisible();
  // Clean up the guest's VMs: closing their tabs deletes them.
  const vmTabs = await owner.evaluate(() => {
    const c = window.__arugula.client;
    return c.state!.tabs.filter((t) => c.tabMachine(t.id)).map((t) => t.id);
  });
  for (const t of vmTabs) await owner.evaluate((tab) => window.__arugula.client.intent({ op: "close_tab", tab }), t);
  await expect.poll(() => owner.evaluate(() => window.__arugula.client.state!.machines.length), { timeout: 30_000 }).toBe(0);
});
