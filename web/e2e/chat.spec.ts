// The chat page: every thread (M61) on every machine in one place. Two
// daemons stand in for geek (the page's own) and jake-mini. Threads are
// started through the API; the bar's Chat place counts what's unread, the
// page (M73) covers the panes and their bar with its own, lists sessions
// as channels with their panes' threads under them, a thread is read and
// written there (an owner's @ of someone who can't see it offers to invite
// them, #297), and "Go to pane" goes back to the pane, on this host or the
// other one.

import { spawn, type ChildProcess } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test, type Page } from "@playwright/test";
import { ready, run } from "./helpers";
import { ANY, daemonPort } from "./ports";
import { labs } from "./labs";

let homeUrl = "";
const dirs: string[] = [];
const daemons: ChildProcess[] = [];
const portOf = new Map<string, number>();

test.use({ baseURL: async ({}, use) => use(homeUrl) });
test.describe.configure({ mode: "serial" });

async function startDaemon(name: string, extra: string[] = []) {
  const state = mkdtempSync(join(tmpdir(), `ilg-e2e-chat-${name}-`));
  dirs.push(state);
  const d = spawn(
    "../target/debug/arugulad",
    [
      ...["--listen", ANY, "--name", name, "--state-dir", labs(state)],
      ...["--shell", "bash --norc --noprofile", "--no-manager-env", "--tailscale-socket", "/nonexistent/tailscaled.sock"],
      ...extra,
    ],
    { stdio: "ignore" },
  );
  daemons.push(d);
  const port = await daemonPort(state, d);
  portOf.set(name, port);
  for (let i = 0; i < 100; i++) {
    try {
      if ((await fetch(`http://127.0.0.1:${port}/api/host`)).ok) return port;
    } catch {
      // not yet
    }
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error(`daemon ${name} did not start`);
}

const api = (name: string, path: string, body?: unknown) =>
  fetch(`http://127.0.0.1:${portOf.get(name)}${path}`, {
    method: body === undefined ? "GET" : "POST",
    headers: body === undefined ? {} : { "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });

/** A host's first session and pane, from what the fleet knows of it. */
async function firstPane(page: Page, name: string): Promise<{ session: number; pane: number }> {
  await expect.poll(() => page.evaluate((n) => window.__arugula.fleet.host(n)?.summary?.panes.length ?? 0, name)).toBeGreaterThan(0);
  return page.evaluate((n) => {
    const st = window.__arugula.fleet.host(n)!.summary!;
    return { session: st.sessions[0].id, pane: st.panes[0].id };
  }, name);
}

test.beforeAll(async () => {
  homeUrl = `http://127.0.0.1:${await startDaemon("geek")}`;
  await startDaemon("jake-mini", ["--allow-origin", homeUrl]);
  const res = await fetch(`${homeUrl}/api/hosts`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ name: "jake-mini", urls: [`http://127.0.0.1:${portOf.get("jake-mini")}`] }),
  });
  expect(res.ok).toBe(true);
});

test.afterAll(() => {
  for (const d of daemons) d.kill("SIGKILL");
  for (const d of dirs) rmSync(d, { recursive: true, force: true });
});

let page: Page;
let home: { session: number; pane: number };
let mini: { session: number; pane: number };

test("the bar counts unread threads on every host", async ({ browser }) => {
  page = await (await browser.newContext()).newPage();
  await page.goto("/");
  await expect.poll(() => page.evaluate(() => window.__arugula?.client.connected)).toBe(true);
  await expect
    .poll(() => page.evaluate(() => (window.__arugula?.fleet?.list ?? []).filter((h) => h.state === "connected").length))
    .toBe(2);
  home = await firstPane(page, "geek");
  mini = await firstPane(page, "jake-mini");

  // Posted as someone else would: an agent, through MCP's route.
  const post = (name: string, key: string, text: string) =>
    page.evaluate(
      async ([host, key, text]) => {
        const r = await window.__arugula.fleet.request(host, "POST", `/api/threads/${key}`, { text });
        return r.ok;
      },
      [name, key, text] as const,
    );
  expect(await post("geek", `pane-${home.pane}`, "the build on geek is red")).toBe(true);
  expect(await post("jake-mini", `pane-${mini.pane}`, "tests pass on the mini")).toBe(true);
  expect(await post("geek", `session-${home.session}`, "standup in five")).toBe(true);

  // Your own messages aren't unread: the button shows up, with no count.
  await expect(page.locator("[data-open-chat]")).toBeVisible();
});

test("chat is a page of its own, over the panes and their bar", async () => {
  const pane = page.locator(`[data-pane="${home.pane}"]`);
  const before = await pane.boundingBox();
  await page.locator(".app > .bar [data-open-chat]").click();
  const chat = page.locator("[data-chat]");
  await expect(chat).toBeVisible();
  expect(await page.evaluate(() => location.hash)).toBe("#chat");

  // Its own bar, with the places; the panes' bar is under it and out of reach.
  await expect(chat.locator(".chat-bar [data-open-chat]")).toHaveAttribute("aria-pressed", "true");
  const box = (await chat.boundingBox())!;
  expect(box.y).toBe(0);
  expect(await page.locator(".app > .bar").evaluate((el) => (el as HTMLElement).inert)).toBe(true);
  expect(await page.locator(".app > .main").evaluate((el) => (el as HTMLElement).inert)).toBe(true);
  const covered = await page.locator(".app > .bar .session-button").evaluate((el) => {
    const r = el.getBoundingClientRect();
    return !!document.elementFromPoint(r.x + r.width / 2, r.y + r.height / 2)?.closest("[data-chat]");
  });
  expect(covered).toBe(true);

  // Escape doesn't leave it; browser Back does, to the same pane, laid out
  // as it was (the panes stayed mounted under the page).
  await page.keyboard.press("Escape");
  await expect(chat).toBeVisible();
  await page.goBack();
  await expect(chat).toBeHidden();
  expect(await pane.boundingBox()).toEqual(before);
  expect(await page.locator(".app > .bar").evaluate((el) => (el as HTMLElement).inert)).toBe(false);

  // The Panes place leaves it too.
  await page.locator("[data-open-chat]").click();
  await expect(chat).toBeVisible();
  await chat.locator("[data-open-panes]").click();
  await expect(chat).toBeHidden();
});

test("the sidebar remembers a machine folded away, and can show only what's unread", async () => {
  await page.locator("[data-open-chat]").click();
  const chat = page.locator("[data-chat]");
  const mini = chat.locator(".chat-list section").filter({ has: page.getByRole("heading", { name: "jake-mini" }) });
  await mini.getByRole("button", { name: "jake-mini" }).click();
  await expect(mini.locator(".chat-row")).toHaveCount(0);
  await page.reload();
  await expect(page.locator("[data-chat]")).toBeVisible();
  await expect(mini.locator(".chat-row")).toHaveCount(0);
  await mini.getByRole("button", { name: "jake-mini" }).click();
  await expect(mini.locator(".chat-row").first()).toBeVisible();

  // The workspace menu: new session, mark all read, unread only.
  await chat.locator("[data-chat-workspace]").click();
  await page.locator(".menu").getByText("Show unread only").click();
  // Everything here is read (your own messages are), so only the thread
  // shown stays listed.
  await expect(chat.locator(".chat-row[data-chat-thread]")).toHaveCount(1);
  await expect(chat.locator(".chat-row.selected")).toHaveCount(1);
  await chat.locator("[data-chat-workspace]").click();
  await page.locator(".menu").getByText("Show every channel").click();
  await expect(chat.locator(".chat-row").first()).toBeVisible();
  await chat.locator("[data-open-panes]").click();
});

test("the chat view lists sessions as channels and panes under them", async () => {
  await page.locator("[data-open-chat]").click();
  const chat = page.locator("[data-chat]");
  await expect(chat).toBeVisible();
  await expect(chat.getByRole("heading", { name: "geek" })).toBeVisible();
  await expect(chat.getByRole("heading", { name: "jake-mini" })).toBeVisible();
  await expect(chat.locator(`[data-chat-thread="session-${home.session}"]`).first()).toBeVisible();
  await expect(chat.locator(`[data-chat-thread="pane-${home.pane}"]`).first()).toBeVisible();
  await expect(chat.locator(`[data-chat-thread="pane-${mini.pane}"]`).last()).toBeVisible();
});

test("a thread is read and written in the view, and links to its pane", async () => {
  const chat = page.locator("[data-chat]");
  await chat.locator(".chat-list section").first().locator(`[data-chat-thread="pane-${home.pane}"]`).click();
  await expect(chat.locator(".chat-thread")).toContainText("the build on geek is red");
  await chat.locator(".chat-thread textarea").fill("looking at it");
  await chat.locator(".chat-thread textarea").press("Enter");
  await expect(chat.locator(".chat-thread")).toContainText("looking at it");
  const saved = (await (await api("geek", `/api/threads/pane-${home.pane}`)).json()) as { messages: { text: string }[] };
  expect(saved.messages.map((m) => m.text)).toContain("looking at it");

  // The session's thread is a channel of its own.
  await chat.locator(".chat-list section").first().locator(`[data-chat-thread="session-${home.session}"]`).click();
  await expect(chat.locator(".chat-thread")).toContainText("standup in five");

  // Back to the pane: the view closes and the pane is the active one.
  await chat.locator(".chat-list section").first().locator(`[data-chat-thread="pane-${home.pane}"]`).click();
  await chat.locator("[data-chat-go]").click();
  await expect(chat).toBeHidden();
  expect(await page.evaluate(() => window.__arugula.client.active())).toBe(home.pane);
});

test("@claude in a session channel says it needs a pane's thread; @notreal isn't marked", async () => {
  await page.locator("[data-open-chat]").click();
  const chat = page.locator("[data-chat]");
  await chat.locator(".chat-list section").first().locator(`[data-chat-thread="session-${home.session}"]`).click();
  await expect(chat.locator(".chat-thread")).toContainText("standup in five");
  await chat.locator(".chat-thread textarea").fill("@claude are you there");
  await chat.locator(".chat-thread textarea").press("Enter");
  await expect(chat.locator(".chat-thread .thread-note.unreached")).toHaveText("@claude reaches an agent from its pane's thread");
  await expect(chat.locator(".chat-thread .thread-msg").filter({ hasText: "are you there" }).locator(".mention")).toHaveCount(0);
  await chat.locator(".chat-thread textarea").fill("@notreal hello");
  await chat.locator(".chat-thread textarea").press("Enter");
  await expect(chat.locator(".chat-thread .thread-note.unreached")).toHaveText("Nobody here called notreal can read this thread");
  await expect(chat.locator(".chat-thread .thread-msg").filter({ hasText: "@notreal hello" }).locator(".mention")).toHaveCount(0);

  // Someone known here who can't see it: the owner is offered to invite
  // them, and told what they'd see (#297).
  const other = ((await (await api("geek", "/api/run", { session: "other" })).json()) as { pane: number }).pane;
  const panes = (await (await api("geek", "/api/panes")).json()) as { id: number; session: number }[];
  const session = panes.find((p) => p.id === other)!.session;
  await api("geek", "/api/acl", { session, principal: "tailnet:sam@example.com", role: "viewer" });
  await chat.locator(".chat-thread textarea").fill("@sam can you look");
  await chat.locator(".chat-thread textarea").press("Enter");
  const offer = chat.locator('.chat-thread .thread-offer[data-offer="tailnet:sam@example.com"]');
  await expect(offer).toContainText("sam can't see this. Invite them?");
  await expect(offer.locator(".thread-offer-sees")).toHaveText("sam will see this message and what follows in this thread");
  await offer.getByLabel("Share the whole thread").check();
  await expect(offer.locator(".thread-offer-sees")).toHaveText("sam will see all of this thread, and no other");
  await offer.getByRole("button", { name: "Not now" }).click();
  await expect(offer).toHaveCount(0);
  await chat.locator("[data-open-panes]").click();
  await expect(chat).toBeHidden();
});

test("another host's thread reads there, and its pane opens on that host", async () => {
  await page.locator("[data-open-chat]").click();
  const chat = page.locator("[data-chat]");
  const miniList = chat.locator(".chat-list section").filter({ has: page.getByRole("heading", { name: "jake-mini" }) });
  await miniList.locator(`[data-chat-thread="pane-${mini.pane}"]`).click();
  await expect(chat.locator(".chat-thread")).toContainText("tests pass on the mini");
  await expect(chat.locator(".chat-title")).toContainText("jake-mini");
  await chat.locator(".chat-thread textarea").fill("nice");
  await chat.locator(".chat-thread textarea").press("Enter");
  await expect
    .poll(async () => ((await (await api("jake-mini", `/api/threads/pane-${mini.pane}`)).json()) as { messages: { text: string }[] }).messages.map((m) => m.text))
    .toContain("nice");

  await chat.locator("[data-chat-go]").click();
  await expect(chat).toBeHidden();
  await expect.poll(() => page.evaluate(() => window.__arugula.hosts.current)).toBe("jake-mini");
  await expect.poll(() => page.evaluate(() => window.__arugula.client.active())).toBe(mini.pane);
});

test("details show the pane live, and a draft waits in its thread", async () => {
  await ready(page, home.pane);
  await run(page, home.pane, "echo PEEK-$((6*7))", "PEEK-42");
  await page.locator("[data-open-chat]").click();
  const chat = page.locator("[data-chat]");
  const geek = chat.locator(".chat-list section").first();
  await geek.locator(`[data-chat-thread="pane-${home.pane}"]`).click();
  await chat.locator("[data-chat-details]").click();
  await expect(chat.locator(`[data-chat-peek="${home.pane}"]`)).toContainText("PEEK-42");

  // A session's details show its panes, each opening its own thread.
  await geek.locator(`[data-chat-thread="session-${home.session}"]`).click();
  await expect(chat.locator(`[data-chat-peek="${home.pane}"]`)).toBeVisible();
  await expect(chat.locator("[data-chat-topic]")).toContainText("pane");

  // Half a message stays with its thread while you look at another.
  await chat.locator(".chat-thread textarea").fill("half a thought");
  await geek.locator(`[data-chat-thread="pane-${home.pane}"]`).click();
  await expect(chat.locator(".chat-thread textarea")).toHaveValue("");
  await geek.locator(`[data-chat-thread="session-${home.session}"]`).click();
  await expect(chat.locator(".chat-thread textarea")).toHaveValue("half a thought");
  await chat.locator(".chat-thread textarea").fill("");
  await chat.locator("[data-chat-details]").click();
  await expect(chat.locator("[data-chat-details-panel]")).toHaveCount(0);
  await chat.locator("[data-open-panes]").click();
});

test("on a phone: the list, then a thread, and back", async ({ browser }) => {
  const phone = await (await browser.newContext({ viewport: { width: 390, height: 760 }, isMobile: true, hasTouch: true })).newPage();
  await phone.goto("/");
  await expect.poll(() => phone.evaluate(() => window.__arugula?.client.connected)).toBe(true);
  await phone.locator(".sheet-button").tap();
  await phone.locator("[data-open-chat]").tap();
  const chat = phone.locator("[data-chat]");
  await expect(chat).toBeVisible();
  expect((await chat.boundingBox())!.width).toBeGreaterThan(380);
  await chat.locator(`[data-chat-thread="pane-${home.pane}"]`).first().tap();
  await expect(chat.locator(".chat-thread")).toContainText("looking at it");
  await chat.locator(".chat-back").tap();
  await expect(chat.locator(".chat-list")).toBeVisible();
  await chat.locator(".thread-close").tap();
  await expect(chat).toBeHidden();
});
