// M23: pane summaries. A summaries-only connection (what the swarm and the
// fleet use) gets every pane's kind, project and activity as field-level
// deltas, attaches to nothing and makes no terminals. A second person gets
// summaries only for the sessions shared with them, and someone else's
// private pane shows only that it's there.

import { spawn, type ChildProcess } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test, type Page } from "@playwright/test";
import { ANY, daemonPort } from "./ports";
import { closeContexts } from "./helpers";
import { labs } from "./labs";

test.afterAll(closeContexts);

let base = "";
const OWNER = "me@example.com";
const FRIEND = "friend@example.com";
let daemon: ChildProcess;
let dir: string;

test.describe.configure({ mode: "serial" });
test.use({ baseURL: async ({}, use) => use(base) });

const api = (path: string, body?: unknown, headers: Record<string, string> = {}) =>
  fetch(base + path, {
    method: body === undefined ? "GET" : "POST",
    headers: { ...headers, ...(body === undefined ? {} : { "content-type": "application/json" }) },
    body: body === undefined ? undefined : JSON.stringify(body),
  });

test.beforeAll(async () => {
  dir = mkdtempSync(join(tmpdir(), "illogical-e2e-summaries-"));
  mkdirSync(join(dir, "work", "myrepo", ".git"), { recursive: true });
  daemon = spawn(
    "../target/debug/illogicald",
    [
      ...["--listen", ANY, "--state-dir", labs(join(dir, "state")), "--owner", OWNER],
      ...["--shell", "bash --norc --noprofile", "--no-manager-env", "--tailscale-socket", "/nonexistent/sock"],
    ],
    { stdio: "ignore" },
  );
  base = `http://127.0.0.1:${await daemonPort(join(dir, "state"), daemon)}`;
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

let owner: Page;
let shared = 0;
let repoPane = 0;
let privatePane = 0;
let otherPane = 0;

/** Open the app, and a summaries-only client beside it. */
async function openSummaries(page: Page) {
  await page.goto("/");
  await expect.poll(() => page.evaluate(() => window.__illogical?.client.connected)).toBe(true);
  await page.evaluate(() => {
    (window as unknown as { sums: ReturnType<typeof window.__illogical.summaries> }).sums = window.__illogical.summaries();
  });
  await expect.poll(() => page.evaluate(() => (window as any).sums.state !== null)).toBe(true);
}

const sum = (page: Page, pane: number) =>
  page.evaluate((p) => (window as any).sums.state.panes.find((x: { id: number }) => x.id === p) ?? null, pane);

test("a summaries-only client gets kind, project and activity as deltas, and no terminals", async ({ browser }) => {
  owner = await (await browser.newContext()).newPage();
  await openSummaries(owner);
  // Sessions: one to share (a pane in a repo, and a private one), one not.
  const repo = join(dir, "work", "myrepo");
  repoPane = (await (await api("/api/run", { cwd: repo })).json()).pane;
  privatePane = (await (await api("/api/run", { cwd: repo, split: repoPane })).json()).pane;
  await owner.evaluate(() => window.__illogical.client.intent({ op: "new_session", name: "mine", from_pane: null }));
  await expect.poll(() => owner.evaluate(() => window.__illogical.client.state!.sessions.length)).toBe(2);
  [shared, otherPane] = await owner.evaluate((p) => {
    const c = window.__illogical.client;
    const s = c.state!;
    const mine = s.sessions.find((x) => x.name === "mine")!;
    const t = s.tabs.find((x) => x.id === mine.tabs[0])!;
    return [s.sessions.find((x) => x.tabs.includes(c.tabOfPane(p)!.id))!.id, t.layout.panes[0][0]];
  }, repoPane);

  await expect.poll(() => sum(owner, repoPane).then((p) => p?.project?.name)).toBe("myrepo");
  const first = await sum(owner, repoPane);
  expect(first.kind).toBe("shell");
  expect(first.epoch).toBeUndefined();
  expect(first.policy).toBeUndefined();

  // A busy command: its kind and activity arrive without a new State.
  const before = await owner.evaluate(() => (window as any).sums.state.rev);
  await api(`/api/panes/${repoPane}/send`, {
    text: `perl -e '$0 = "cargo test"; for (1..60) { print "running test $_\\n"; select(undef, undef, undef, 0.05) }'`,
    enter: true,
  });
  await expect.poll(() => sum(owner, repoPane).then((p) => p?.kind)).toBe("test");
  await expect.poll(() => sum(owner, repoPane).then((p) => p?.activity?.bps ?? 0)).toBeGreaterThan(100);
  expect(await sum(owner, repoPane).then((p) => p.attention)).toBe("working");
  expect(await owner.evaluate(() => (window as any).sums.state.rev)).toBe(before);
  // It made no terminals and attached to nothing.
  expect(await owner.evaluate(() => (window as any).sums.panes.size)).toBe(0);
  // The tab view beside it still draws the same pane live.
  await owner.evaluate((p) => window.__illogical.client.setActive(p), repoPane);
  await expect.poll(() => owner.evaluate((p) => window.__illogical.text(p), repoPane)).toContain("running test 3");
});

test("someone else gets summaries only for what's shared with them", async ({ browser }) => {
  await owner.evaluate((p) => window.__illogical.client.paneOp(p, { op: "set_private", on: true }), privatePane);
  await api(`/api/panes/${privatePane}/send`, { text: "cd / && echo secret-place", enter: true });
  expect((await api("/api/acl", { session: shared, principal: `tailnet:${FRIEND}`, role: "viewer" })).ok).toBe(true);
  const friend = await (await browser.newContext({ extraHTTPHeaders: { "tailscale-user-login": FRIEND } })).newPage();
  await openSummaries(friend);
  const ids = () => friend.evaluate(() => (window as any).sums.state.panes.map((p: { id: number }) => p.id).sort());
  const sharedPanes = await owner.evaluate((s) => {
    const c = window.__illogical.client;
    const tabs = c.state!.sessions.find((x) => x.id === s)!.tabs;
    return c.state!.tabs.filter((t) => tabs.includes(t.id)).flatMap((t) => t.layout.panes.map(([id]) => id)).sort();
  }, shared);
  expect(sharedPanes).toContain(repoPane);
  expect(sharedPanes).toContain(privatePane);
  expect(sharedPanes).not.toContain(otherPane);
  await expect.poll(ids).toEqual(sharedPanes);
  const priv = await sum(friend, privatePane);
  expect(priv.private).toBe(true);
  expect(priv.cwd ?? null).toBeNull();
  expect(priv.kind ?? null).toBeNull();
  expect(priv.project ?? null).toBeNull();

  // Work in a session that isn't shared never reaches them.
  await api(`/api/panes/${otherPane}/send`, { text: "echo not-for-friends", enter: true });
  await expect.poll(() => sum(owner, otherPane).then((p) => p?.last?.text)).toBe("echo not-for-friends");
  expect(await sum(friend, otherPane)).toBeNull();
  // But the shared pane's changes do.
  await api(`/api/panes/${repoPane}/send`, { text: "echo for-friends", enter: true });
  await expect.poll(() => sum(friend, repoPane).then((p) => p?.last?.text)).toBe("echo for-friends");

  // Revoking it takes the panes away.
  expect((await api("/api/acl", { session: shared, principal: `tailnet:${FRIEND}`, role: null })).ok).toBe(true);
  await expect.poll(() => friend.evaluate(() => (window as any).sums.connected)).toBe(false);
});
