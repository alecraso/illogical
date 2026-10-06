// M74: a thread's messages look like a team chat's. A session's thread is
// seeded with history across days (written to the daemon's thread file
// while it's stopped, as a restart keeps threads): a line between days, a
// red "New" line at the first unread, pictures, an agent's badge, runs of
// one person under one heading, and a small Markdown in which typed HTML
// stays text. Then @ completes a name and the hover toolbar quotes. M75:
// Activity, search, the Ctrl+K switcher and the channel keys.

import { spawn, type ChildProcess } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test, type Page } from "@playwright/test";
import { ANY, daemonPort } from "./ports";

const OWNER = "me@example.com";
const FRIEND = "friend@example.com";
const PIC = `data:image/svg+xml,${encodeURIComponent('<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8"><rect width="8" height="8" fill="#f00"/></svg>')}`;
let base = "";
let daemon: ChildProcess;
let dir: string;

test.describe.configure({ mode: "serial" });
test.use({ baseURL: async ({}, use) => use(base) });

async function start() {
  // A restart writes its new port here; the last run's would be read first.
  rmSync(join(dir, "listen"), { force: true });
  daemon = spawn(
    "../target/debug/illogicald",
    [
      ...["--listen", ANY, "--name", "box", "--state-dir", dir, "--owner", OWNER],
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
  throw new Error("the daemon didn't start");
}

async function stop() {
  const exited = new Promise((r) => daemon.once("exit", r));
  daemon.kill("SIGTERM");
  await exited;
}

test.beforeAll(async () => {
  dir = mkdtempSync(join(tmpdir(), "illogical-e2e-chat-msgs-"));
  await start();
});

test.afterAll(() => {
  daemon?.kill("SIGKILL");
  rmSync(dir, { recursive: true, force: true });
});

async function openAs(page: Page, hash = "") {
  await page.goto(`${base}/${hash}`);
  await expect.poll(() => page.evaluate(() => window.__illogical?.client.connected)).toBe(true);
}

let session = 0;
let owner: Page;

test("history across days reads like a team chat", async ({ browser }) => {
  const first = await (await browser.newContext()).newPage();
  await openAs(first);
  session = await first.evaluate(() => window.__illogical.client.state!.sessions[0].id);
  await fetch(`${base}/api/acl`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ session, principal: `tailnet:${FRIEND}`, role: "editor" }),
  });
  await first.context().close();
  await stop();

  // Two days ago, yesterday just before midnight, and today: the owner has
  // read up to yesterday's.
  const now = new Date();
  const midnight = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime();
  const friend = { who: `tailnet:${FRIEND}`, name: "Friend Person", pic: PIC };
  const msgs = [
    { id: 1, at: midnight - 2 * 86_400_000 + 3_600_000, ...friend, text: "the old one" },
    { id: 2, at: midnight - 60_000, ...friend, text: "late night" },
    { id: 3, at: Math.min(now.getTime() - 120_000, midnight + 60_000), ...friend, text: "morning, **bold** and `code` and _soft_" },
    { id: 4, at: Math.min(now.getTime() - 110_000, midnight + 90_000), ...friend, text: "same run <script>window.hacked=1</script> https://example.com/x" },
    { id: 5, at: Math.min(now.getTime() - 100_000, midnight + 100_000), who: "mcp:claude", name: "claude", text: "```\nfn main() {}\n```\ndone @me", agent: true, mentions: ["owner"] },
  ];
  writeFileSync(join(dir, "threads", `session-${session}.jsonl`), msgs.map((m) => JSON.stringify(m)).join("\n") + "\n");
  writeFileSync(join(dir, "threads", "reads.json"), JSON.stringify({ owner: { [`session-${session}`]: 2 } }));
  await start();

  owner = await (await browser.newContext({ permissions: ["clipboard-read", "clipboard-write"] })).newPage();
  await openAs(owner, `#chat=box/session-${session}`);
  const thread = owner.locator(".chat-thread");
  await expect(thread.locator(".thread-msg")).toHaveCount(5);

  // A line between days, "Yesterday" and "Today" by name.
  const days = thread.locator(".msg-day");
  await expect(days).toHaveCount(3);
  await expect(days.nth(1)).toHaveText("Yesterday");
  await expect(days.nth(2)).toHaveText("Today");

  // "New" sits right before the first unread (the third message).
  const newLine = thread.locator("[data-new-line]");
  await expect(newLine).toHaveCount(1);
  expect(await newLine.evaluate((el) => (el.nextElementSibling as HTMLElement).dataset.msg)).toBe("3");

  // Pictures at the head of a run; the run's next message has none.
  await expect(thread.locator('[data-msg="1"] .msg-avatar img')).toHaveAttribute("src", PIC);
  await expect(thread.locator('[data-msg="4"] .msg-avatar')).toHaveCount(0);
  await expect(thread.locator('[data-msg="4"] .msg-hover-time')).toHaveCount(1);

  // The agent's message: its badge, a code block, and its @mention of you.
  await expect(thread.locator('[data-msg="5"] .msg-badge')).toHaveText("Agent");
  await expect(thread.locator('[data-msg="5"] .msg-code')).toHaveText("fn main() {}");
  await expect(thread.locator('[data-msg="5"] .mention.me')).toHaveText("@me");
  await expect(thread.locator('[data-msg="5"]')).toHaveClass(/for-me/);

  // Markdown, and HTML that stays text.
  await expect(thread.locator('[data-msg="3"] b')).toHaveText("bold");
  await expect(thread.locator('[data-msg="3"] code')).toHaveText("code");
  await expect(thread.locator('[data-msg="3"] i')).toHaveText("soft");
  await expect(thread.locator('[data-msg="4"] .thread-text')).toContainText("<script>window.hacked=1</script>");
  await expect(thread.locator('[data-msg="4"] script')).toHaveCount(0);
  expect(await owner.evaluate(() => (window as unknown as { hacked?: number }).hacked)).toBeUndefined();
  await expect(thread.locator('[data-msg="4"] a')).toHaveAttribute("href", "https://example.com/x");
});

test("@ completes a name, and Quote in reply quotes", async ({ browser }) => {
  // The friend is here, so they're someone to mention.
  const friend = await (await browser.newContext({ extraHTTPHeaders: { "tailscale-user-login": FRIEND, "tailscale-user-name": "Friend Person" } })).newPage();
  await openAs(friend);
  const box = owner.locator(".chat-thread textarea");
  await box.fill("");
  await box.pressSequentially("hi @fri");
  const pick = owner.locator(".mention-pick");
  await expect(pick).toContainText("friend@example.com");
  await box.press("Enter");
  await expect(pick).toHaveCount(0);
  await expect(box).toHaveValue("hi @friend ");

  await owner.locator('[data-msg="2"]').hover();
  await owner.locator('[data-msg="2"] [data-msg-reply]').click();
  await expect(box).toHaveValue("> late night\nhi @friend ");
  await box.press("Enter");
  await expect(owner.locator(".chat-thread .thread-msg").last()).toContainText("> late night");
  await expect(owner.locator(".chat-thread .thread-msg").last().locator(".mention")).toHaveText("@friend");
  await friend.context().close();
});

test("a copied link opens the message and flashes it", async () => {
  await owner.locator('[data-msg="1"]').hover();
  await owner.locator('[data-msg="1"] [data-msg-link]').click();
  const link = await owner.evaluate(() => navigator.clipboard.readText());
  expect(link).toMatch(new RegExp(`#chat=box/session-${session}&msg=1$`));
  await owner.goto(link);
  await expect(owner.locator('.chat-thread [data-msg="1"]')).toBeInViewport();
  await expect(owner.locator('.chat-thread [data-msg="1"]')).toHaveClass(/flash/);
});

test("Activity has what's for you, and clears when it's read", async () => {
  const pane = await owner.evaluate(() => window.__illogical.client.state!.panes[0].id);
  // The friend mentions the owner in the pane's thread.
  const r = await fetch(`${base}/api/threads/pane-${pane}`, {
    method: "POST",
    headers: { "content-type": "application/json", "tailscale-user-login": FRIEND },
    body: JSON.stringify({ text: "@me the pane is stuck" }),
  });
  expect(r.ok).toBe(true);
  const activity = owner.locator("[data-chat-activity]");
  await expect(activity.locator(".chat-count")).toHaveText("1");
  await activity.click();
  const view = owner.locator("[data-chat-activity-view]");
  await expect(view.locator(".chat-hit.unread")).toContainText("the pane is stuck");
  // The agent's message that mentioned the owner is there too.
  await expect(view.locator('[data-chat-hit="5"]')).toBeVisible();

  // Shift+Esc reads everything.
  await owner.keyboard.press("Shift+Escape");
  await expect(activity.locator(".chat-count")).toHaveCount(0);
  await expect(view.locator(".chat-hit.unread")).toHaveCount(0);

  // A hit opens its message in its thread.
  await view.locator(".chat-hit", { hasText: "the pane is stuck" }).click();
  await expect(owner.locator(".chat-thread .thread-msg.flash")).toContainText("the pane is stuck");
});

test("search finds messages and opens them", async () => {
  const field = owner.locator("[data-chat-search]");
  await field.fill("old one");
  const view = owner.locator("[data-chat-search-view]");
  await expect(view.locator(".chat-hit")).toHaveCount(1);
  await expect(view).toContainText("1 message");
  await view.locator('[data-chat-hit="1"]').click();
  await expect(owner.locator('.chat-thread [data-msg="1"]')).toBeInViewport();
  await expect(field).toHaveValue("");
  await field.fill("nothing like this");
  await expect(view).toContainText("No messages match");
  await field.press("Escape");
  await expect(view).toHaveCount(0);
});

test("Ctrl+K jumps to a channel, and Alt+arrows walk them", async () => {
  const pane = await owner.evaluate(() => window.__illogical.client.state!.panes[0].id);
  const name = await owner.evaluate(() => window.__illogical.client.state!.sessions[0].name);
  await owner.locator(`.chat-row[data-chat-thread="pane-${pane}"]`).click();
  await owner.keyboard.press("Control+k");
  const sw = owner.locator("[data-chat-switcher]");
  await expect(sw).toBeVisible();
  await expect(sw.locator("input")).toBeFocused();
  await owner.keyboard.type(name.split(" ")[0]);
  await owner.keyboard.press("Enter");
  await expect(sw).toHaveCount(0);
  await expect.poll(() => owner.evaluate(() => location.hash)).toBe(`#chat=box/session-${session}`);
  // The hash moves before the page does: wait for the channel to show.
  const selected = owner.locator(".chat-row.selected[data-chat-thread]");
  await expect(selected).toHaveAttribute("data-chat-thread", `session-${session}`);

  // The session's channel, then its pane's thread under it, and back.
  await owner.keyboard.press("Alt+ArrowDown");
  await expect.poll(() => owner.evaluate(() => location.hash)).toBe(`#chat=box/pane-${pane}`);
  await expect(selected).toHaveAttribute("data-chat-thread", `pane-${pane}`);
  await owner.keyboard.press("Alt+ArrowUp");
  await expect.poll(() => owner.evaluate(() => location.hash)).toBe(`#chat=box/session-${session}`);
});
