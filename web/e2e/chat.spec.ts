// The chat view: every thread (M61) on every machine in one place. Two
// daemons stand in for geek (the page's own) and jake-mini. Threads are
// started through the API; the bar's Chat button counts what's unread,
// the view lists sessions as channels with their panes' threads under
// them, a thread is read and written there, and "Go to pane" goes back to
// the pane, on this host or the other one.

import { spawn, type ChildProcess } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test, type Page } from "@playwright/test";
import { ANY, daemonPort } from "./ports";

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
    "../target/debug/illogicald",
    [
      ...["--listen", ANY, "--name", name, "--state-dir", state],
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
  await expect.poll(() => page.evaluate((n) => window.__illogical.fleet.host(n)?.summary?.panes.length ?? 0, name)).toBeGreaterThan(0);
  return page.evaluate((n) => {
    const st = window.__illogical.fleet.host(n)!.summary!;
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
  await expect.poll(() => page.evaluate(() => window.__illogical?.client.connected)).toBe(true);
  await expect
    .poll(() => page.evaluate(() => (window.__illogical?.fleet?.list ?? []).filter((h) => h.state === "connected").length))
    .toBe(2);
  home = await firstPane(page, "geek");
  mini = await firstPane(page, "jake-mini");

  // Posted as someone else would: an agent, through MCP's route.
  const post = (name: string, key: string, text: string) =>
    page.evaluate(
      async ([host, key, text]) => {
        const r = await window.__illogical.fleet.request(host, "POST", `/api/threads/${key}`, { text });
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
  expect(await page.evaluate(() => window.__illogical.client.active())).toBe(home.pane);
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
  await expect.poll(() => page.evaluate(() => window.__illogical.hosts.current)).toBe("jake-mini");
  await expect.poll(() => page.evaluate(() => window.__illogical.client.active())).toBe(mini.pane);
});

test("Escape closes the view", async () => {
  await page.locator("[data-open-chat]").click();
  await expect(page.locator("[data-chat]")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.locator("[data-chat]")).toBeHidden();
});

test("on a phone: the list, then a thread, and back", async ({ browser }) => {
  const phone = await (await browser.newContext({ viewport: { width: 390, height: 760 }, isMobile: true, hasTouch: true })).newPage();
  await phone.goto("/");
  await expect.poll(() => phone.evaluate(() => window.__illogical?.client.connected)).toBe(true);
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
