// M12: principals and roles. A second tailnet user (a header on loopback,
// as `tailscale serve` adds it) gets in only to what's shared with them:
// as a viewer they see that session live and nothing else, and typing, API
// calls and layout changes are refused; made an editor they can type at
// once, without reconnecting; revoked, they're cut off within a second.
// The audit log has each change.

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
let daemon: ChildProcess;
let dir: string;

test.describe.configure({ mode: "serial" });
test.use({ baseURL: async ({}, use) => use(base) });

test.beforeAll(async () => {
  dir = mkdtempSync(join(tmpdir(), "arugula-e2e-access-"));
  daemon = spawn(
    "../target/debug/arugulad",
    [
      ...["--listen", ANY, "--state-dir", labs(dir), "--owner", OWNER],
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

const asFriend = { "tailscale-user-login": FRIEND };
const api = (path: string, body?: unknown, headers: Record<string, string> = {}) =>
  fetch(base + path, {
    method: body === undefined ? "GET" : "POST",
    headers: { ...headers, ...(body === undefined ? {} : { "content-type": "application/json" }) },
    body: body === undefined ? undefined : JSON.stringify(body),
  });

let owner: Page;
let friend: Page;
let shared = 0;
let sharedPane = 0;
let privatePane = 0;

async function openAs(page: Page) {
  await page.goto("/");
  await expect.poll(() => page.evaluate(() => window.__arugula?.client.connected)).toBe(true);
}

test("before anything is shared, a second user is turned away", async ({ browser }) => {
  owner = await (await browser.newContext()).newPage();
  await openAs(owner);
  // Two sessions: one to share, one private.
  await owner.evaluate(() => window.__arugula.client.intent({ op: "new_session", name: "private", from_pane: null }));
  await expect.poll(() => owner.evaluate(() => window.__arugula.client.state!.sessions.length)).toBe(2);
  [shared, sharedPane, privatePane] = await owner.evaluate(() => {
    const c = window.__arugula.client;
    const [a, b] = c.state!.sessions;
    const first = (s: typeof a) => c.state!.tabs.find((t) => t.id === s.tabs[0])!.root;
    const pane = (n: typeof a extends unknown ? ReturnType<typeof first> : never): number => (n.type === "pane" ? n.pane : pane(n.children[0].node));
    return [a.id, pane(first(a)), pane(first(b))];
  });
  expect((await api("/", undefined, asFriend)).status).toBe(403);
});

test("a viewer sees the shared session live, and nothing else", async ({ browser }) => {
  expect((await api("/api/acl", { session: shared, principal: `tailnet:${FRIEND}`, role: "viewer" })).ok).toBe(true);
  friend = await (await browser.newContext({ extraHTTPHeaders: asFriend })).newPage();
  await openAs(friend);
  const seen = await friend.evaluate(() => {
    const s = window.__arugula.client.state!;
    return { sessions: s.sessions.map((x) => x.id), panes: s.panes.map((p) => p.id), role: window.__arugula.client.role() };
  });
  expect(seen.sessions).toEqual([shared]);
  expect(seen.panes).toEqual([sharedPane]);
  expect(seen.role).toBe("viewer");
  // Live: what the owner runs shows up for the friend.
  await owner.evaluate((s) => window.__arugula.client.selectSession(s), shared);
  await ready(owner, sharedPane);
  await run(owner, sharedPane, "echo owner-$((6*7))", "owner-42");
  await ready(friend, sharedPane);
  await expect.poll(() => text(friend, sharedPane)).toContain("owner-42");
});

test("a viewer can't type, call the API or change the layout", async () => {
  await friend.locator(`[data-pane="${sharedPane}"]`).click({ position: { x: 40, y: 40 } });
  await friend.keyboard.type("echo viewer-typed\n");
  await expect(friend.getByText("you're watching this session")).toBeVisible();
  expect(await text(owner, sharedPane)).not.toContain("viewer-typed");
  const send = await api(`/api/panes/${sharedPane}/send`, { text: "echo nope\n" }, asFriend);
  expect(send.status).toBe(403);
  expect((await api(`/api/panes/${privatePane}/capture`, undefined, asFriend)).status).toBe(404);
  expect((await api("/api/run", { command: "id" }, asFriend)).status).toBe(403);
  expect((await api("/api/fs/list?path=/", undefined, asFriend)).status).toBe(403);
  expect((await api(`/api/panes/${sharedPane}/capture`, undefined, asFriend)).status).toBe(200);
  const before = await owner.evaluate(() => window.__arugula.client.state!.panes.length);
  await friend.evaluate((p) => window.__arugula.client.intent({ op: "split", pane: p, edge: "right" }), sharedPane);
  await new Promise((r) => setTimeout(r, 500));
  expect(await owner.evaluate(() => window.__arugula.client.state!.panes.length)).toBe(before);
});

test("made an editor, they type at once, without reconnecting", async () => {
  const clientId = await friend.evaluate(() => window.__arugula.client.clientId);
  expect((await api("/api/acl", { session: shared, principal: `tailnet:${FRIEND}`, role: "editor" })).ok).toBe(true);
  await expect.poll(() => friend.evaluate(() => window.__arugula.client.role())).toBe("editor");
  // It runs on the owner's machine, so they trust the friend with it (M14),
  // and the owner typed there last, so drives it (M13): take control.
  await owner.evaluate(
    ([p, to]) => window.__arugula.client.paneOp(p, { op: "grant_trust", to, minutes: 30 }),
    [sharedPane, `tailnet:${FRIEND}`] as const,
  );
  await expect.poll(() => friend.evaluate((p) => window.__arugula.client.mayType(p), sharedPane)).toBe(true);
  await friend.evaluate((p) => window.__arugula.client.paneOp(p, { op: "take_control" }), sharedPane);
  await run(friend, sharedPane, "echo friend-$((6*7))", "friend-42");
  await expect.poll(() => text(owner, sharedPane)).toContain("friend-42");
  expect(await friend.evaluate(() => window.__arugula.client.clientId)).toBe(clientId);
});

test("revoked, they're cut off within a second; the audit log has it all", async () => {
  const t = Date.now();
  expect((await api("/api/acl", { session: shared, principal: `tailnet:${FRIEND}`, role: null })).ok).toBe(true);
  await expect.poll(() => friend.evaluate(() => window.__arugula.client.connected), { timeout: 1000, intervals: [50] }).toBe(false);
  expect(Date.now() - t).toBeLessThan(1500);
  expect((await api("/", undefined, asFriend)).status).toBe(403);
  const log = (await (await api("/api/acl")).json()) as { audit: { action: string; role: string | null }[] };
  expect(log.audit.map((e) => `${e.action}:${e.role ?? ""}`)).toEqual(["grant:viewer", "grant:editor", "revoke:"]);
});
