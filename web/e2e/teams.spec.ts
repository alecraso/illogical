// M19: teams. Two people at different companies, neither on a tailnet, join
// a team by invite; a team-owned box joins the team; both use it through
// control's relay, see each other there and pass control back and forth;
// one runs a build on it. A read-only link works in a logged-out browser
// and dies at expiry. Removing a member cuts them off within a second.

import { spawn, type ChildProcess } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { createServer, type Server } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test, type Browser, type Page } from "@playwright/test";
import { ready, run, text } from "./helpers";

const CONTROL = 7780;
const GITHUB = 7781;
const BOX = 7782;
const base = `http://127.0.0.1:${CONTROL}`;
const procs: ChildProcess[] = [];
const dirs: string[] = [];
let gh: Server;

test.describe.configure({ mode: "serial" });
test.use({ baseURL: base });

function temp(what: string) {
  const d = mkdtempSync(join(tmpdir(), `illogical-e2e-teams-${what}-`));
  dirs.push(d);
  return d;
}

test.beforeAll(async () => {
  // A fake GitHub with as many users as there are `as` cookies.
  gh = createServer((req, res) => {
    const u = new URL(req.url!, `http://127.0.0.1:${GITHUB}`);
    if (u.pathname === "/login/oauth/authorize") {
      const who = /(?:^|;\s*)as=(\w+)/.exec(req.headers.cookie ?? "")?.[1] ?? "nobody";
      const back = new URL(u.searchParams.get("redirect_uri")!);
      back.searchParams.set("code", who);
      back.searchParams.set("state", u.searchParams.get("state")!);
      res.writeHead(302, { location: back.href }).end();
    } else if (u.pathname === "/login/oauth/access_token") {
      let body = "";
      req.on("data", (d) => (body += d));
      req.on("end", () => {
        const code = new URLSearchParams(body).get("code");
        res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify({ access_token: `tok-${code}` }));
      });
    } else if (u.pathname === "/user") {
      const login = (req.headers.authorization ?? "").replace("Bearer tok-", "");
      const id = [...login].reduce((h, c) => h * 31 + c.charCodeAt(0), 7);
      res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify({ id, login }));
    } else res.writeHead(404).end();
  }).listen(GITHUB, "127.0.0.1");
  procs.push(
    spawn(
      "../target/debug/illogical-control",
      [
        ...["--listen", `127.0.0.1:${CONTROL}`, "--public-url", base, "--db", join(temp("db"), "control.db")],
        ...["--github-client-id", "id", "--github-client-secret", "s", "--static-dir", "dist"],
        ...["--github-url", `http://127.0.0.1:${GITHUB}`, "--github-api", `http://127.0.0.1:${GITHUB}`],
      ],
      { stdio: "ignore" },
    ),
  );
  for (let i = 0; i < 100; i++) {
    try {
      if ((await fetch(`${base}/control.json`)).ok) break;
    } catch {
      // not yet
    }
    await new Promise((r) => setTimeout(r, 100));
  }
});

test.afterAll(() => {
  for (const p of procs) p.kill("SIGKILL");
  gh?.close();
  for (const d of dirs) rmSync(d, { recursive: true, force: true });
});

async function person(browser: Browser, login: string): Promise<Page> {
  const ctx = await browser.newContext();
  await ctx.addCookies([{ name: "as", value: login, url: `http://127.0.0.1:${GITHUB}` }]);
  const page = await ctx.newPage();
  await page.goto("/");
  await page.locator("[data-signin=github]").click();
  await page.locator("[data-saved-codes]").click();
  await page.waitForFunction(() => window.__illogical?.control?.phase === "ready");
  return page;
}

const hostNames = (page: Page) => page.evaluate(() => window.__illogical.hosts.names);

let alice: Page;
let bob: Page;
let team = "";
let pane = 0;

test("two people at different companies join a team by invite", async ({ browser }) => {
  alice = await person(browser, "alice");
  await alice.evaluate(() => window.__illogical.control!.createTeam("Acme"));
  team = await alice.evaluate(() => window.__illogical.control!.teams[0].team);
  const link = await alice.evaluate((t) => window.__illogical.control!.invite(t, "editor"), team);
  bob = await person(browser, "bob");
  await bob.goto(link);
  await bob.locator("[data-accept-invite]").click();
  await expect(bob.locator("[data-invite-pending]")).toBeVisible();
  // Alice is asked, sees Bob's fingerprint, and adds him (signing the roster).
  await alice.evaluate(() => window.__illogical.control!.refresh());
  await expect(alice.locator("[data-admit-yes]")).toBeVisible({ timeout: 15_000 });
  await alice.locator("[data-admit-yes]").click();
  await expect
    .poll(() => alice.evaluate(() => window.__illogical.control!.teams[0].roster.members.map((m) => `${m.name}:${m.role}`)), { timeout: 15_000 })
    .toEqual(["alice:owner", "bob:editor"]);
});

test("a team-owned box joins; both use it through the relay and pass control", async () => {
  const state = temp("box");
  const joining = spawn("../target/debug/illogicald", ["join", base, "--name", "buildbox", "--team", team, "--state-dir", state], {
    stdio: ["ignore", "pipe", "ignore"],
  });
  procs.push(joining);
  const link = await new Promise<string>((res) => {
    let out = "";
    joining.stdout!.on("data", (d) => {
      out += d;
      const m = out.match(/(http\S+#join=[A-Z0-9-]+)/);
      if (m) res(m[1]);
    });
  });
  const exited = new Promise<number | null>((r) => joining.on("exit", r));
  await alice.goto(link);
  await alice.locator("[data-approve-join]").click();
  expect(await exited).toBe(0);
  procs.push(
    spawn(
      "../target/debug/illogicald",
      [
        ...["--listen", `127.0.0.1:${BOX}`, "--name", "buildbox", "--state-dir", state],
        ...["--shell", "bash --norc --noprofile", "--no-manager-env", "--tailscale-socket", "/nonexistent/sock"],
      ],
      { stdio: "ignore" },
    ),
  );
  for (const p of [alice, bob]) {
    await p.goto("/");
    await p.waitForFunction(() => window.__illogical?.control?.phase === "ready");
    await expect.poll(() => hostNames(p), { timeout: 30_000 }).toEqual(["buildbox"]);
    await expect.poll(() => p.evaluate(() => window.__illogical.client.connected), { timeout: 30_000 }).toBe(true);
    expect(await p.evaluate(() => window.__illogical.client.path)).toBe("relayed");
  }
  pane = await alice.evaluate(() => window.__illogical.client.state!.panes[0].id);
  // Each sees the other.
  for (const p of [alice, bob]) await p.locator(`[data-pane="${pane}"]`).click({ position: { x: 40, y: 40 } });
  await expect(alice.locator(".people .avatar")).toHaveCount(1);
  await expect(bob.locator(".people .avatar")).toHaveCount(1);
  // Bob (an editor of the team) runs a build on the team's box.
  await ready(bob, pane);
  await run(bob, pane, "echo build-$((6*7))-ok", "build-42-ok");
  await expect.poll(() => text(alice, pane)).toContain("build-42-ok");
  // Alice takes control; Bob is held back; she hands it back on request.
  await alice.evaluate((p) => window.__illogical.client.paneOp(p, { op: "take_control" }), pane);
  await run(alice, pane, "echo alice-$((6*7))", "alice-42");
  await bob.keyboard.type("echo BOB-INTERRUPTS\n");
  await new Promise((r) => setTimeout(r, 400));
  expect(await text(alice, pane)).not.toContain("BOB-INTERRUPTS");
  await bob.evaluate((p) => window.__illogical.client.paneOp(p, { op: "request_control" }), pane);
  await alice.locator("[data-give]").click();
  await run(bob, pane, "echo bob-$((6*7))", "bob-42");
});

test("a read-only link works logged out, and dies at expiry", async ({ browser }) => {
  const session = await alice.evaluate(() => window.__illogical.client.state!.sessions[0].id);
  const url = await alice.evaluate(
    ([s]) => {
      const c = window.__illogical.client;
      return window.__illogical.control!.makeLink((m, p, b) => c.request(m, p, b), c.e2e!.daemon.id, s, 12, false);
    },
    [session] as const,
  );
  await new Promise((r) => setTimeout(r, 1500)); // control learns the daemon has a link
  const stranger = await (await browser.newContext()).newPage();
  await stranger.goto(url);
  await expect.poll(() => stranger.evaluate(() => window.__illogical?.client.connected), { timeout: 20_000 }).toBe(true);
  expect(await stranger.evaluate(() => window.__illogical.control)).toBeNull();
  await ready(stranger, pane);
  await alice.evaluate((p) => window.__illogical.client.paneOp(p, { op: "take_control" }), pane);
  await run(alice, pane, "echo live-$((6*7))", "live-42");
  await expect.poll(() => text(stranger, pane)).toContain("live-42");
  // Read-only.
  await stranger.locator(`[data-pane="${pane}"]`).click({ position: { x: 40, y: 40 } });
  await stranger.keyboard.type("echo STRANGER\n");
  await new Promise((r) => setTimeout(r, 400));
  expect(await text(alice, pane)).not.toContain("STRANGER");
  // It ends on time, and can't come back.
  await expect.poll(() => stranger.evaluate(() => window.__illogical.client.connected), { timeout: 20_000 }).toBe(false);
  await new Promise((r) => setTimeout(r, 3000));
  expect(await stranger.evaluate(() => window.__illogical.client.connected)).toBe(false);
});

test("removing a member cuts them off within a second", async () => {
  await expect.poll(() => bob.evaluate(() => window.__illogical.client.connected)).toBe(true);
  const t = Date.now();
  await alice.evaluate((tm) => window.__illogical.control!.changeTeam(tm, (ms) => ms.filter((m) => m.name !== "bob")), team);
  await expect.poll(() => bob.evaluate(() => window.__illogical.client.connected), { timeout: 3000, intervals: [50] }).toBe(false);
  expect(Date.now() - t).toBeLessThan(1500);
});
