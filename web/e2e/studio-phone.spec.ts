// #86's phone check, with no person (#214 section 6): a studio app made
// from the template through studio's token API, `illogical studio login`
// with a test token, `hud share --role follower` in the box and
// `illogical studio follower APP` with its link, the app opened with
// `illogical app`, then its agent's question answered from a teammate's
// Pixel 7 and a release gate approved from the owner's iPhone (WebKit),
// with hud told who did each.
//
// Studio and the box are fakes, as in apps.spec.ts: studio.arugula.io isn't
// reachable from tests, and its token API (studio#292) lists apps and mints
// entry links. Making an app from the template through a token is the
// fixture's own route (`POST /api/apps`), standing in for studio's web UI.
// The box is hud's routes as S22 used them, plus hud's trusted follower
// (hud#736): a follower link's session may name who answered with
// `onBehalfOf`, and `hud share --role follower` (a stand-in script) mints one.

import { spawn, type ChildProcess } from "node:child_process";
import { chmodSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { createServer, type IncomingMessage, type Server, type ServerResponse } from "node:http";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { expect, test, type Page } from "@playwright/test";
import { tokenCookies } from "./local-token";
import { daemon, iphone, launchWebkit, pixel7 } from "./phones";
import { listen } from "./ports";

const TOKEN = "e2e-studio-token";
const OWNER = "me@example.com";
const SAM = "sam@example.com";

let dir = "";
let url = "";
let proc: ChildProcess | undefined;
let studio = "";
let box = "";
const servers: Server[] = [];

// The fake studio's state.
const apps: { name: string; title: string; url: string; createdAt: number; template: string }[] = [];
const entryKeys = new Set<string>();
// The fake box's.
const followerKeys = new Set<string>();
const sessions = new Map<string, "owner" | "follower">();
let question: Record<string, unknown> | null = null;
const answers: { body: Record<string, unknown>; as: string }[] = [];
let gates: Record<string, unknown>[] = [];
const approvals: { body: Record<string, unknown>; as: string }[] = [];
const streams = new Set<ServerResponse>();
const feeds = new Set<ServerResponse>();

const json = (res: ServerResponse, status: number, v: unknown) => res.writeHead(status, { "content-type": "application/json" }).end(JSON.stringify(v));
const sessionOf = (req: IncomingMessage) => sessions.get(/(?:^|;\s*)hud_session=(\w+)/.exec(req.headers.cookie ?? "")?.[1] ?? "");
const frame = () => `data: ${JSON.stringify({ type: "hud-chat-queue", chatKey: "c1", waiting: [], ...(question ? { question } : {}) })}\n\n`;
const changed = () => streams.forEach((s) => s.write(frame()));
const moved = () => feeds.forEach((s) => s.write("event: live-state\ndata: {}\n\n"));
function body(req: IncomingMessage): Promise<Record<string, unknown>> {
  return new Promise((ok) => {
    let b = "";
    req.on("data", (d) => (b += d));
    req.on("end", () => ok(b ? JSON.parse(b) : {}));
  });
}
function session(res: ServerResponse, kind: "owner" | "follower") {
  const s = `${kind}${sessions.size + 1}x${Date.now()}`;
  sessions.set(s, kind);
  res.writeHead(302, { location: "/", "set-cookie": `hud_session=${s}; Path=/; HttpOnly; SameSite=None; Secure; Partitioned`, "cache-control": "no-store" }).end();
}

test.describe.configure({ mode: "serial" });

test.beforeAll(async () => {
  dir = mkdtempSync(join(tmpdir(), "ilg-e2e-studio-phone-"));
  const boxServer = createServer(async (req, res) => {
    const u = new URL(req.url!, "http://box");
    const as = sessionOf(req);
    if (u.pathname === "/__enter") return entryKeys.has(u.searchParams.get("k") ?? "") ? session(res, "owner") : void res.writeHead(403).end();
    if (u.pathname === "/__hud/join") return followerKeys.has(u.searchParams.get("t") ?? "") ? session(res, "follower") : void res.writeHead(401).end();
    // What `hud share --role follower` asks hud for, inside the box.
    if (u.pathname === "/__test/share" && req.method === "POST") {
      if (u.searchParams.get("role") !== "follower") return json(res, 400, { error: "role" });
      const t = `f${followerKeys.size + 1}x${Date.now()}`;
      followerKeys.add(t);
      return json(res, 200, { url: `${box}/__hud/join?t=${t}` });
    }
    if (u.pathname.startsWith("/__hud/api/")) {
      if (!as) return json(res, 401, { error: "no session" });
      if (u.pathname === "/__hud/api/tabs") return json(res, 200, { tabs: [{ chatKey: "c1", title: "main" }] });
      if (u.pathname === "/__hud/api/chat/stream") {
        res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache" });
        res.write(frame());
        streams.add(res);
        req.on("close", () => streams.delete(res));
        return;
      }
      if (u.pathname === "/__hud/api/work") return json(res, 200, { v: 1, groups: [{ id: "approve", items: gates.map((gate) => ({ key: `gate:${gate.name}`, gate })) }] });
      if (u.pathname === "/__hud/api/live/stream") {
        res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache" });
        res.write("event: live-snapshot\ndata: {}\n\n");
        feeds.add(res);
        req.on("close", () => feeds.delete(res));
        return;
      }
      if (req.method === "POST" && req.headers.origin !== box) return json(res, 403, { error: "cross-site" });
      // hud#736: only a follower's session may name someone else.
      const b = req.method === "POST" ? await body(req) : {};
      if (b.onBehalfOf && as !== "follower") return json(res, 403, { error: "onBehalfOf needs a follower session" });
      if (u.pathname === "/__hud/api/chat/answer" && req.method === "POST") {
        answers.push({ body: b, as });
        question = null;
        changed();
        return json(res, 200, { answered: true });
      }
      if (u.pathname === "/__hud/api/work/gates/approve" && req.method === "POST") {
        approvals.push({ body: b, as });
        gates = [];
        moved();
        return json(res, 200, { v: 1, approval: { command: "chant approve", exitCode: 0 } });
      }
      return json(res, 404, {});
    }
    if (!as) return void res.writeHead(401, { "content-type": "text/html" }).end("<p id=door>You need a link to get in</p>");
    res.writeHead(200, { "content-type": "text/html" }).end("<title>Pinboard</title><p id=app>Hello from the box</p>");
  });
  box = `http://localhost:${await listen(boxServer)}`;
  servers.push(boxServer);

  const studioServer = createServer(async (req, res) => {
    if (req.headers.authorization !== `Bearer ${TOKEN}`) return json(res, 401, { error: "who are you?" });
    const u = new URL(req.url!, "http://studio");
    if (u.pathname === "/api/apps" && req.method === "GET") return json(res, 200, { apps: apps.map(({ name, title, url, createdAt }) => ({ name, title, url, createdAt })) });
    // The fixture's: a new app from the box template.
    if (u.pathname === "/api/apps" && req.method === "POST") {
      const b = await body(req);
      if (b.template !== "arugula-box-template") return json(res, 400, { error: "no such template" });
      const app = { name: String(b.name), title: String(b.title ?? b.name), url: box, createdAt: Date.now(), template: String(b.template) };
      apps.push(app);
      return json(res, 201, app);
    }
    const open = /^\/api\/apps\/([\w-]+)\/open$/.exec(u.pathname);
    if (open && req.method === "POST" && apps.some((a) => a.name === open[1])) {
      const k = `k${entryKeys.size + 1}x${Date.now()}`;
      entryKeys.add(k);
      return json(res, 200, { url: `${box}/__enter?e=${Date.now() + 600_000}&k=${k}` });
    }
    json(res, 404, {});
  });
  studio = `http://127.0.0.1:${await listen(studioServer)}`;
  servers.push(studioServer);

  // hud in the box, as far as `hud share` goes.
  writeFileSync(
    join(dir, "hud"),
    `#!/bin/sh\n[ "$1 $2 $3" = "share --role follower" ] || { echo "usage: hud share --role follower" >&2; exit 2; }\n` +
      `curl -fsS -X POST "${box}/__test/share?role=follower" | sed -e 's/.*"url":"\\([^"]*\\)".*/\\1/'\n`,
  );
  chmodSync(join(dir, "hud"), 0o755);

  ({ proc, url } = await daemon(join(dir, "state"), ["--owner", OWNER, "--tailscale-socket", "/nonexistent/sock", "--wisp-token-file", "/nonexistent"]));
});

test.afterAll(() => {
  proc?.kill("SIGKILL");
  for (const s of [...streams, ...feeds]) s.end();
  for (const s of servers) {
    s.closeAllConnections();
    s.close();
  }
  if (dir) rmSync(dir, { recursive: true, force: true });
});

const CLI = resolve("../target/debug/illogical");
/** Run a program with `input` on stdin; its output. Never synchronously:
 * the fakes it talks to live in this process. */
function run(bin: string, args: string[], input = ""): Promise<string> {
  return new Promise((ok, fail) => {
    const p = spawn(bin, args, { stdio: ["pipe", "pipe", "pipe"] });
    let out = "";
    let err = "";
    p.stdout.on("data", (d) => (out += d));
    p.stderr.on("data", (d) => (err += d));
    p.on("exit", (code) => (code === 0 ? ok(out) : fail(new Error(`${bin} ${args.join(" ")}: ${code}\n${out}${err}`))));
    p.stdin.end(input);
  });
}
/** The CLI against the test daemon. */
const cli = (args: string[], input = "") => run(CLI, ["--socket", join(dir, "state/sock"), ...args], input);
const api = (path: string, method = "GET", b?: unknown, headers: Record<string, string> = {}) =>
  fetch(url + path, { method, headers: { "content-type": "application/json", ...headers }, body: b === undefined ? undefined : JSON.stringify(b) });
const info = async (pane: number) => ((await (await api("/api/panes")).json()) as { id: number; reason?: { kind: string } | null; answered?: { who: string; how: string } }[]).find((p) => p.id === pane);

let block = 0;

test("an app from the template, studio login with a token, a follower link, the app opened", async () => {
  // In studio: a new app from the template.
  const made = await fetch(`${studio}/api/apps`, {
    method: "POST",
    headers: { authorization: `Bearer ${TOKEN}`, "content-type": "application/json" },
    body: JSON.stringify({ template: "arugula-box-template", name: "pinboard", title: "Pinboard" }),
  });
  expect(made.status).toBe(201);

  // `illogical studio login`, the token on stdin as it would be pasted.
  expect(await cli(["studio", "login", studio], `${TOKEN}\n`)).toBe("logged in; 1 app\n");
  expect(JSON.stringify(await (await api("/api/studio")).json())).not.toContain(TOKEN);

  // In the box: `hud share --role follower`; its link kept for the app.
  const link = (await run(join(dir, "hud"), ["share", "--role", "follower"])).trim();
  expect(link).toMatch(/\/__hud\/join\?t=f\d+x\d+$/);
  await cli(["studio", "follower", "pinboard"], `${link}\n`);
  expect(await cli(["studio"])).toContain("follower link for pinboard");
  expect(await cli(["studio"])).not.toContain(link);

  // `illogical app pinboard`: the block follows with the follower link.
  block = Number((await cli(["app", "pinboard"])).trim().replace(/^%/, ""));
  expect(block).toBeGreaterThan(0);
  await expect.poll(async () => ((await (await api(`/api/blocks/${block}`)).json()) as { state: { follower: { state: string } } }).state?.follower?.state, { timeout: 15_000 }).toBe("following");
  const state = ((await (await api(`/api/blocks/${block}`)).json()) as { state: Record<string, unknown> }).state;
  expect(state.follower_credential).toBe(true);
  expect([...sessions.values()]).toContain("follower");
});

test("the box's agent asks; Sam answers from a Pixel 7, and hud hears it was Sam", async ({ browser }) => {
  // Sam may edit the session the app is in.
  const session = ((await (await api("/api/panes")).json()) as { id: number; session: number }[]).find((p) => p.id === block)!.session;
  expect((await api("/api/acl", "POST", { session, principal: `tailnet:${SAM}`, role: "editor" })).ok).toBe(true);
  const ctx = await browser.newContext({ ...pixel7, baseURL: url, extraHTTPHeaders: { "tailscale-user-login": SAM }, storageState: { cookies: tokenCookies, origins: [] } });
  const sam = await ctx.newPage();
  await sam.goto("/");
  await expect.poll(() => sam.evaluate(() => window.__illogical?.client.role())).toBe("editor");

  const now = Date.now();
  question = {
    requestId: "r1",
    summary: "Which colour should the header be?",
    askedAt: now,
    expiresAt: now + 300_000,
    options: [
      { optionId: "o0", name: "Green", description: "Matches arugula" },
      { optionId: "o1", name: "Blue", description: "Calmer" },
    ],
  };
  changed();
  // On Sam's phone: Needs you, then the card on the app's block.
  await expect.poll(() => sam.evaluate((b) => window.__illogical.client.info(b)?.reason?.kind ?? null, block), { timeout: 15_000 }).toBe("ask");
  await sam.locator(".sheet-button").tap();
  const row = sam.locator(`[data-wants="${block}"]`);
  await expect(row).toContainText("Which colour should the header be?");
  await row.locator(".sheet-item").tap();
  const card = sam.locator(`[data-pane="${block}"]`).getByRole("dialog", { name: "Which colour should the header be?" });
  await expect(card).toBeVisible();
  await card.getByRole("radio", { name: /^Blue/ }).tap();
  await card.getByRole("button", { name: "Submit" }).tap();

  await expect.poll(() => answers.length, { timeout: 10_000 }).toBe(1);
  expect(answers[0]).toEqual({ as: "follower", body: { chatKey: "c1", requestId: "r1", optionId: "o1", onBehalfOf: { name: "sam", via: "illogical" } } });
  await expect.poll(async () => (await info(block))?.answered).toMatchObject({ how: "answered", who: `tailnet:${SAM}` });
  await expect(card).toHaveCount(0);
  await ctx.close();
});

test("a release waits at ship; the owner approves from an iPhone (WebKit), and hud hears who", async () => {
  test.setTimeout(60_000);
  const webkit = await launchWebkit();
  try {
    const ctx = await webkit.newContext({ ...iphone, baseURL: url, storageState: { cookies: tokenCookies, origins: [] } });
    const me = await ctx.newPage();
    await me.goto("/");
    await expect.poll(() => me.evaluate(() => window.__illogical?.client.connected)).toBe(true);

    gates = [{ member: "delivery", component: "release", name: "ship", env: "prod", needed: 1, approvals: 0, approve: "chant approve release ship --env prod" }];
    moved();
    await expect.poll(() => me.evaluate((b) => window.__illogical.client.info(b)?.reason?.kind ?? null, block), { timeout: 20_000 }).toBe("gate");
    await me.locator(".sheet-button").tap();
    const row = me.locator(`[data-wants="${block}"]`);
    await expect(row).toContainText("release waits at gate ship");
    await row.locator("[data-approve-gate]").tap();

    await expect.poll(() => approvals.length, { timeout: 10_000 }).toBe(1);
    expect(approvals[0]).toEqual({
      as: "follower",
      body: { member: "delivery", component: "release", gate: "ship", env: "prod", onBehalfOf: { name: "me", via: "illogical" } },
    });
    await expect.poll(async () => (await info(block))?.reason ?? null).toBeNull();
    expect((await info(block))?.answered).toMatchObject({ how: "approved", who: "owner" });
    // The owner's history says so.
    const hist = (await (await api(`/api/history?pane=${block}`)).json()) as { text: string }[];
    expect(hist.map((h) => h.text)).toContain("approved delivery: release at gate ship");
    await ctx.close();
  } finally {
    await webkit.close();
  }
});
