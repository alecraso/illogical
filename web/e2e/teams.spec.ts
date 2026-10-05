// M19: teams. Two people at different companies, neither on a tailnet, join
// a team by invite; a team-owned box joins the team; both use it through
// control's relay, see each other there and pass control back and forth;
// one runs a build on it. A read-only link works in a logged-out browser
// and dies at expiry. Removing a member cuts them off within a second.
// Joining (#100): the approver sees the team, picks it or Just me, and only
// its owners can approve; Cancel turns the daemon down. A presigned invite
// lets someone already in another team in with one click, no owner's yes.

import { spawn, type ChildProcess } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { createServer, type Server } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test, type Browser, type Page } from "@playwright/test";
import { ready, run, text } from "./helpers";
import { ANY, controlPort, listen } from "./ports";

let base = "";
let github = "";
const procs: ChildProcess[] = [];
const dirs: string[] = [];
let gh: Server;

test.describe.configure({ mode: "serial" });
test.use({ baseURL: async ({}, use) => use(base) });

function temp(what: string) {
  const d = mkdtempSync(join(tmpdir(), `illogical-e2e-teams-${what}-`));
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

async function person(browser: Browser, login: string, before?: (page: Page) => Promise<void>): Promise<Page> {
  const ctx = await browser.newContext();
  await ctx.addCookies([{ name: "as", value: login, url: github }]);
  const page = await ctx.newPage();
  if (before) await before(page);
  else await page.goto("/");
  await page.locator("[data-signin=github]").click();
  await page.locator("[data-stored-codes]").check();
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
  // The Teams panel (#99): the team's id, its join command and an invite
  // link, each with Copy.
  // Alice has no machine yet: the Teams panel is on her header (#97).
  await alice.getByRole("button", { name: "Teams…" }).click();
  await expect(alice.locator("[data-teams-intro]")).toBeVisible();
  await expect(alice.locator(".control-roles")).toContainText("drives: also types");
  await expect(alice.locator("[data-team-id]")).toHaveText(team);
  await expect(alice.locator("[data-team-join]")).toHaveText(`illogicald join ${base} --team ${team}`);
  // Lock asks first, and says what it drops (#101).
  await alice.locator(`[data-lock="${team}"]`).click();
  await expect(alice.locator("[data-lock-warning]")).toContainText("open invites and requests are dropped");
  await alice.getByRole("button", { name: "Cancel" }).click();
  await expect(alice.locator("[data-lock-warning]")).toHaveCount(0);
  expect(await alice.evaluate(() => window.__illogical.control!.teams[0].locked)).toBe(false);
  // The invite's role is picked in the team's own section (default: drives).
  await expect(alice.locator(`[data-invite-role="${team}"]`)).toHaveValue("editor");
  // This one waits for her yes (the presigned kind is below).
  const section = alice.locator(`[data-team="${team}"]`);
  await section.locator("[data-invite-ask-first]").check();
  await alice.locator(`[data-invite="${team}"]`).click();
  await expect(section.locator("[data-invite-link]")).toContainText("#invite=");
  const link = (await section.locator("[data-invite-link]").textContent())!;
  await expect(section.locator("[data-invite-link] + [data-copy]")).toBeVisible();
  await alice.getByRole("button", { name: "Done" }).click();
  // Bob follows it with no account: the sign-in page says who invited him
  // to what (#103), and the invite is waiting once he's in.
  bob = await person(browser, "bob", async (p) => {
    await p.goto(link);
    await expect(p.locator("[data-why=invite]")).toContainText("alice invited you to Acme. Sign in or make an account to accept.");
  });
  await expect(bob.locator("[data-invite-team]")).toHaveText("Acme");
  await expect(bob.locator(".control-prompt")).toContainText("as someone who drives");
  await bob.locator("[data-accept-invite]").click();
  await expect(bob.locator("[data-invite-pending]")).toBeVisible();
  await bob.getByRole("button", { name: "Done" }).click();
  await expect(bob.locator(`[data-asked="${team}"]`)).toHaveText("Waiting for alice to add you to Acme. Their machines appear here when they do.");
  await expect(bob.getByRole("heading", { name: "Add your own machine" })).toBeVisible();
  // Alice is asked, sees Bob's fingerprint, and adds him (signing the roster).
  await alice.evaluate(() => window.__illogical.control!.refresh());
  await expect(alice.locator("[data-admit-yes]")).toBeVisible({ timeout: 15_000 });
  await expect(alice.locator(".control-prompt")).toContainText("used an invite, as someone who drives");
  await alice.locator("[data-admit-yes]").click();
  await expect
    .poll(() => alice.evaluate(() => window.__illogical.control!.teams[0].roster.members.map((m) => `${m.name}:${m.role}`)), { timeout: 15_000 })
    .toEqual(["alice:owner", "bob:editor"]);
  // Bob hears, without a reload.
  await expect(bob.locator(`[data-joined="${team}"]`)).toHaveText("You're in Acme", { timeout: 15_000 });
  await bob.getByRole("button", { name: "OK", exact: true }).click();
  await expect(bob.locator("[data-asked]")).toHaveCount(0);
  // A member who isn't an owner is told to ask one for machines.
  await bob.getByRole("button", { name: "Teams…" }).click();
  await expect(bob.locator("[data-ask-owner]")).toBeVisible();
  await expect(bob.locator("[data-team-join]")).toHaveCount(0);
  await expect(bob.locator(`[data-member] .dim`).first()).toHaveText("owner");
  await bob.getByRole("button", { name: "Done" }).click();
});

/** `illogicald join`, waiting for approval: its link, what it printed,
 * and how it ended. */
async function startJoin(name: string, state: string, extra: string[] = [], env: NodeJS.ProcessEnv = {}) {
  const joining = spawn("../target/debug/illogicald", ["join", base, "--name", name, "--state-dir", state, ...extra], {
    stdio: ["pipe", "pipe", "pipe"],
    env: { ...process.env, ...env },
  });
  procs.push(joining);
  let out = "";
  let err = "";
  joining.stderr!.on("data", (d) => (err += d));
  const exited = new Promise<number | null>((r) => joining.on("exit", r));
  const link = await new Promise<string>((res) => {
    joining.stdout!.on("data", (d) => {
      out += d;
      const m = out.match(/(http\S+#join=[A-Z0-9-]+)/);
      if (m) res(m[1]);
    });
  });
  // Answer the machine's question: the account's fingerprint, as `page`
  // shows it while approving.
  const confirm = async (page: Page) => {
    const account = await page.locator("[data-join-account]").getAttribute("data-join-account");
    return () => joining.stdin!.end(`${account}\n`);
  };
  return { link, exited, confirm, out: () => out, err: () => err };
}

function runDaemon(name: string, state: string) {
  procs.push(
    spawn(
      "../target/debug/illogicald",
      [
        ...["--listen", ANY, "--name", name, "--state-dir", state],
        ...["--shell", "bash --norc --noprofile", "--no-manager-env", "--tailscale-socket", "/nonexistent/sock"],
      ],
      { stdio: "ignore" },
    ),
  );
}

test("a team-owned box joins; both use it through the relay and pass control", async () => {
  const state = temp("box");
  const j = await startJoin("buildbox", state, ["--team", team]);
  expect(j.out()).toContain("To add this machine (buildbox) to the team Acme");
  expect(j.out()).toContain("Waiting for approval (the code lasts 15 minutes)");
  // #100: Bob (an editor) sees it's for the team, and can't approve it.
  await bob.goto(j.link);
  await expect(bob.locator("[data-join-team]")).toHaveText("Acme");
  await expect(bob.locator("[data-join-not-owner]")).toBeVisible();
  await expect(bob.locator("[data-approve-join]")).toBeDisabled();
  // Alice owns it: the team is picked already, and she's told what it grants.
  await alice.goto(j.link);
  await expect(alice.locator("[data-join-team]")).toHaveText("Acme");
  await expect(alice.locator("[data-join-to]")).toHaveValue(team);
  await expect(alice.locator("[data-join-grants]")).toContainText("The members of Acme reach it by their role");
  const answer = await j.confirm(alice);
  await alice.locator("[data-approve-join]").click();
  answer();
  expect(await j.exited).toBe(0);
  expect(j.out()).toContain("This machine is in the team Acme");
  expect(j.out()).toContain("illogicald isn't running here");
  runDaemon("buildbox", state);
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

test("a presigned invite: someone already in a team joins another in one click", async ({ browser }) => {
  // Carol has her own team already.
  const carol = await person(browser, "carol");
  await carol.evaluate(() => window.__illogical.control!.createTeam("Carols"));
  // A team machine that doesn't understand presigned invites (an older
  // daemon, played by one that says it understands nothing): Alice's link
  // asks her first, and says why.
  const old = temp("oldbox");
  const j = await startJoin("oldbox", old, ["--team", team], { ILLOGICAL_FEATURES: "" });
  await alice.goto(j.link);
  const answer = await j.confirm(alice);
  await alice.locator("[data-approve-join]").click();
  answer();
  expect(await j.exited).toBe(0);
  await alice.goto("/");
  await alice.waitForFunction(() => window.__illogical?.control?.phase === "ready");
  const section = alice.locator(`[data-team="${team}"]`);
  // The panel opens once the page's control overlay is listening.
  await expect(async () => {
    await alice.evaluate(() => window.dispatchEvent(new CustomEvent("illogical:control-panel", { detail: "teams" })));
    await expect(section).toBeVisible({ timeout: 500 });
  }).toPass({ timeout: 15_000 });
  await section.locator(`[data-invite-role="${team}"]`).selectOption("viewer");
  await expect(section.locator("[data-invite-ask-first]")).not.toBeChecked();
  await alice.locator(`[data-invite="${team}"]`).click();
  await expect(section.locator("[data-invite-why]")).toContainText("oldbox needs an update before one-click invites work in this team");
  await expect(section).toContainText("Anyone with this link can ask to join");
  // Once it's gone, presigned links are back (buildbox understands them).
  const left = spawn("../target/debug/illogicald", ["leave", "--state-dir", old], { stdio: "ignore" });
  expect(await new Promise((r) => left.on("exit", r))).toBe(0);
  // Alice makes a presigned link (the default) for someone who watches.
  await alice.locator(`[data-invite="${team}"]`).click();
  await expect(section.locator("[data-invite-why]")).toHaveCount(0);
  await expect(section).toContainText("One person can join with this link, within a day");
  const link = (await section.locator("[data-invite-link]").textContent())!;
  expect(link).toMatch(/#pinvite=[0-9a-f]{16}\.[0-9a-f]{64}$/);
  await alice.getByRole("button", { name: "Done" }).click();
  // A signed-out page still says who invited whom.
  const stranger = await (await browser.newContext()).newPage();
  await stranger.goto(link);
  await expect(stranger.locator("[data-why=invite]")).toContainText("alice invited you to Acme.");
  // Carol opens it: one button, and she's in, with no visit from Alice.
  await carol.goto(link);
  await expect(carol.locator("[data-invite-team]")).toHaveText("Acme");
  await expect(carol.locator(".control-prompt")).toContainText("Joining adds you right away");
  await carol.locator("[data-accept-invite]").click();
  await expect(carol.locator("[data-invite-joined]")).toBeVisible({ timeout: 15_000 });
  const mine = await carol.evaluate(() => window.__illogical.control!.teams.map((t) => `${t.roster.name}:${t.role}`).sort());
  expect(mine).toEqual(["Acme:viewer", "Carols:owner"]);
  await carol.getByRole("button", { name: "Done" }).click();
  // Alice's browser checks the version Carol wrote, as daemons do.
  await alice.evaluate(() => window.__illogical.control!.refresh());
  await expect
    .poll(() => alice.evaluate(() => window.__illogical.control!.teams.find((t) => t.roster.name === "Acme")!.roster.members.map((m) => `${m.name}:${m.role}`)))
    .toEqual(["alice:owner", "bob:editor", "carol:viewer"]);
  // The team's box takes the new roster: Carol reaches it.
  await carol.goto("/");
  await carol.waitForFunction(() => window.__illogical?.control?.phase === "ready");
  await expect.poll(() => hostNames(carol), { timeout: 30_000 }).toContain("buildbox");
  await expect.poll(() => carol.evaluate(() => window.__illogical.client.connected), { timeout: 30_000 }).toBe(true);
  // Now that the team's history has a presigned version, an older daemon
  // can't follow it, so it can't join the team.
  const late = spawn("../target/debug/illogicald", ["join", base, "--name", "latebox", "--state-dir", temp("latebox"), "--team", team], {
    stdio: ["ignore", "ignore", "pipe"],
    env: { ...process.env, ILLOGICAL_FEATURES: "" },
  });
  let lateErr = "";
  late.stderr!.on("data", (d) => (lateErr += d));
  expect(await new Promise((r) => late.on("exit", r))).not.toBe(0);
  expect(lateErr).toContain("this machine needs an update before it can join this team");
  // Once: the same link does nothing for anyone after her. Dave opens it
  // signed out and signs in with GitHub: he's back at the invite, and his
  // browser never sent its one-time key anywhere (control could use it).
  const seed = link.split(".").at(-1)!;
  const sent: string[] = [];
  const dave = await person(browser, "dave", async (page) => {
    page.on("request", (r) => sent.push(`${r.url()} ${r.postData() ?? ""}`));
    await page.goto(link);
  });
  await expect(dave.locator(".control-prompt .control-error")).toContainText("expired, was used, or never was");
  expect(sent.some((r) => r.includes("/auth/github"))).toBe(true);
  expect(sent.filter((r) => r.includes(seed))).toEqual([]);
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
  // Remove asks first (#101).
  const bobId = await bob.evaluate(() => window.__illogical.control!.account);
  await alice.evaluate(() => dispatchEvent(new CustomEvent("illogical:control-panel", { detail: "teams" })));
  const remove = alice.locator(`[data-remove-member="${bobId}"]`);
  await remove.click();
  await expect(remove).toHaveText("Really remove?");
  const t = Date.now();
  await remove.click();
  await expect.poll(() => bob.evaluate(() => window.__illogical.client.connected), { timeout: 3000, intervals: [50] }).toBe(false);
  expect(Date.now() - t).toBeLessThan(1500);
});

const mineState = temp("mine");
/** The team a daemon pinned, from its saved control state. */
const pinned = (state: string) => {
  try {
    return (JSON.parse(readFileSync(join(state, "control.json"), "utf8")) as { team?: { team: string } }).team?.team ?? null;
  } catch {
    return undefined;
  }
};

test("Cancel turns a join down; Just me keeps a machine apart from the team's", async () => {
  // #100: Cancel tells the daemon, which stops waiting.
  const no = await startJoin("nope", temp("nope"));
  await alice.goto(no.link);
  await expect(alice.locator("[data-join-grants]")).toContainText("Only your devices reach it");
  await alice.locator("[data-cancel-join]").click();
  expect(await no.exited).not.toBe(0);
  expect(no.err()).toContain("turned down on");
  // --team only picks ahead: Alice keeps this one to herself.
  const state = mineState;
  const j = await startJoin("minebox", state, ["--team", team]);
  await alice.goto(j.link);
  await expect(alice.locator("[data-join-to]")).toHaveValue(team);
  await alice.locator("[data-join-to]").selectOption("");
  await expect(alice.locator("[data-join-grants]")).toContainText("Only your devices reach it");
  const answer = await j.confirm(alice);
  await alice.locator("[data-approve-join]").click();
  answer();
  expect(await j.exited).toBe(0);
  expect(j.out()).toContain("This machine is in your account");
  expect(j.out()).toContain("Not the team Acme");
  runDaemon("minebox", state);
  await alice.goto("/");
  await alice.waitForFunction(() => window.__illogical?.control?.phase === "ready");
  await expect.poll(async () => (await hostNames(alice)).sort(), { timeout: 30_000 }).toEqual(["buildbox", "minebox"]);
  // The host menu groups them: hers, and the team's.
  await alice.locator(".host-button").click();
  await expect(alice.locator(".menu-header", { hasText: "Team Acme" })).toBeVisible();
  await expect(alice.locator(".menu-header", { hasText: "Yours" })).toBeVisible();
});

test("Move to… puts a machine in a team and back, signed by the device", async () => {
  // minebox (from the test before) is Alice's own.
  expect(pinned(mineState)).toBeNull();
  const minebox = await alice.evaluate(() => window.__illogical.control!.daemons.find((d) => d.name === "minebox")!.id);
  // Online, so the nudge reaches it now rather than at its next refresh.
  await expect
    .poll(() => alice.evaluate((id) => window.__illogical.control!.daemons.find((d) => d.id === id)?.online, minebox), { timeout: 30_000 })
    .toBe(true);
  await alice.evaluate(() => dispatchEvent(new CustomEvent("illogical:control-panel", { detail: "devices" })));
  const row = alice.locator(`[data-move="${minebox}"]`);
  await row.locator("[data-move-to]").selectOption(team);
  await expect(row.locator("[data-move-explain]")).toContainText("Acme's members reach minebox by their role");
  await row.locator("[data-move-go]").click();
  await expect(row.locator("[data-move-explain]")).toHaveCount(0);
  // Control lists it as the team's, and the daemon pinned the team itself.
  await expect.poll(() => alice.evaluate((id) => window.__illogical.control!.daemons.find((d) => d.id === id)?.team, minebox)).toBe(team);
  await expect.poll(() => pinned(mineState), { timeout: 15_000 }).toBe(team);
  await expect(alice.locator(`[data-device="${minebox}"] [data-team-badge]`)).toHaveText("Acme");

  // Control refuses a move nobody signed, and an old one again.
  const forged = await alice.evaluate(
    (id) =>
      fetch(`/api/daemons/${id}/team`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ team: null, at: Date.now(), by: window.__illogical.control!.keys.id, sig: "00" }),
      }).then((r) => r.status),
    minebox,
  );
  expect(forged).toBe(400);

  // Back to just her: the daemon drops the team.
  await row.locator("[data-move-to]").selectOption("");
  await expect(row.locator("[data-move-explain]")).toContainText("Acme's members lose minebox at once");
  await row.locator("[data-move-go]").click();
  await expect.poll(() => pinned(mineState), { timeout: 15_000 }).toBeNull();
  await expect.poll(() => alice.evaluate((id) => window.__illogical.control!.daemons.find((d) => d.id === id)?.team ?? null, minebox)).toBeNull();
  await alice.getByRole("button", { name: "Done" }).click();
});

test("a session shared with someone outside your teams waits for their yes", async ({ browser }) => {
  test.setTimeout(120_000);
  const erin = await person(browser, "erin");
  const who = await alice.evaluate(() => window.__illogical.control!.person("erin"));
  // minebox is Alice's own again (the test before): she shares a session.
  await alice.evaluate(() => window.__illogical.hosts.select("minebox"));
  await expect
    .poll(() => alice.evaluate(() => window.__illogical.client.connected && !!window.__illogical.client.state?.sessions.length), { timeout: 20_000 })
    .toBe(true);
  const shared = await alice.evaluate(async (c) => {
    const cl = window.__illogical.client;
    const session = cl.state!.sessions[0].id;
    return (await cl.request("POST", "/api/acl", { session, principal: `account:${c.account}`, role: "viewer", root: c.root, name: "erin" })).ok;
  }, who);
  expect(shared).toBe(true);
  // Erin is asked first, by Alice's name; the machine isn't hers to see yet.
  const offers = () => erin.evaluate(async () => (await window.__illogical.control!.refresh(), window.__illogical.control!.offers.length));
  await expect.poll(offers, { timeout: 30_000 }).toBe(1);
  await expect(erin.locator("[data-share-offer-owner]")).toHaveText("alice");
  await expect(erin.locator("[data-share-offer-machine]")).toHaveText("minebox");
  expect(await hostNames(erin)).not.toContain("minebox");
  await erin.locator("[data-share-accept]").click();
  await expect
    .poll(async () => (await erin.evaluate(() => window.__illogical.control!.refresh()), hostNames(erin)), { timeout: 30_000 })
    .toContain("minebox");
});
