// #344: control's relay at its ceiling. A machine that's only reachable
// through the relay holds control's one relay socket; a page that wants
// it is told control is full, says so, and waits half a minute or so
// rather than retrying every few seconds. Signing in still works.

import { spawn, type ChildProcess } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { createServer, type Server } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test, type Page } from "@playwright/test";
import { ANY, controlPort, listen } from "./ports";
import { closeContexts } from "./helpers";
import { labs } from "./labs";

test.afterAll(closeContexts);

let base = "";
const procs: ChildProcess[] = [];
const dirs: string[] = [];
let gh: Server;

test.use({ baseURL: async ({}, use) => use(base) });

function temp(what: string) {
  const d = mkdtempSync(join(tmpdir(), `illogical-e2e-relayfull-${what}-`));
  dirs.push(d);
  return d;
}

async function up(url: string) {
  for (let i = 0; i < 100; i++) {
    try {
      if ((await fetch(url)).status < 500) return;
    } catch {
      // not yet
    }
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error(`${url} didn't come up`);
}

test.beforeAll(async () => {
  gh = createServer((req, res) => {
    const u = new URL(req.url!, "http://github");
    if (u.pathname === "/login/oauth/authorize") {
      const back = new URL(u.searchParams.get("redirect_uri")!);
      back.searchParams.set("code", "c0de");
      back.searchParams.set("state", u.searchParams.get("state")!);
      res.writeHead(302, { location: back.href }).end();
    } else if (u.pathname === "/login/oauth/access_token") {
      res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify({ access_token: "gho_test" }));
    } else if (u.pathname === "/user") {
      res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify({ id: 9, login: "full" }));
    } else res.writeHead(404).end();
  });
  const github = `http://127.0.0.1:${await listen(gh)}`;
  const db = join(temp("db"), "control.db");
  procs.push(
    spawn(
      "../target/debug/illogical-control",
      [
        ...["--listen", ANY, "--public-url", "http://127.0.0.1:0", "--db", db],
        ...["--github-client-id", "id", "--github-client-secret", "s", "--static-dir", "dist"],
        ...["--github-url", github, "--github-api", github],
        ...["--relay-max-total", "1"],
      ],
      { stdio: "ignore" },
    ),
  );
  base = `http://127.0.0.1:${await controlPort(db, procs.at(-1))}`;
  await up(`${base}/control.json`);
});

test.afterAll(() => {
  for (const p of procs) p.kill("SIGKILL");
  gh?.close();
  for (const d of dirs) rmSync(d, { recursive: true, force: true });
});

async function signIn(page: Page) {
  await page.goto("/");
  await page.locator("[data-signin=github]").click();
  await expect(page.locator(".control-center, .control-page, .app")).toBeVisible();
}

test("a full relay: the page says so and waits; signing in still works", async ({ browser }) => {
  test.setTimeout(90_000);
  const ctx = await browser.newContext();
  const approver = await ctx.newPage();
  await signIn(approver);
  await approver.locator("[data-stored-codes]").check();
  await approver.locator("[data-saved-codes]").click();

  // A machine joins, reachable only through the relay.
  const state = temp("mac");
  const joining = spawn("../target/debug/illogicald", ["join", base, "--name", "mac", "--state-dir", state], {
    stdio: ["pipe", "pipe", "ignore"],
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
  await approver.goto(link);
  const account = await approver.locator("[data-join-account]").getAttribute("data-join-account");
  await approver.locator("[data-approve-join]").click();
  joining.stdin!.end(`${account}\n`);
  expect(await exited).toBe(0);
  // Nothing of this page's holds the relay's one socket.
  await approver.close();
  procs.push(
    spawn(
      "../target/debug/illogicald",
      [
        ...["--listen", ANY, "--name", "mac", "--state-dir", labs(state)],
        ...["--shell", "bash --norc --noprofile", "--no-manager-env", "--tailscale-socket", "/nonexistent/sock"],
      ],
      { stdio: "ignore" },
    ),
  );
  // The machine dials in, and takes it.
  await expect
    .poll(async () => ((await (await ctx.request.get("/api/directory")).json()) as { daemons: { online: boolean }[] }).daemons.map((d) => d.online), {
      timeout: 20_000,
    })
    .toEqual([true]);

  const page = await ctx.newPage();
  const relay: string[] = [];
  page.on("websocket", (ws) => {
    if (ws.url().includes("/api/relay/")) relay.push(ws.url());
  });
  const said: string[] = [];
  page.on("console", (m) => said.push(m.text()));
  await page.goto("/");
  await page.waitForFunction(() => window.__illogical?.control?.phase === "ready", null, { timeout: 20_000 });
  await expect.poll(() => said.some((s) => s.includes("relay: control is full")), { timeout: 15_000 }).toBe(true);
  // It waits half a minute or so: no more tries in the next ten seconds.
  const tries = relay.length;
  await page.waitForTimeout(10_000);
  expect(relay.length).toBe(tries);
  expect(tries).toBeLessThanOrEqual(2);
  const hosts = await page.evaluate(() => (window.__illogical?.fleet?.list ?? []).map((h) => [h.name, h.state]));
  expect(hosts).not.toContainEqual(["mac", "connected"]);

  // Control's pages and sign-ins don't wait on the relay.
  const someoneElse = await (await browser.newContext()).newPage();
  await signIn(someoneElse);
});
