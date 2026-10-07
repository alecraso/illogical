// #206: deleting an account that founded a team. Alice founds Acme and
// adds a team machine; Bob joins with a presigned link and uses the box
// through control's relay. When Alice deletes her account, Bob's relayed
// connection ends at once (not at the box's next check, a minute later),
// the box leaves his host list, and his page says the team was deleted.
// Bob's own box, which was Acme's, is hung up on too and dials back in.

import { spawn, type ChildProcess } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { createServer, type Server } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test, type Browser, type Page } from "@playwright/test";
import { controlPanel, ready, run, closeContexts } from "./helpers";
import { ANY, controlPort, listen } from "./ports";
import { labs } from "./labs";

test.afterAll(closeContexts);

let base = "";
let github = "";
const procs: ChildProcess[] = [];
const dirs: string[] = [];
let gh: Server;

test.describe.configure({ mode: "serial" });
test.use({ baseURL: async ({}, use) => use(base) });

function temp(what: string) {
  const d = mkdtempSync(join(tmpdir(), `arugula-e2e-delete-${what}-`));
  dirs.push(d);
  return d;
}

test.beforeAll(async () => {
  // A fake GitHub with as many users as there are `as` cookies.
  gh = createServer((req, res) => {
    const u = new URL(req.url!, "http://github");
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
  });
  github = `http://127.0.0.1:${await listen(gh)}`;
  const db = join(temp("db"), "control.db");
  procs.push(
    spawn(
      "../target/debug/arugula-control",
      [
        ...["--listen", ANY, "--public-url", "http://127.0.0.1:0", "--db", db],
        ...["--github-client-id", "id", "--github-client-secret", "s", "--static-dir", "dist"],
        ...["--github-url", github, "--github-api", github],
      ],
      { stdio: "ignore" },
    ),
  );
  base = `http://127.0.0.1:${await controlPort(db, procs.at(-1))}`;
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
  await ctx.addCookies([{ name: "as", value: login, url: github }]);
  const page = await ctx.newPage();
  await page.goto("/");
  await page.locator("[data-signin=github]").click();
  await page.locator("[data-stored-codes]").check();
  await page.locator("[data-saved-codes]").click();
  await page.waitForFunction(() => window.__arugula?.control?.phase === "ready");
  return page;
}

const hostNames = (page: Page) => page.evaluate(() => window.__arugula.hosts.names);
const connected = (page: Page) => page.evaluate(() => window.__arugula.client.connected);

/** `arugulad join --team`, approved by `owner`; then the daemon runs. */
async function teamMachine(owner: Page, name: string, team: string) {
  const state = temp(name);
  const joining = spawn("../target/debug/arugulad", ["join", base, "--name", name, "--state-dir", state, "--team", team], {
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
  await owner.goto(link);
  await expect(owner.locator("[data-join-to]")).toHaveValue(team);
  const account = await owner.locator("[data-join-account]").getAttribute("data-join-account");
  await owner.locator("[data-approve-join]").click();
  joining.stdin!.end(`${account}\n`);
  expect(await exited).toBe(0);
  procs.push(
    spawn(
      "../target/debug/arugulad",
      [
        ...["--listen", ANY, "--name", name, "--state-dir", labs(state)],
        ...["--shell", "bash --norc --noprofile", "--no-manager-env", "--tailscale-socket", "/nonexistent/sock"],
      ],
      { stdio: "ignore" },
    ),
  );
}

test("a founder deletes their account: members lose the team's machines at once, and hear why", async ({ browser }) => {
  test.setTimeout(120_000);
  const alice = await person(browser, "alice");
  await alice.evaluate(() => window.__arugula.control!.createTeam("Acme"));
  const team = await alice.evaluate(() => window.__arugula.control!.teams[0].team);
  const link = await alice.evaluate((t) => window.__arugula.control!.invite(t, "editor"), team);
  const bob = await person(browser, "bob");
  const seed = link.split("#pinvite=")[1].split(".")[1];
  await bob.evaluate(([t, s]) => window.__arugula.control!.redeem(t, s), [team, seed]);
  await teamMachine(alice, "acmebox", team);
  // Alice makes Bob an owner, and he puts his own box in Acme too.
  await alice.evaluate(async (t) => {
    const c = window.__arugula.control!;
    await c.refresh();
    await c.changeTeam(t, (ms) => ms.map((m) => (m.name === "bob" ? { ...m, role: "owner" } : m)));
  }, team);
  await bob.evaluate(() => window.__arugula.control!.refresh());
  await teamMachine(bob, "bobbox", team);

  // Bob works on Acme's box through the relay.
  await bob.goto("/");
  await bob.waitForFunction(() => window.__arugula?.control?.phase === "ready");
  await expect.poll(async () => (await hostNames(bob)).sort(), { timeout: 30_000 }).toEqual(["acmebox", "bobbox"]);
  await bob.evaluate(() => window.__arugula.hosts.select("acmebox"));
  await expect.poll(() => connected(bob), { timeout: 30_000 }).toBe(true);
  expect(await bob.evaluate(() => window.__arugula.client.path)).toBe("relayed");
  const pane = await bob.evaluate(() => window.__arugula.client.state!.panes[0].id);
  await ready(bob, pane);
  await run(bob, pane, "echo before-$((6*7))", "before-42");

  // Alice deletes her account; Acme goes with it.
  await alice.goto("/");
  await alice.waitForFunction(() => window.__arugula?.control?.phase === "ready");
  await controlPanel(alice, "account");
  await alice.locator("[data-delete-account]").click();
  await expect(alice.locator("[data-disband]")).toContainText("Acme");
  await alice.locator("[data-delete-confirm]").fill("alice");
  const deleted = Date.now();
  await alice.locator("[data-delete-go]").click();
  await expect(alice.locator("[data-signin=github]")).toBeVisible();

  // Bob's relayed connection to Alice's box ends now, well inside the
  // box's own minute between checks.
  await expect.poll(() => connected(bob), { timeout: 5_000, intervals: [100] }).toBe(false);
  expect(Date.now() - deleted).toBeLessThan(5_000);
  // His page says what happened, and the box is gone from his list.
  const notice = bob.locator(".control-prompt").filter({ has: bob.locator("[data-notice]") });
  await expect(notice.locator("h2")).toHaveText("Acme was deleted", { timeout: 15_000 });
  await expect(notice).toContainText("Its founder deleted their account.");
  await expect.poll(() => hostNames(bob), { timeout: 15_000 }).toEqual(["bobbox"]);
  await notice.getByRole("button", { name: "OK", exact: true }).click();
  await expect(bob.locator("[data-notice]")).toHaveCount(0);
  // Seen once: a reload doesn't show it again.
  await bob.reload();
  await bob.waitForFunction(() => window.__arugula?.control?.phase === "ready");
  await bob.evaluate(() => window.__arugula.control!.refresh());
  await expect(bob.locator("[data-notice]")).toHaveCount(0);
  // His own box was hung up on too (it was Acme's); it dialled back in,
  // his now, and works through the relay as before.
  await bob.evaluate(() => window.__arugula.hosts.select("bobbox"));
  await expect.poll(() => connected(bob), { timeout: 30_000 }).toBe(true);
  expect(await bob.evaluate(() => window.__arugula.client.path)).toBe("relayed");
  await expect.poll(() => bob.evaluate(() => window.__arugula.client.state?.panes.length ?? 0)).toBeGreaterThan(0);
  const own = await bob.evaluate(() => window.__arugula.client.active()!);
  await ready(bob, own);
  await run(bob, own, "echo after-$((6*7))", "after-42");
});
