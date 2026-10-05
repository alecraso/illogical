// M61: threads on panes and sessions. The owner, a friend who drives (on a
// laptop and a phone) and a watcher talk about one pane: messages arrive
// live, each person's unread badge is their own, an @mention lights up for
// the person named, a quote jumps back to the output it came from, a
// watcher reads but can't post, and the session has a thread of its own.

import { spawn, type ChildProcess } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test, type Page } from "@playwright/test";
import { ready, run } from "./helpers";
import { deliver, tap } from "./phones";
import { ANY, daemonPort } from "./ports";

let base = "";
const OWNER = "me@example.com";
const FRIEND = "friend@example.com";
const WATCHER = "watcher@example.com";
let daemon: ChildProcess;
let dir: string;

test.describe.configure({ mode: "serial" });
test.use({ baseURL: async ({}, use) => use(base) });

test.beforeAll(async () => {
  dir = mkdtempSync(join(tmpdir(), "illogical-e2e-threads-"));
  daemon = spawn(
    "../target/debug/illogicald",
    [
      ...["--listen", ANY, "--state-dir", dir, "--owner", OWNER],
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

test.afterAll(() => {
  daemon?.kill("SIGKILL");
  rmSync(dir, { recursive: true, force: true });
});

const api = (path: string, body?: unknown) =>
  fetch(base + path, {
    method: body === undefined ? "GET" : "POST",
    headers: body === undefined ? {} : { "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });

async function openAs(page: Page) {
  await page.goto("/");
  await expect.poll(() => page.evaluate(() => window.__illogical?.client.connected)).toBe(true);
}

let owner: Page;
let friend: Page;
let phone: Page;
let watcher: Page;
let session = 0;
let pane = 0;

test("a message reaches everyone on the pane, live", async ({ browser }) => {
  owner = await (await browser.newContext()).newPage();
  await openAs(owner);
  [session, pane] = await owner.evaluate(() => {
    const c = window.__illogical.client;
    return [c.state!.sessions[0].id, c.state!.panes[0].id];
  });
  await api("/api/acl", { session, principal: `tailnet:${FRIEND}`, role: "editor" });
  await api("/api/acl", { session, principal: `tailnet:${WATCHER}`, role: "viewer" });
  const as = (who: string) => ({ extraHTTPHeaders: { "tailscale-user-login": who } });
  friend = await (await browser.newContext(as(FRIEND))).newPage();
  phone = await (await browser.newContext({ ...as(FRIEND), viewport: { width: 390, height: 760 }, isMobile: true, hasTouch: true })).newPage();
  watcher = await (await browser.newContext(as(WATCHER))).newPage();
  for (const p of [friend, phone, watcher]) await openAs(p);

  // The owner opens the pane's thread from its menu and writes.
  await owner.locator(`[data-pane="${pane}"]`).click({ button: "right", position: { x: 60, y: 60 } });
  await owner.getByRole("menuitem", { name: "Thread", exact: true }).click();
  const panel = owner.locator(".thread-panel");
  await expect(panel).toContainText(`Thread · %${pane}`);
  await panel.locator("textarea").fill("the deploy script hangs, @friend can you look?");
  await panel.locator("textarea").press("Enter");
  await expect(panel.locator(".thread-msg")).toContainText("the deploy script hangs");
  // @friend reached someone, so it's marked.
  await expect(panel.locator(".thread-text b")).toHaveText("@friend");

  // The friend's badge says it's for them; the watcher's just that it's new.
  const badge = (p: Page) => p.locator(`[data-pane="${pane}"] .thread-badge`);
  await expect(badge(friend)).toHaveClass(/mention/);
  await expect(badge(friend)).toContainText("@ 1");
  await expect(badge(watcher)).toHaveClass(/unread/);
  await expect(badge(watcher)).not.toHaveClass(/mention/);
  // The owner wrote it: nothing new for them.
  await expect(badge(owner)).not.toHaveClass(/unread/);

  // The friend opens it from the badge (reading it clears their badge on
  // both their clients) and answers while the owner watches it arrive.
  await badge(friend).click();
  await expect(friend.locator(".thread-panel .thread-msg")).toContainText("the deploy script hangs");
  await expect(badge(phone)).not.toHaveClass(/unread/);
  await friend.locator(".thread-panel textarea").fill("on it @me");
  await friend.locator(".thread-panel textarea").press("Enter");
  await expect(panel.locator(".thread-msg")).toHaveCount(2);
  await expect(panel.locator(".thread-msg").last()).toContainText("on it @me");
  await expect(panel.locator(".thread-msg").last()).toHaveClass(/for-me/);
  // The owner has it open, so it's read as it arrives.
  await expect(badge(owner)).not.toHaveClass(/unread/);
});

test("an @ that reached no one stays plain and tells only its poster", async () => {
  const panel = owner.locator(".thread-panel");
  await panel.locator("textarea").fill("@notreal what's up");
  await panel.locator("textarea").press("Enter");
  const msg = panel.locator(".thread-msg").filter({ hasText: "what's up" });
  await expect(msg).toBeVisible();
  await expect(msg.locator("b")).toHaveCount(0);
  await expect(panel.locator(".thread-note.unreached")).toHaveText("Nobody here called notreal can read this thread");
  // Typing again clears the note.
  await panel.locator("textarea").fill("x");
  await expect(panel.locator(".thread-note.unreached")).toHaveCount(0);
  await panel.locator("textarea").fill("");

  // Not the friend's page: nothing is said to them, and it's plain there too.
  await friend.locator(".thread-panel .thread-msg").filter({ hasText: "what's up" }).waitFor();
  await expect(friend.locator(".thread-panel .thread-note.unreached")).toHaveCount(0);
  await expect(friend.locator(".thread-panel .thread-msg").filter({ hasText: "what's up" }).locator("b")).toHaveCount(0);
});

test("a quote jumps back to the output it came from", async () => {
  await ready(owner, pane);
  await run(owner, pane, "echo QUOTE-$((6*7)); seq 1 5", "QUOTE-42");
  await api(`/api/threads/pane-${pane}`, { text: "this line?", quote: { pane, text: "QUOTE-42" } });
  const quote = owner.locator(".thread-panel .thread-quote").last();
  await expect(quote).toContainText("QUOTE-42");
  await quote.click();
  await expect.poll(() => owner.evaluate((p) => window.__illogical.client.panes.get(p)!.view.selection(), pane)).toContain("QUOTE-42");
});

test("a watcher reads but can't post", async () => {
  await watcher.locator(`[data-pane="${pane}"] .thread-badge`).click();
  const panel = watcher.locator(".thread-panel");
  await expect(panel.locator(".thread-msg")).toHaveCount(4);
  await expect(panel.locator("textarea")).toHaveCount(0);
  await expect(panel).toContainText("you can read its threads, not post");
  // Nor through the API.
  const r = await watcher.evaluate((p) => window.__illogical.client.postThread({ pane: p }, "sneaky").then(() => "posted", (e: Error) => e.message), pane);
  expect(r).toContain("watching");
});

test("on a phone the thread takes the screen", async () => {
  await phone.locator(`[data-pane="${pane}"] .thread-badge`).tap();
  const panel = phone.locator(".thread-panel.phone");
  await expect(panel).toBeVisible();
  await expect(panel.locator(".thread-msg")).toHaveCount(4);
  const box = await panel.boundingBox();
  expect(box!.width).toBeGreaterThan(380);
  await panel.locator(".thread-close").tap();
  await expect(panel).toHaveCount(0);
});

test("the session has a thread of its own", async () => {
  await owner.keyboard.press("Escape");
  await owner.locator(".session-button").click();
  await owner.getByRole("menuitem", { name: "Session thread" }).click();
  const panel = owner.locator(".thread-panel");
  await expect(panel).toContainText("Session thread");
  await panel.locator("textarea").fill("standup in 5");
  await panel.locator("textarea").press("Enter");
  await expect(panel.locator(".thread-msg")).toHaveCount(1);
  // The friend sees the session button light up, and the menu says how many.
  await expect(friend.locator(".session-button .session-unread")).toBeVisible();
  await friend.keyboard.press("Escape");
  await friend.locator(".session-button").click();
  await friend.getByRole("menuitem", { name: "Session thread (1 new)" }).click();
  await expect(friend.locator(".thread-panel .thread-msg")).toContainText("standup in 5");
  await expect(friend.locator(".session-button .session-unread")).toHaveCount(0);
});

test("a mention's notification opens its thread", async ({ browser }) => {
  const context = await browser.newContext({ extraHTTPHeaders: { "tailscale-user-login": FRIEND } });
  await context.grantPermissions(["notifications"]);
  const page = await context.newPage();
  await openAs(page);
  // What the daemon pushes for a mention: its own tag, and the thread.
  const tag = `thread-pane-${pane}`;
  await deliver(context, page, { title: "me mentioned you", body: "@friend look", pane, tag, thread: `pane-${pane}` });
  // The worker keeps the thread in what the notification carries...
  const worker = context.serviceWorkers()[0];
  const data = await worker.evaluate(async (tag) => {
    const [n] = await (self as unknown as { registration: ServiceWorkerRegistration }).registration.getNotifications({ tag });
    return n.data as { pane: number; thread: string };
  }, tag);
  expect(data).toMatchObject({ pane, thread: `pane-${pane}` });
  // ...so a tap opens the thread over the pane.
  await expect(page.locator(".thread-panel")).toHaveCount(0);
  await tap(context, tag, "");
  await expect(page.locator(".thread-panel")).toContainText(`Thread · %${pane}`);
  await expect(page.locator(".thread-panel .thread-msg")).toHaveCount(4);
  await context.close();
});

test("an older daemon (no threads feature) gets no thread items", async () => {
  await owner.keyboard.press("Escape");
  // What `GET /api/host` says on a daemon from before M61.
  await owner.route("**/api/host", async (r) => {
    const res = await r.fetch();
    const host = (await res.json()) as { features?: Record<string, boolean> };
    delete host.features?.threads;
    await r.fulfill({ response: res, json: host });
  });
  await owner.locator(`[data-pane="${pane}"]`).click({ button: "right", position: { x: 60, y: 60 } });
  await expect(owner.getByRole("menuitem", { name: "Split right" })).toBeVisible();
  await expect(owner.getByRole("menuitem", { name: "Thread", exact: true })).toHaveCount(0);
  await owner.keyboard.press("Escape");
  await owner.unroute("**/api/host");
});
