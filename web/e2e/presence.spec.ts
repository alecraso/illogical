// M13: live sharing and presence. The owner and a friend (an editor, on a
// laptop and a phone) in one session: each sees the others' avatars and
// focus; control passes back and forth and nobody's keystrokes interleave;
// history says who ran what. A "from now" viewer can't reach output from
// before they were let in, by scrolling, tail or capture.

import { spawn, type ChildProcess } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test, type Page } from "@playwright/test";
import { ready, run, text, closeContexts } from "./helpers";
import { ANY, daemonPort } from "./ports";
import { labs } from "./labs";

test.afterAll(closeContexts);

let base = "";
const OWNER = "me@example.com";
const FRIEND = "friend@example.com";
const LATE = "late@example.com";
let daemon: ChildProcess;
let dir: string;

test.describe.configure({ mode: "serial" });
test.use({ baseURL: async ({}, use) => use(base) });

test.beforeAll(async () => {
  dir = mkdtempSync(join(tmpdir(), "illogical-e2e-presence-"));
  daemon = spawn(
    "../target/debug/illogicald",
    [
      ...["--listen", ANY, "--state-dir", labs(dir), "--owner", OWNER],
      ...["--shell", "bash --norc --noprofile", "--no-manager-env", "--tailscale-socket", "/nonexistent/sock"],
    ],
    // #118: drivers who stop typing let go after 8 s here.
    { stdio: "ignore", env: { ...process.env, ILLOGICAL_DRIVER_LAPSE_MS: "8000" } },
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

test.afterAll(() => {
  daemon?.kill("SIGKILL");
  rmSync(dir, { recursive: true, force: true });
});

const api = (path: string, body?: unknown, headers: Record<string, string> = {}) =>
  fetch(base + path, {
    method: body === undefined ? "GET" : "POST",
    headers: { ...headers, ...(body === undefined ? {} : { "content-type": "application/json" }) },
    body: body === undefined ? undefined : JSON.stringify(body),
  });

async function openAs(page: Page) {
  await page.goto("/");
  await expect.poll(() => page.evaluate(() => window.__illogical?.client.connected)).toBe(true);
}

let owner: Page;
let friend: Page;
let phone: Page;
let session = 0;
let pane = 0;

test("three people in one session see each other", async ({ browser }) => {
  owner = await (await browser.newContext()).newPage();
  await openAs(owner);
  [session, pane] = await owner.evaluate(() => {
    const c = window.__illogical.client;
    return [c.state!.sessions[0].id, c.state!.panes[0].id];
  });
  await api("/api/acl", { session, principal: `tailnet:${FRIEND}`, role: "editor" });
  // The pane runs on the owner's machine: they trust the friend with it (M14).
  await owner.evaluate(
    ([p, to]) => window.__illogical.client.paneOp(p, { op: "grant_trust", to, minutes: 30 }),
    [pane, `tailnet:${FRIEND}`] as const,
  );
  const headers = { "tailscale-user-login": FRIEND };
  friend = await (await browser.newContext({ extraHTTPHeaders: headers })).newPage();
  phone = await (await browser.newContext({ extraHTTPHeaders: headers, viewport: { width: 390, height: 760 }, isMobile: true, hasTouch: true })).newPage();
  await openAs(friend);
  await openAs(phone);
  for (const p of [owner, friend]) await p.locator(`[data-pane="${pane}"]`).click({ position: { x: 40, y: 40 } });
  // The owner sees the friend (once, for two clients), focused on the pane.
  await expect(owner.locator(`.people [data-who="tailnet:${FRIEND}"]`)).toHaveCount(1);
  await expect(owner.locator(`[data-pane="${pane}"] .pane-person`)).toContainText("friend");
  // The friend sees the owner.
  await expect(friend.locator(`.people [data-who="owner"]`)).toHaveCount(1);
  const seen = await phone.evaluate(() => window.__illogical.client.others().map((p) => p.who));
  expect(seen).toContain("owner");
});

test("control passes back and forth; keystrokes never interleave", async () => {
  await ready(owner, pane);
  await ready(friend, pane);
  // The owner types first, so drives.
  await run(owner, pane, "echo owner-$((6*7))", "owner-42");
  await expect(friend.locator(`[data-pane="${pane}"] [data-driver="owner"]`)).toBeVisible();
  // The friend's typing is held back, with the reason.
  await friend.keyboard.type("echo HIJACK\n");
  await expect(friend.getByText("is driving this pane")).toBeVisible();
  await new Promise((r) => setTimeout(r, 300));
  expect(await text(owner, pane)).not.toContain("HIJACK");
  // The friend takes control; now the owner is the one held back.
  await friend.evaluate((p) => window.__illogical.client.paneOp(p, { op: "take_control" }), pane);
  await expect(owner.getByText("took control")).toBeVisible();
  await run(friend, pane, "echo friend-$((6*7))", "friend-42");
  await owner.locator(`[data-pane="${pane}"]`).click({ position: { x: 40, y: 40 } });
  await owner.keyboard.type("echo OWNER-INTERRUPTS\n");
  await new Promise((r) => setTimeout(r, 300));
  expect(await text(friend, pane)).not.toContain("OWNER-INTERRUPTS");
  // The owner asks; the friend hands over.
  await owner.evaluate((p) => window.__illogical.client.paneOp(p, { op: "request_control" }), pane);
  await friend.locator("[data-give]").click();
  await run(owner, pane, "echo back-$((6*7))", "back-42");
});

test("history says who ran each command", async () => {
  await expect
    .poll(async () => {
      const h = (await (await api(`/api/history?pane=${pane}`)).json()) as { text: string; by?: string }[];
      return h.filter((c) => c.text?.startsWith("echo")).map((c) => `${c.by}: ${c.text}`);
    })
    .toEqual([`${OWNER}: echo owner-$((6*7))`, `${FRIEND}: echo friend-$((6*7))`, `${OWNER}: echo back-$((6*7))`]);
  const handoffs = (await (await api(`/api/panes/${pane}/drivers`)).json()) as { who: string }[];
  expect(handoffs.map((h) => h.who)).toEqual([OWNER, FRIEND, OWNER]);
});

test("a 'from now' viewer can't reach what came before", async ({ browser }) => {
  // Something private, then enough output to push it off the screen.
  await run(owner, pane, "echo BEFORE-$((6*7)); seq 1 200", "BEFORE-42");
  await expect.poll(() => text(owner, pane)).toContain("\n200");
  expect((await api("/api/acl", { session, principal: `tailnet:${LATE}`, role: "viewer", history: false })).ok).toBe(true);
  const headers = { "tailscale-user-login": LATE };
  const late = await (await browser.newContext({ extraHTTPHeaders: headers })).newPage();
  await openAs(late);
  await ready(late, pane);
  await run(owner, pane, "echo AFTER-$((6*7))", "AFTER-42");
  await expect.poll(() => text(late, pane)).toContain("AFTER-42");
  // Not in their terminal, scrollback included.
  expect(await text(late, pane)).not.toContain("BEFORE-42");
  // Nor through the API.
  expect((await api(`/api/panes/${pane}/tail`, undefined, headers)).status).toBe(403);
  expect((await api(`/api/panes/${pane}/capture?scope=scrollback`, undefined, headers)).status).toBe(403);
  expect((await api(`/api/panes/${pane}/export.cast`, undefined, headers)).status).toBe(403);
  const screen = await api(`/api/panes/${pane}/capture`, undefined, headers);
  expect(screen.status).toBe(200);
  expect(await screen.text()).not.toContain("BEFORE-42");
});

test("typing shows for a few seconds; an idle driver lets go (#118)", async () => {
  const seen = (p: Page) =>
    p.evaluate((id) => {
      const i = window.__illogical.client.info(id);
      return { driver: i?.driver?.who ?? null, typing: !!i?.typing };
    }, pane);
  await run(owner, pane, "echo again-$((6*7))", "again-42");
  await expect.poll(() => seen(friend)).toEqual({ driver: "owner", typing: true });
  // A few seconds on it still drives, but isn't typing.
  await expect.poll(() => seen(friend), { timeout: 10_000 }).toEqual({ driver: "owner", typing: false });
  // Then it lets go, and the friend drives by typing, with no take-over.
  await expect.poll(() => seen(friend), { timeout: 15_000 }).toEqual({ driver: null, typing: false });
  await friend.locator(`[data-pane="${pane}"]`).click({ position: { x: 40, y: 40 } });
  await run(friend, pane, "echo lapsed-$((6*7))", "lapsed-42");
  await expect.poll(() => seen(owner)).toEqual({ driver: `tailnet:${FRIEND}`, typing: true });
});
