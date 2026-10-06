// M30: the team's swarm. Alice and Bob are on a team; each has two machines,
// and the team has a box. Alice shares a session on one of hers with Bob,
// and one on the other with the whole team; Bob shares one of his with
// Alice. Each page's fleet holds everything that person can see: their
// own machines, the team's box and what was shared with them, and the two
// pages agree on what they share. Each sees their own private panes, never
// the other's. Grouped by person it's three groups: me, the other, and
// the team. Revoking a share takes its panes (and their cards) out of the
// other's fleet within a second; locking the team takes the team's.

import { spawn, type ChildProcess } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { createServer, type Server } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test, type Browser, type Page } from "@playwright/test";
import { ANY, controlPort, listen } from "./ports";
import { closeContexts } from "./helpers";
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
  const d = mkdtempSync(join(tmpdir(), `illogical-e2e-tswarm-${what}-`));
  dirs.push(d);
  return d;
}

test.beforeAll(async () => {
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
      "../target/debug/illogical-control",
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
  await page.waitForFunction(() => window.__illogical?.control?.phase === "ready");
  return page;
}

/** `illogicald join` (for a team with `team`), approved from `page`; then
 * the daemon runs, reachable only through the relay. */
async function addMachine(page: Page, name: string, team?: string) {
  const state = temp(name);
  const joining = spawn("../target/debug/illogicald", ["join", base, "--name", name, "--state-dir", state, ...(team ? ["--team", team] : [])], {
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
  await page.goto(link);
  // The machine asks whether the account is the one this browser shows.
  const account = await page.locator("[data-join-account]").getAttribute("data-join-account");
  await page.locator("[data-approve-join]").click();
  joining.stdin!.end(`${account}\n`);
  expect(await exited).toBe(0);
  procs.push(
    spawn(
      "../target/debug/illogicald",
      [
        ...["--listen", ANY, "--name", name, "--state-dir", labs(state)],
        ...["--shell", "bash --norc --noprofile", "--no-manager-env", "--tailscale-socket", "/nonexistent/sock"],
      ],
      { stdio: "ignore" },
    ),
  );
}

async function home(page: Page) {
  await page.goto("/");
  await page.waitForFunction(() => window.__illogical?.control?.phase === "ready");
}

/** Show `host` in the tab view, connected. */
async function show(page: Page, host: string) {
  await expect.poll(() => page.evaluate(() => window.__illogical.hosts.names), { timeout: 30_000 }).toContain(host);
  await page.evaluate((h) => window.__illogical.hosts.select(h), host);
  await expect
    .poll(() => page.evaluate((h) => window.__illogical.hosts.current === h && window.__illogical.client.connected && !!window.__illogical.client.state, host), {
      timeout: 30_000,
    })
    .toBe(true);
}

/** A request to the shown host's API, through its channel. */
const call = (page: Page, method: string, path: string, body?: unknown) =>
  page.evaluate(
    async ([m, p, b]) => {
      const r = await window.__illogical.client.request(m as string, p as string, b);
      return { ok: r.ok, status: r.status, body: await r.json<Record<string, unknown>>().catch(() => null) };
    },
    [method, path, body] as const,
  );

const keys = (page: Page) =>
  page.evaluate(() => (window.__illogical?.fleet?.panes ?? []).map((p) => p.key).sort());
const live = (page: Page, hosts: string[]) =>
  page.evaluate((hs) => hs.every((h) => window.__illogical?.fleet?.host(h)?.state === "connected"), hosts);
const groups = (page: Page) =>
  page.evaluate(() => window.__illogical.fleet.byPerson().map((g) => `${g.person.kind}:${g.person.name}:${new Set(g.panes.map((p) => p.host)).size}`));

let alice: Page;
let bob: Page;
let team = "";
/** Panes each share puts in the other's view, as fleet keys. */
const sharedA1: string[] = [];
const sharedA2: string[] = [];
const sharedB1: string[] = [];
let privateA1 = "";
let privateB1 = "";

test("two teammates with two machines each and a team box", async ({ browser }) => {
  test.setTimeout(120_000);
  alice = await person(browser, "alice");
  await alice.evaluate(() => window.__illogical.control!.createTeam("Acme"));
  team = await alice.evaluate(() => window.__illogical.control!.teams[0].team);
  const link = await alice.evaluate((t) => window.__illogical.control!.invite(t, "editor", true), team);
  bob = await person(browser, "bob");
  await bob.goto(link);
  await bob.locator("[data-accept-invite]").click();
  await expect(bob.locator("[data-invite-pending]")).toBeVisible();
  await alice.evaluate(() => window.__illogical.control!.refresh());
  await expect(alice.locator("[data-admit-yes]")).toBeVisible({ timeout: 15_000 });
  await alice.locator("[data-admit-yes]").click();
  await expect
    .poll(() => alice.evaluate(() => window.__illogical.control!.teams[0].roster.members.length), { timeout: 15_000 })
    .toBe(2);

  await addMachine(alice, "buildbox", team);
  await addMachine(alice, "a1");
  await addMachine(alice, "a2");
  await addMachine(bob, "b1");
  await addMachine(bob, "b2");
  for (const p of [alice, bob]) await home(p);
  await expect.poll(() => live(alice, ["buildbox", "a1", "a2"]), { timeout: 30_000 }).toBe(true);
  await expect.poll(() => live(bob, ["buildbox", "b1", "b2"]), { timeout: 30_000 }).toBe(true);
});

test("shares with a person and with the team: both see the same team swarm", async () => {
  test.setTimeout(120_000);
  const bobAcct = await alice.evaluate(() => window.__illogical.control!.person("bob"));
  const aliceAcct = await bob.evaluate(() => window.__illogical.control!.person("alice"));

  // a1: session 1 shared with Bob (its pane, and a private one beside it);
  // a second session stays Alice's own.
  await show(alice, "a1");
  const a1Split = (await call(alice, "POST", "/api/run", { split: 1 })).body!.pane as number;
  await alice.evaluate((p) => window.__illogical.client.paneOp(p, { op: "set_private", on: true }), a1Split);
  await alice.evaluate(() => window.__illogical.client.intent({ op: "new_session", name: "mine", from_pane: null }));
  expect(
    (await call(alice, "POST", "/api/acl", { session: 1, principal: `account:${bobAcct.account}`, role: "editor", root: bobAcct.root, name: "bob" })).ok,
  ).toBe(true);
  sharedA1.push("a1:1");
  privateA1 = `a1:${a1Split}`;

  // a2: session 1 shared with the whole team, pinned to its founder.
  await show(alice, "a2");
  const pin = await alice.evaluate(() => {
    const t = window.__illogical.control!.teams[0];
    return { id: t.team, root: `${t.pin.founder}.${t.pin.founder_root}`, name: t.roster.name };
  });
  expect((await call(alice, "POST", "/api/acl", { session: 1, principal: `team:${pin.id}`, role: "editor", root: pin.root, name: pin.name })).ok).toBe(true);
  sharedA2.push("a2:1");

  // b1: Bob shares session 1 with Alice, with a private pane in it.
  await show(bob, "b1");
  const b1Split = (await call(bob, "POST", "/api/run", { split: 1 })).body!.pane as number;
  await bob.evaluate((p) => window.__illogical.client.paneOp(p, { op: "set_private", on: true }), b1Split);
  expect(
    (await call(bob, "POST", "/api/acl", { session: 1, principal: `account:${aliceAcct.account}`, role: "viewer", root: aliceAcct.root, name: "alice" })).ok,
  ).toBe(true);
  sharedB1.push("b1:1");
  privateB1 = `b1:${b1Split}`;

  // The machines tell control who they let in; the directory lists them.
  for (const p of [alice, bob]) await p.evaluate(() => window.__illogical.control!.refresh());
  await expect
    .poll(async () => (await bob.evaluate(() => window.__illogical.control!.refresh()), live(bob, ["a1", "a2", "buildbox", "b1", "b2"])), {
      timeout: 60_000,
    })
    .toBe(true);
  await expect
    .poll(async () => (await alice.evaluate(() => window.__illogical.control!.refresh()), live(alice, ["b1", "a1", "a2", "buildbox"])), {
      timeout: 60_000,
    })
    .toBe(true);

  // The team swarm (the team's box and everything shared between them) is
  // the same on both pages.
  const swarm = (ks: string[]) =>
    ks.filter((k) => k.startsWith("buildbox:") || [...sharedA1, ...sharedA2, ...sharedB1].includes(k));
  await expect.poll(async () => swarm(await keys(bob))).toEqual(swarm(await keys(alice)));
  expect(swarm(await keys(alice))).toEqual(expect.arrayContaining(["buildbox:1", "a1:1", "a2:1", "b1:1"]));
  // Bob gets only what was shared from Alice's machines: not her own session.
  expect((await keys(bob)).filter((k) => k.startsWith("a1:"))).toEqual(["a1:1"]);
  expect((await keys(alice)).some((k) => k.startsWith("b2:"))).toBe(false);
});

test("each sees their own private panes, never the other's", async () => {
  expect(await keys(alice)).toContain(privateA1);
  expect(await keys(bob)).not.toContain(privateA1);
  expect(await keys(bob)).toContain(privateB1);
  expect(await keys(alice)).not.toContain(privateB1);
});

test("by person it's three groups: me, the other, and the team", async () => {
  await expect.poll(() => groups(alice)).toEqual(["me:alice:2", "person:bob:1", "team:Acme:1"]);
  await expect.poll(() => groups(bob)).toEqual(["me:bob:2", "person:alice:2", "team:Acme:1"]);
  // Each pane says who drives it and who's looking (M13).
  await show(alice, "buildbox");
  await alice.locator('[data-pane="1"]').click({ position: { x: 40, y: 40 } });
  await alice.keyboard.type("echo hi\n");
  await expect
    .poll(() => bob.evaluate(() => window.__illogical.fleet.panes.find((p) => p.key === "buildbox:1")?.driver?.name ?? null))
    .toBe("alice");
  await expect
    .poll(() => bob.evaluate(() => window.__illogical.fleet.panes.find((p) => p.key === "buildbox:1")?.watchers.map((w) => w.name) ?? []))
    .toEqual(["alice"]);
});

test("revoking a share takes its panes and their cards out within a second", async () => {
  // A card on the shared pane: it needs someone.
  await show(alice, "a1");
  expect((await call(alice, "POST", "/api/panes/1/attention", { state: "needs_input" })).ok).toBe(true);
  await expect
    .poll(() => bob.evaluate(() => window.__illogical.fleet.panes.find((p) => p.key === "a1:1")?.info.attention ?? null))
    .toBe("needs_input");
  const bobAcct = await alice.evaluate(() => window.__illogical.control!.person("bob"));
  const t0 = Date.now();
  expect((await call(alice, "POST", "/api/acl", { session: 1, principal: `account:${bobAcct.account}`, role: null })).ok).toBe(true);
  await expect.poll(async () => (await keys(bob)).filter((k) => k.startsWith("a1:")), { timeout: 3000, intervals: [50] }).toEqual([]);
  expect(Date.now() - t0).toBeLessThan(1000);
  // Not greyed and kept: gone, and nothing on Bob's side wants him there.
  expect(await bob.evaluate(() => window.__illogical.fleet.panes.filter((p) => p.host === "a1" && p.info.attention === "needs_input").length)).toBe(0);
});

test("locking the team takes the team's panes out of the other's fleet", async () => {
  await expect.poll(async () => (await keys(bob)).some((k) => k.startsWith("buildbox:"))).toBe(true);
  const t0 = Date.now();
  await alice.evaluate((t) => window.__illogical.control!.lockTeam(t, true), team);
  await expect
    .poll(async () => (await keys(bob)).filter((k) => k.startsWith("buildbox:") || k.startsWith("a2:")), { timeout: 5000, intervals: [50] })
    .toEqual([]);
  console.log(`team locked: Bob's fleet lost the team's panes in ${Date.now() - t0} ms`);
  // Alice, an owner, still has them.
  expect((await keys(alice)).some((k) => k.startsWith("buildbox:"))).toBe(true);
});
