// The default, which is what a stranger gets: a machine without the `labs`
// file shows no chat, threads or huddles, no Fountain, studio, VM or
// sandbox, no "Invite over ssh", and one swarm theme. The rest of the suite
// runs with labs on (e2e/labs.ts), so this is the only proof of the default,
// and it is in the set that always runs (`E2E_SET=rest`).
//
// The daemon here is set up for every one of those (a Fountain login, a
// linked studio, a sandbox provider, guest ssh), so only the missing file
// hides them: and the last test makes the file, on the running daemon, and
// loads the page again to see them all.

import { spawn, type ChildProcess } from "node:child_process";
import { existsSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test, type Page } from "@playwright/test";
import { closeContexts, open, paneEl, ready } from "./helpers";
import { ANY, daemonPort } from "./ports";

const OWNER = "me@example.com";
const FRIEND = "friend@example.com";
let base = "";
let state = "";
let daemon: ChildProcess | undefined;

test.use({ baseURL: async ({}, use) => use(base) });
test.describe.configure({ mode: "serial" });
test.afterAll(closeContexts);

test.beforeAll(async () => {
  state = mkdtempSync(join(tmpdir(), "illogical-e2e-labs-off-"));
  // Set up for Fountain, the studio and VMs, so that only labs is missing.
  writeFileSync(join(state, "fountain-credentials"), '[default]\napi_key = "ftn_test_e2e"\n');
  writeFileSync(join(state, "studio.json"), JSON.stringify({ url: "http://127.0.0.1:9", token: "e2e-studio-token" }));
  writeFileSync(join(state, "wisp-token"), "e2e-wisp-token");
  daemon = spawn(
    "../target/debug/illogicald",
    [
      ...["--listen", ANY, "--state-dir", state, "--shell", "bash --norc --noprofile", "--no-manager-env"],
      ...["--owner", OWNER, "--tailscale-socket", "/nonexistent/tailscaled.sock"],
      ...["--wisp-url", "http://127.0.0.1:9", "--wisp-token-file", join(state, "wisp-token")],
      ...["--guest-ssh", "127.0.0.1:0", "--guest-ssh-host", "127.0.0.1"],
    ],
    {
      stdio: "ignore",
      env: { ...process.env, ILLOGICAL_FOUNTAIN_CREDENTIALS: join(state, "fountain-credentials") },
    },
  );
  base = `http://127.0.0.1:${await daemonPort(state, daemon)}`;
});

test.afterAll(() => {
  daemon?.kill("SIGKILL");
  if (state) rmSync(state, { recursive: true, force: true });
});

const api = (path: string, body?: unknown, as?: string) =>
  fetch(base + path, {
    method: body === undefined ? "GET" : "POST",
    headers: { ...(body === undefined ? {} : { "content-type": "application/json" }), ...(as ? { "tailscale-user-login": as } : {}) },
    body: body === undefined ? undefined : JSON.stringify(body),
  });

const features = async () => ((await (await api("/api/host")).json()) as { features: Record<string, boolean> }).features;

/** What this page shows to a stranger's eye, wherever labs would put it. */
const MENU_HIDDEN = [/^Thread\b/, /^Session thread/, /huddle/i, /\bVM\b/, /^Sandboxes/, /^Fountain/, /studio app/i, /^Invite over ssh/];

async function menuItems(page: Page) {
  const items = (await page.getByRole("menuitem").allInnerTexts()).map((s) => s.trim());
  expect(items.length).toBeGreaterThan(3);
  return items;
}

async function noLabsItems(page: Page, pane: number) {
  // The pane's menu: still the usual, and still the read-only link.
  await paneEl(page, pane).click({ button: "right", position: { x: 60, y: 60 } });
  await expect(page.getByRole("menuitem", { name: "Share read-only link…" })).toBeVisible();
  for (const item of await menuItems(page)) for (const h of MENU_HIDDEN) expect(item, `pane menu: ${item}`).not.toMatch(h);
  await expect(page.getByRole("menuitem", { name: "Invite over ssh…" })).toHaveCount(0);
  await page.keyboard.press("Escape");
  // The session menu and the + button's.
  for (const [where, button] of [
    [page.locator(".session-button"), "left"],
    [page.locator(".new-tab"), "right"],
  ] as const) {
    await where.click({ button });
    await expect(page.getByRole("menuitem").first()).toBeVisible();
    for (const item of await menuItems(page)) for (const h of MENU_HIDDEN) expect(item, `menu: ${item}`).not.toMatch(h);
    await page.keyboard.press("Escape");
  }
}

test("the host says labs is off, and what follows it", async () => {
  expect(await features()).toEqual({ labs: false, blocks: false, vms: false, fountain: false, studio: false, threads: false, calls: false });
});

test("on a desktop: no chat, threads or huddles, no Fountain, studio or VMs, no ssh invite", async ({ page }) => {
  await open(page);
  const pane = await page.evaluate(() => window.__illogical.client.state!.panes[0].id);
  await ready(page, pane);
  await expect.poll(() => page.evaluate(() => window.__illogical.client.features !== null)).toBe(true);
  expect(await page.evaluate(() => window.__illogical.client.hasLabs())).toBe(false);

  // The bar: Panes and Swarm, no Chat, no huddle button.
  await expect(page.locator("[data-open-swarm]")).toBeVisible();
  await expect(page.locator("[data-open-chat]")).toHaveCount(0);
  await expect(page.locator("[data-huddle]")).toHaveCount(0);
  await noLabsItems(page, pane);

  // The palette has no chat entry (and does have the rest).
  await page.keyboard.press("ControlOrMeta+Shift+KeyP");
  const palette = page.getByRole("dialog", { name: "Command palette" });
  await expect(palette).toBeVisible();
  await palette.getByRole("textbox", { name: "Command" }).fill("chat");
  await expect(palette.getByRole("option", { name: /chat/i })).toHaveCount(0);
  await palette.getByRole("textbox", { name: "Command" }).fill("split");
  await expect(palette.getByRole("option").first()).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(palette).toBeHidden();
});

test("a thread message posted through the API shows nowhere", async ({ page }) => {
  await open(page);
  const [pane, session] = await page.evaluate(() => {
    const c = window.__illogical.client;
    return [c.state!.panes[0].id, c.state!.sessions[0].id];
  });
  await ready(page, pane);
  // A friend who drives posts, so there's something new for the owner.
  expect((await api("/api/acl", { session, principal: `tailnet:${FRIEND}`, role: "editor" })).ok).toBe(true);
  expect((await api(`/api/threads/pane-${pane}`, { text: "anyone here? @me" }, FRIEND)).ok).toBe(true);
  expect((await api(`/api/threads/session-${session}`, { text: "standup in 5" }, FRIEND)).ok).toBe(true);
  // The daemon still sends the page its threads (this is visibility, not
  // enforcement)...
  await expect
    .poll(() => page.evaluate(() => (window.__illogical.client.state!.threads ?? []).filter((t) => t.unread).length))
    .toBeGreaterThan(0);
  // ...and the page shows no sign of them: no badge, no unread dot, no thread.
  await expect(page.locator(".thread-badge")).toHaveCount(0);
  await expect(page.locator(".session-unread")).toHaveCount(0);
  await expect(page.locator(".thread-panel")).toHaveCount(0);
  // Nor a huddle on the session, were there one.
  expect(
    await page.evaluate((s) => {
      const c = window.__illogical.client;
      c.state!.calls = [{ session: s, id: "x", started: 0, members: [] }];
      return c.call(s);
    }, session),
  ).toBeUndefined();
  expect(await page.evaluate(([p, s]) => [window.__illogical.client.thread({ pane: p }), window.__illogical.client.thread({ session: s })], [pane, session])).toEqual([undefined, undefined]);
  await expect(page.locator("[data-open-chat]")).toHaveCount(0);
  await noLabsItems(page, pane);
});

test("the swarm has one theme and no picker, whatever this browser saved", async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem("illogical.swarm.theme", "city"));
  await open(page);
  await page.goto("/#swarm");
  await expect(page.locator(".swarm")).toBeVisible();
  await expect(page.locator(".swarm-bar")).toBeVisible();
  await expect.poll(() => page.evaluate(() => window.__illogical.client.features !== null)).toBe(true);
  await expect(page.locator(".swarm")).toHaveAttribute("data-theme", "blocks");
  await expect(page.locator("[data-theme-pick]")).toHaveCount(0);
  await expect(page.getByText("Theme", { exact: true })).toHaveCount(0);
  await expect(page.locator("canvas.swarm-city")).toHaveCount(0);
  // Its tiles show no unread, though the daemon has threads with some.
  expect(await page.evaluate(() => (window.__illogical.client.state!.threads ?? []).some((t) => t.unread))).toBe(true);
  expect(await page.evaluate(() => window.__illogical.fleet.panes.map((p) => [p.unread, p.mention]))).not.toEqual([]);
  expect(await page.evaluate(() => window.__illogical.fleet.panes.every((p) => !p.unread && !p.mention))).toBe(true);
  // The rest of the bar is still there.
  await expect(page.locator('[data-g="machine"]')).toBeVisible();
});

test("a mention's notification opens only the pane", async ({ page }) => {
  const pane = ((await (await api("/api/panes")).json()) as { id: number }[])[0].id;
  await api(`/api/threads/pane-${pane}`, { text: "look at this @me" });
  // A cold load, as a tap on the notification is: not a navigation within a page.
  await page.goto(`/#pane=${pane}&thread=pane-${pane}`);
  await expect.poll(() => page.evaluate(() => window.__illogical?.client.connected)).toBe(true);
  await expect.poll(() => page.evaluate((p) => window.__illogical.client.active() === p, pane)).toBe(true);
  // Features are read before the thread would open: wait for them, and then
  // for a moment more, since the failure is a panel that opens late.
  await expect.poll(() => page.evaluate(() => window.__illogical.client.features !== null)).toBe(true);
  await page.waitForTimeout(500);
  await expect(page.locator(".thread-panel")).toHaveCount(0);
  await expect(paneEl(page, pane)).toBeVisible();
});

test.describe("on a phone", () => {
  test.use({ viewport: { width: 390, height: 760 }, isMobile: true, hasTouch: true });
  test("the sheet has no chat, Fountain, studio or VM entries", async ({ page }) => {
    await open(page);
    await expect.poll(() => page.evaluate(() => window.__illogical.client.features !== null)).toBe(true);
    await page.locator(".sheet-button").tap();
    // What it does have.
    await expect(page.locator("[data-open-swarm]")).toBeVisible();
    await expect(page.locator("[data-open-pr]")).toBeVisible();
    for (const gone of ["[data-open-chat]", "[data-studio-apps]", "[data-open-fountain]"]) await expect(page.locator(gone)).toHaveCount(0);
    for (const name of ["New VM tab", "Sandboxes"]) await expect(page.getByRole("button", { name })).toHaveCount(0);
    await expect(page.locator(".session-unread, .thread-badge, [data-huddle]")).toHaveCount(0);
  });
});

test("a daemon from before labs, which says nothing of it, gets none of it either", async ({ page }) => {
  // What `GET /api/host` says on an older daemon: every feature on, and no
  // `labs`. Control serves this page to such daemons too.
  await page.route("**/api/host", async (r) => {
    const res = await r.fetch();
    const host = (await res.json()) as { features?: Record<string, boolean> };
    host.features = { blocks: false, vms: true, fountain: true, studio: true, threads: true, calls: true };
    await r.fulfill({ response: res, json: host });
  });
  await open(page);
  const pane = await page.evaluate(() => window.__illogical.client.state!.panes[0].id);
  await ready(page, pane);
  await expect.poll(() => page.evaluate(() => window.__illogical.client.features?.vms)).toBe(true);
  expect(await page.evaluate(() => [window.__illogical.client.hasLabs(), window.__illogical.client.hasThreads(), window.__illogical.client.hasCalls()])).toEqual([false, false, false]);
  expect(await page.evaluate(() => (["vms", "fountain", "studio"] as const).map((f) => window.__illogical.client.has(f)))).toEqual([false, false, false]);
  await expect(page.locator("[data-open-chat]")).toHaveCount(0);
  await expect(page.locator("[data-huddle]")).toHaveCount(0);
  await expect(page.locator(".thread-badge, .session-unread")).toHaveCount(0);
  await noLabsItems(page, pane);
});

test("making the file on the running daemon brings it all back, with no restart", async ({ page }) => {
  expect(existsSync(join(state, "labs"))).toBe(false);
  writeFileSync(join(state, "labs"), "");
  // Something new for the owner, from the friend.
  const pane0 = ((await (await api("/api/panes")).json()) as { id: number }[])[0].id;
  expect((await api(`/api/threads/pane-${pane0}`, { text: "still there?" }, FRIEND)).ok).toBe(true);
  expect(await features()).toEqual({ labs: true, blocks: false, vms: true, fountain: true, studio: true, threads: true, calls: true });

  await open(page);
  const pane = await page.evaluate(() => window.__illogical.client.state!.panes[0].id);
  await ready(page, pane);
  await expect.poll(() => page.evaluate(() => window.__illogical.client.hasLabs())).toBe(true);
  await expect(page.locator("[data-open-chat]")).toBeVisible();
  await expect(page.locator("[data-huddle]").first()).toBeVisible();
  await expect(page.locator(".thread-badge.unread")).toBeVisible();
  await expect(page.locator(".session-unread")).toBeVisible();
  await paneEl(page, pane).click({ button: "right", position: { x: 60, y: 60 } });
  for (const name of ["Invite over ssh…", "New VM pane on the right", "Open a studio app…", "Fountain agents…"]) {
    await expect(page.getByRole("menuitem", { name })).toBeVisible();
  }
  await expect(page.getByRole("menuitem", { name: /^Thread/ })).toBeVisible();
  await page.keyboard.press("Escape");

  await page.goto("/#swarm");
  await expect(page.locator("[data-theme-pick]")).toHaveCount(4);
  await expect.poll(() => page.evaluate(() => window.__illogical.fleet.panes.some((p) => p.unread > 0))).toBe(true);
});
