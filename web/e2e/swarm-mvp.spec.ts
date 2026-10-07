// The swarm MVP (#44), end to end in one flow. Jake and Sam are on a team,
// each with a laptop and a phone and two machines of their own, and the team
// has a box. Everything is on loopback, with every daemon reached through a
// local control's relay, standing in for different networks; the phones are
// Pixel-sized touch contexts (real phones are out of reach).
//
// - Both people's swarms show all of the team's shared panes, grouped by
//   person: themselves, the other, and the team.
// - Claude Code asks to run a tool in a terminal on Jake's machine (its
//   PermissionRequest hook, fed S18's recorded inputs through
//   `arugula hook`). The card lifts to all four rails; Sam allows it from
//   the phone's strip, and every card says "Allowed by sam".
// - Sam sends "now run the tests" as the follow-up: on Jake's own machine
//   after Jake's trust grant, on the team's box straight through. It reaches
//   Claude Code through its inbox hook, and `arugula log --who` and
//   history attribute the approval and the follow-up to Sam.

import { execFileSync, spawn, type ChildProcess } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createServer, type Server } from "node:http";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { devices, expect, test, type Browser, type BrowserContextOptions, type Page } from "@playwright/test";
import { ANY, controlPort, listen } from "./ports";
import { closeContexts } from "./helpers";
import { labs } from "./labs";

test.afterAll(closeContexts);

let base = "";
let github = "";
const cli = resolve("../target/debug/arugula");
const fixtures = resolve("../crates/daemon/tests/fixtures");
const procs: ChildProcess[] = [];
const dirs: string[] = [];
const states = new Map<string, string>();
let gh: Server;

test.describe.configure({ mode: "serial" });
test.use({ baseURL: async ({}, use) => use(base) });

const { viewport, userAgent, deviceScaleFactor, isMobile, hasTouch } = devices["Pixel 7"];
const PHONE = { viewport, userAgent, deviceScaleFactor, isMobile, hasTouch };

function temp(what: string) {
  const d = mkdtempSync(join(tmpdir(), `arugula-e2e-mvp-${what}-`));
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

/** Someone's first browser: signed in, its recovery codes saved. */
async function laptop(browser: Browser, login: string, opts: BrowserContextOptions = {}): Promise<Page> {
  const ctx = await browser.newContext(opts);
  await ctx.addCookies([{ name: "as", value: login, url: github }]);
  const page = await ctx.newPage();
  await page.goto("/");
  await page.locator("[data-signin=github]").click();
  await page.locator("[data-stored-codes]").check();
  await page.locator("[data-saved-codes]").click();
  await page.waitForFunction(() => window.__arugula?.control?.phase === "ready");
  return page;
}

/** Their phone: signed in as the same person, approved from the laptop. */
async function phone(browser: Browser, login: string, approver: Page): Promise<Page> {
  const ctx = await browser.newContext(PHONE);
  await ctx.addCookies([{ name: "as", value: login, url: github }]);
  const page = await ctx.newPage();
  await page.goto("/");
  await page.locator("[data-signin=github]").click();
  await expect(page.getByText("Approve this browser")).toBeVisible();
  const fp = await page.locator("[data-fingerprint]").getAttribute("data-fingerprint");
  await approver.goto("/");
  await approver.waitForFunction(() => window.__arugula?.control?.phase === "ready");
  await approver.evaluate(() => window.__arugula.control!.refresh());
  await expect(approver.locator(`[data-pending="${fp}"]`)).toBeVisible({ timeout: 20_000 });
  await approver.locator("[data-approve]").click();
  await page.waitForFunction(() => window.__arugula?.control?.phase === "ready", undefined, { timeout: 20_000 });
  return page;
}

/** `arugulad join` (the team's with `team`), approved from `page`; then
 * the daemon runs, reached only through the relay. */
async function machine(page: Page, name: string, team?: string) {
  const state = temp(name);
  states.set(name, state);
  const args = ["join", base, "--name", name, "--state-dir", state, ...(team ? ["--team", team] : [])];
  const joining = spawn("../target/debug/arugulad", args, { stdio: ["pipe", "pipe", "ignore"] });
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
      "../target/debug/arugulad",
      [
        ...["--listen", ANY, "--name", name, "--state-dir", labs(state)],
        ...["--shell", "bash --norc --noprofile", "--no-manager-env", "--tailscale-socket", "/nonexistent/sock"],
      ],
      { stdio: "ignore" },
    ),
  );
}

/** A request to `host`'s API from this page, over its fleet channel. */
const call = (page: Page, host: string, method: string, path: string, body?: unknown) =>
  page.evaluate(
    async ([h, m, p, b]) => {
      const r = await window.__arugula.fleet.request(h as string, m as string, p as string, b);
      return { ok: r.ok, status: r.status, body: await r.json<Record<string, unknown>>().catch(() => null) };
    },
    [host, method, path, body] as const,
  );

/** A pane's screen, wrapping undone. */
const capture = (page: Page, host: string, pane: number) =>
  page.evaluate(
    async ([h, p]) => (await (await window.__arugula.fleet.request(h, "GET", `/api/panes/${p}/capture?format=text`)).text!()).replace(/\n/g, ""),
    [host, pane] as const,
  );

const live = (page: Page, hosts: string[]) =>
  page.evaluate((hs) => hs.every((h) => window.__arugula?.fleet?.host(h)?.state === "connected"), hosts);

/** Claude Code's hooks for one tool call, then its inbox, as a shell line:
 * what Claude Code would run, fed S18's recorded inputs. */
function claudeHooks(tag: string, command: string): string {
  const d = temp(tag);
  const read = (n: string) => JSON.parse(readFileSync(join(fixtures, n), "utf8"));
  const pre = { ...read("s18-hook-pretooluse-bash.json"), tool_input: { command }, tool_use_id: `toolu_${tag}` };
  const perm = { ...read("s18-hook-permission.json"), tool_input: { command } };
  writeFileSync(join(d, "pre.json"), JSON.stringify(pre));
  writeFileSync(join(d, "perm.json"), JSON.stringify(perm));
  writeFileSync(join(d, "stop.json"), JSON.stringify(read("s18-hook-stop.json")));
  return `${cli} hook < ${d}/pre.json; ${cli} hook < ${d}/perm.json; ${cli} inbox < ${d}/stop.json; echo ${tag}-inbox-$((6*7))`;
}

/** The swarm, with these hosts' panes in it. */
async function swarm(page: Page, hosts: string[]) {
  await page.evaluate(() => window.__arugula.control!.refresh());
  await expect.poll(() => live(page, hosts), { timeout: 60_000 }).toBe(true);
  await page.evaluate(() => (location.hash = "#swarm"));
  await expect(page.locator(".swarm")).toBeVisible();
}

const clusters = (page: Page) =>
  page.evaluate(() => ((window.__arugula.swarm as { clusters: { name: string }[] } | null)?.clusters ?? []).map((c) => c.name).sort());
const keys = (page: Page) => page.evaluate(() => window.__arugula.fleet.panes.map((p) => p.key).sort());

let jake: Page;
let jakePhone: Page;
let sam: Page;
let samPhone: Page;
const JAKES = ["mac", "studio"];
const SAMS = ["sam-1", "sam-2"];
const ALL = [...JAKES, ...SAMS, "teambox"];

test("two people, a laptop and a phone each, two machines each and a team box", async ({ browser }) => {
  test.setTimeout(180_000);
  jake = await laptop(browser, "jake");
  await jake.evaluate(() => window.__arugula.control!.createTeam("Acme"));
  const team = await jake.evaluate(() => window.__arugula.control!.teams[0].team);
  sam = await laptop(browser, "sam");
  const link = await jake.evaluate((t) => window.__arugula.control!.invite(t, "editor", true), team);
  await sam.goto(link);
  await sam.locator("[data-accept-invite]").click();
  await jake.evaluate(() => window.__arugula.control!.refresh());
  await expect(jake.locator("[data-admit-yes]")).toBeVisible({ timeout: 15_000 });
  await jake.locator("[data-admit-yes]").click();
  await expect.poll(() => jake.evaluate(() => window.__arugula.control!.teams[0].roster.members.length), { timeout: 15_000 }).toBe(2);
  jakePhone = await phone(browser, "jake", jake);
  samPhone = await phone(browser, "sam", sam);

  await machine(jake, "teambox", team);
  for (const m of JAKES) await machine(jake, m);
  for (const m of SAMS) await machine(sam, m);
  for (const p of [jake, sam]) {
    await p.goto("/");
    await p.waitForFunction(() => window.__arugula?.control?.phase === "ready");
  }
  await expect.poll(() => live(jake, ["teambox", ...JAKES]), { timeout: 60_000 }).toBe(true);
  await expect.poll(() => live(sam, ["teambox", ...SAMS]), { timeout: 60_000 }).toBe(true);

  // What each shares: Jake's mac with Sam (an editor), Jake's studio with
  // the team, Sam's first machine with the team and the second with Jake.
  const samAcct = await jake.evaluate(() => window.__arugula.control!.person("sam"));
  const jakeAcct = await sam.evaluate(() => window.__arugula.control!.person("jake"));
  const teamPin = (p: Page) =>
    p.evaluate(() => {
      const t = window.__arugula.control!.teams[0];
      return { principal: `team:${t.team}`, root: `${t.pin.founder}.${t.pin.founder_root}`, name: t.roster.name };
    });
  const share = async (p: Page, host: string, grant: { principal: string; root: string; name: string }, role: string) =>
    expect((await call(p, host, "POST", "/api/acl", { session: 1, role, ...grant })).ok).toBe(true);
  await share(jake, "mac", { principal: `account:${samAcct.account}`, root: samAcct.root, name: "sam" }, "editor");
  await share(jake, "studio", await teamPin(jake), "editor");
  await share(sam, "sam-1", await teamPin(sam), "editor");
  await share(sam, "sam-2", { principal: `account:${jakeAcct.account}`, root: jakeAcct.root, name: "jake" }, "viewer");
});

test("both people's swarms show the team's shared panes, grouped by person", async () => {
  test.setTimeout(120_000);
  for (const p of [jake, jakePhone, sam, samPhone]) await swarm(p, ALL);
  for (const p of [jake, jakePhone, sam, samPhone]) {
    await expect.poll(() => keys(p)).toEqual(expect.arrayContaining(["mac:1", "studio:1", "sam-1:1", "sam-2:1", "teambox:1"]));
    await p.locator('[data-g="person"]').click();
  }
  for (const p of [jake, jakePhone]) await expect.poll(() => clusters(p)).toEqual(["sam", "team Acme", "you"]);
  for (const p of [sam, samPhone]) await expect.poll(() => clusters(p)).toEqual(["jake", "team Acme", "you"]);
});

test("Claude Code's approval on Jake's machine reaches both rails; Sam allows it from the phone", async () => {
  test.setTimeout(120_000);
  // Jake's tab view shows his mac underneath, so his trust prompt has a home.
  await jake.evaluate(() => window.__arugula.hosts.select("mac"));
  expect((await call(jake, "mac", "POST", "/api/panes/1/send", { text: claudeHooks("mac", "cargo test"), enter: true })).ok).toBe(true);
  const card = (p: Page) => p.locator('.swarm-card[data-bundle][data-panes~="mac:1"]');
  for (const p of [jake, jakePhone, sam, samPhone]) {
    await expect(card(p)).toBeVisible({ timeout: 20_000 });
    await expect(card(p).locator(".agent-perm-cmd")).toHaveText("cargo test");
  }
  await card(samPhone).scrollIntoViewIfNeeded();
  await card(samPhone).getByRole("button", { name: "Allow", exact: true }).tap();
  for (const p of [jake, jakePhone, sam, samPhone]) {
    await expect(p.locator('.swarm-card[data-answered][data-panes="mac:1"] .answered-by')).toHaveText(/^Allowed by sam, \d\d:\d\d/, {
      timeout: 15_000,
    });
  }
  await expect.poll(() => capture(jake, "mac", 1)).toContain('"behavior":"allow"');
});

test("Sam's follow-up needs Jake's trust on his machine, then reaches the agent", async () => {
  await expect.poll(() => jake.evaluate(() => window.__arugula.fleet.panes.find((p) => p.key === "mac:1")?.info.inbox)).toBe(true);
  const done = samPhone.locator('.swarm-card[data-answered][data-panes="mac:1"]');
  await done.locator(".followup input").fill("now run the tests");
  await done.locator(".followup button").tap();
  const ask = done.getByRole("button", { name: /^Ask .+ for 30 minutes$/ });
  await expect(ask).toBeVisible();
  await ask.tap();
  // Jake lets Sam drive it for half an hour, from the tab view.
  await jake.evaluate(() => (location.hash = ""));
  await expect(jake.locator('[data-trust-request="1"]')).toBeVisible({ timeout: 15_000 });
  await jake.locator("[data-trust]").click();
  // The grant goes over Jake's connection and the follow-up over Sam's:
  // send it once the daemon says Sam is trusted, or it can get there first
  // and be refused again.
  await expect.poll(() => jake.evaluate(() => window.__arugula.fleet.panes.find((p) => p.key === "mac:1")?.info.trusted?.length ?? 0)).toBe(1);
  await done.locator(".followup input").fill("now run the tests");
  await done.locator(".followup button").tap();
  await expect(done.locator(".followup-sent")).toHaveText("Sent.");
  await expect.poll(() => capture(jake, "mac", 1)).toContain("A follow-up from sam (sent through Arugula): now run the tests");
  await expect.poll(() => capture(jake, "mac", 1)).toContain("mac-inbox-42");

  // The pane's history attributes both to Sam.
  const sock = join(states.get("mac")!, "sock");
  expect(execFileSync(cli, ["--socket", sock, "log", "%1", "--who"], { encoding: "utf8" })).toContain("sam");
  const hist = JSON.parse(execFileSync(cli, ["--socket", sock, "--json", "history", "--pane", "%1"], { encoding: "utf8" })) as {
    text: string;
    by?: string;
  }[];
  expect(hist).toEqual(
    expect.arrayContaining([
      expect.objectContaining({ text: "allowed: Bash: cargo test", by: "sam" }),
      expect.objectContaining({ text: "follow-up: now run the tests", by: "sam" }),
    ]),
  );
});

test("on the team's box, Sam's answer and follow-up go straight through", async () => {
  await jake.evaluate(() => (location.hash = "#swarm"));
  expect((await call(jake, "teambox", "POST", "/api/panes/1/send", { text: claudeHooks("box", "cargo build"), enter: true })).ok).toBe(true);
  const card = (p: Page) => p.locator('.swarm-card[data-bundle][data-panes~="teambox:1"]');
  for (const p of [jake, samPhone]) await expect(card(p)).toBeVisible({ timeout: 20_000 });
  await card(samPhone).scrollIntoViewIfNeeded();
  await card(samPhone).getByRole("button", { name: "Allow", exact: true }).tap();
  await expect(jake.locator('.swarm-card[data-answered][data-panes="teambox:1"] .answered-by')).toHaveText(/^Allowed by sam/, {
    timeout: 15_000,
  });
  await expect.poll(() => sam.evaluate(() => window.__arugula.fleet.panes.find((p) => p.key === "teambox:1")?.info.inbox)).toBe(true);
  const done = samPhone.locator('.swarm-card[data-answered][data-panes="teambox:1"]');
  await done.locator(".followup input").fill("now run the tests");
  await done.locator(".followup button").tap();
  await expect(done.locator(".followup-sent")).toHaveText("Sent.");
  await expect.poll(() => capture(jake, "teambox", 1)).toContain("A follow-up from sam (sent through Arugula): now run the tests");
  await expect.poll(() => capture(jake, "teambox", 1)).toContain("box-inbox-42");
});
