// M29: team answers. Jake and Sam are on a team (Val watches it), on
// different networks: here, three browsers and two daemons on loopback,
// every daemon reached through control's relay as on the internet, and
// Sam's "phone" a Pixel-sized touch context (real phones are out of reach).
//
// Claude Code in a terminal on Jake's own machine asks to run cargo test
// (its PermissionRequest and PreToolUse hooks, fed S18's recorded inputs
// through `arugula hook`). Sam, sharing that session as an editor, allows
// it from the phone; Jake's screen says "Allowed by sam". Sam's follow-up
// needs Jake's trust on Jake's machine, then reaches Claude Code through
// its inbox hook (`arugula inbox`), and `arugula log --who` and history
// attribute both to Sam. On the team's box the follow-up goes straight
// through, and Val, a viewer, sees the card but can't answer it (403).
// Answering from a notification: the service worker allows a card over its
// own end-to-end channel.

import { execFileSync, spawn, type ChildProcess } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createServer, type Server } from "node:http";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { devices, expect, test, type Browser, type BrowserContextOptions, type Page } from "@playwright/test";
import { text, closeContexts } from "./helpers";
import { ANY, controlPort, listen } from "./ports";
import { labs } from "./labs";

test.afterAll(closeContexts);

let base = "";
let github = "";
const cli = resolve("../target/debug/arugula");
const fixtures = resolve("../crates/daemon/tests/fixtures");
const procs: ChildProcess[] = [];
const dirs: string[] = [];
let gh: Server;

test.describe.configure({ mode: "serial" });
test.use({ baseURL: async ({}, use) => use(base) });

function temp(what: string) {
  const d = mkdtempSync(join(tmpdir(), `arugula-e2e-answers-${what}-`));
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

async function person(browser: Browser, login: string, opts: BrowserContextOptions = {}): Promise<Page> {
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

/** A daemon joins control (as Jake's, or the team's) and starts. */
async function machine(owner: Page, name: string, team?: string): Promise<string> {
  const state = temp(name);
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
  await owner.goto(link);
  // The machine asks whether the account is the one this browser shows.
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
  return state;
}

/** Show `host` on a page, connected. */
async function show(page: Page, host: string) {
  await page.goto("/");
  await page.waitForFunction(() => window.__arugula?.control?.phase === "ready");
  await expect.poll(() => page.evaluate(() => window.__arugula.hosts.names), { timeout: 30_000 }).toContain(host);
  await page.evaluate((h) => window.__arugula.hosts.select(h), host);
  await expect.poll(() => page.evaluate(() => window.__arugula.client.connected), { timeout: 30_000 }).toBe(true);
}

/** Type a line into a pane, through the API (the shell runs it). */
async function send(page: Page, pane: number, line: string) {
  await page.evaluate(
    async ([pane, line]) => {
      const r = await window.__arugula.client.request("POST", `/api/panes/${pane}/send`, { text: line, enter: true });
      if (!r.ok) throw new Error(`send: ${r.status}`);
    },
    [pane, line] as const,
  );
}

/** Claude Code's hooks for one tool call, then its inbox, as a line for a
 * shell: what Claude Code would run, fed S18's recorded inputs. */
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

/** A pane's text with its wrapping undone (it's narrow on a phone). */
const flat = async (page: Page, pane: number) => (await text(page, pane)).replace(/\n/g, "");

const askOf = (page: Page, pane: number) => page.evaluate((p) => window.__arugula.client.info(p)?.ask ?? null, pane);

let jake: Page;
let sam: Page;
let val: Page;
let team = "";
let mac = "";
let teamboxPane = 0;

test("a permission prompt on Jake's machine is allowed by Sam from the phone, and the follow-up needs Jake's trust", async ({ browser }) => {
  jake = await person(browser, "jake");
  await jake.evaluate(() => window.__arugula.control!.createTeam("Acme"));
  team = await jake.evaluate(() => window.__arugula.control!.teams[0].team);
  const { viewport, userAgent, deviceScaleFactor, isMobile, hasTouch } = devices["Pixel 7"];
  sam = await person(browser, "sam", { viewport, userAgent, deviceScaleFactor, isMobile, hasTouch });
  val = await person(browser, "val");
  for (const [p, role] of [
    [sam, "editor"],
    [val, "viewer"],
  ] as const) {
    const link = await jake.evaluate(([t, r]) => window.__arugula.control!.invite(t, r, true), [team, role] as const);
    await p.goto(link);
    await p.locator("[data-accept-invite]").click();
    await jake.evaluate(() => window.__arugula.control!.refresh());
    await expect(jake.locator("[data-admit-yes]")).toBeVisible({ timeout: 15_000 });
    await jake.locator("[data-admit-yes]").click();
    await expect(jake.locator("[data-admit-yes]")).toHaveCount(0, { timeout: 15_000 });
  }
  mac = await machine(jake, "mac");

  // Jake shares the session on his machine with Sam, who may drive it.
  await show(jake, "mac");
  const pane = await jake.evaluate(() => window.__arugula.client.state!.panes[0].id);
  const session = await jake.evaluate(() => window.__arugula.client.state!.sessions[0].id);
  await jake.evaluate(
    async (s) => {
      const c = window.__arugula.client;
      const sam = await window.__arugula.control!.person("sam");
      const r = await c.request("POST", "/api/acl", { session: s, principal: `account:${sam.account}`, role: "editor", history: true, root: sam.root, name: sam.name });
      if (!r.ok) throw new Error(`share: ${r.status}`);
    },
    session,
  );
  await sam.evaluate(() => window.__arugula.control!.refresh());
  await show(sam, "mac");
  await sam.evaluate((p) => window.__arugula.client.setActive(p), pane);

  // Claude Code in Jake's terminal asks to run cargo test.
  await send(jake, pane, claudeHooks("mac", "cargo test"));
  for (const p of [jake, sam]) await expect.poll(() => askOf(p, pane), { timeout: 15_000 }).toMatchObject({ kind: "permission", tool: "Bash", id: "toolu_mac" });
  // On the phone: the card, and Sam allows it.
  const card = sam.locator(`[data-pane="${pane}"] .ask.perm`);
  await expect(card).toBeVisible();
  await expect(card.locator(".agent-perm-cmd")).toHaveText("cargo test");
  await card.getByRole("button", { name: "Allow", exact: true }).tap();
  // Jake's screen says who allowed it.
  for (const p of [jake, sam]) await expect(p.locator(`[data-pane="${pane}"] .answered-by`)).toHaveText(/^Allowed by sam, \d\d:\d\d/);

  // The follow-up: it runs on Jake's own machine, so Sam asks for trust.
  await expect.poll(() => jake.evaluate((p) => window.__arugula.client.info(p)?.inbox, pane)).toBe(true);
  const box = sam.locator(`[data-pane="${pane}"] .followup input`);
  await box.fill("then open a PR");
  await sam.locator(`[data-pane="${pane}"] .followup button`).tap();
  const ask = sam.getByRole("button", { name: /^Ask .+ for 30 minutes$/ });
  await expect(ask).toBeVisible();
  await ask.tap();
  await expect(jake.locator(`[data-trust-request="${pane}"]`)).toBeVisible();
  await jake.locator("[data-trust]").click();
  // The grant goes over Jake's connection and the follow-up over Sam's:
  // send it once the daemon says Sam is trusted, or it can get there first
  // and be refused again.
  await expect.poll(() => jake.evaluate((p) => window.__arugula.client.info(p)?.trusted?.length ?? 0, pane)).toBe(1);
  await box.fill("then open a PR");
  await sam.locator(`[data-pane="${pane}"] .followup button`).tap();
  await expect(sam.locator(`[data-pane="${pane}"] .followup-sent`)).toHaveText("Sent.");
  // Claude Code got it from its inbox hook (here, printed to the terminal).
  await expect.poll(() => flat(jake, pane)).toContain("A follow-up from sam (sent through Arugula): then open a PR");
  await expect.poll(() => text(jake, pane)).toContain("mac-inbox-42");

  // `arugula log --who` and history attribute both to Sam.
  const sock = join(mac, "sock");
  const who = execFileSync(cli, ["--socket", sock, "log", `%${pane}`, "--who"], { encoding: "utf8" });
  expect(who).toContain("sam");
  const hist = JSON.parse(execFileSync(cli, ["--socket", sock, "--json", "history", "--pane", `%${pane}`], { encoding: "utf8" })) as {
    text: string;
    by?: string;
  }[];
  expect(hist).toEqual(
    expect.arrayContaining([
      expect.objectContaining({ text: "allowed: Bash: cargo test", by: "sam" }),
      expect.objectContaining({ text: "follow-up: then open a PR", by: "sam" }),
    ]),
  );
});

test("on the team's box the follow-up goes straight through, and a viewer can't answer", async () => {
  await machine(jake, "teambox", team);
  for (const p of [jake, sam, val]) {
    await p.evaluate(() => window.__arugula.control!.refresh());
    await show(p, "teambox");
  }
  teamboxPane = await jake.evaluate(() => window.__arugula.client.state!.panes[0].id);
  const pane = teamboxPane;
  await sam.evaluate((p) => window.__arugula.client.setActive(p), pane);
  await send(jake, pane, claudeHooks("box", "cargo build"));
  for (const p of [jake, sam, val]) await expect.poll(() => askOf(p, pane), { timeout: 15_000 }).toMatchObject({ kind: "permission" });

  // Val watches: the card, without buttons; the API says no.
  await expect(val.locator(`[data-pane="${pane}"] .ask.perm`)).toBeVisible();
  await expect(val.locator(`[data-pane="${pane}"] .ask.perm button`)).toHaveCount(0);
  await expect(val.locator(`[data-pane="${pane}"] .ask-viewer`)).toBeVisible();
  const status = await val.evaluate(
    async (p) => (await window.__arugula.client.request("POST", "/api/attention/act", { action: "allow", pane: p })).status,
    pane,
  );
  expect(status).toBe(403);

  await sam.locator(`[data-pane="${pane}"] .ask.perm`).getByRole("button", { name: "Allow", exact: true }).tap();
  await expect(val.locator(`[data-pane="${pane}"] .answered-by`)).toHaveText(/^Allowed by sam/);
  // A viewer gets no follow-up box either.
  await expect(val.locator(`[data-pane="${pane}"] .followup`)).toHaveCount(0);
  await expect.poll(() => jake.evaluate((p) => window.__arugula.client.info(p)?.inbox, pane)).toBe(true);
  await sam.locator(`[data-pane="${pane}"] .followup input`).fill("now run the tests");
  await sam.locator(`[data-pane="${pane}"] .followup button`).tap();
  await expect(sam.locator(`[data-pane="${pane}"] .followup-sent`)).toHaveText("Sent.");
  await expect.poll(() => flat(jake, pane)).toContain("A follow-up from sam (sent through Arugula): now run the tests");
});

test("a notification's Allow answers over the service worker's own channel", async () => {
  const pane = teamboxPane;
  const ctx = sam.context();
  await ctx.grantPermissions(["notifications"]);
  await sam.evaluate(() => navigator.serviceWorker.ready);
  // What the worker needs: the directory it reaches daemons by.
  await sam.evaluate(() => window.__arugula.control!.refresh());
  const daemon = await sam.evaluate(() => window.__arugula.client.e2e!.daemon.id);
  await send(jake, pane, claudeHooks("push", "cargo publish --dry-run"));
  await expect.poll(() => askOf(sam, pane), { timeout: 15_000 }).toMatchObject({ kind: "permission", id: "toolu_push" });
  // Close the app's card view on this page so the answer can only come from
  // the worker: deliver the push the daemon would send, and press Allow.
  // This page's registration: listen before enabling (enable reports the
  // registrations at once), and take the one for control's scope, not the
  // first in a list that can hold others.
  const cdp = await ctx.newCDPSession(sam);
  const registered = new Promise<string>((resolve) => {
    cdp.on("ServiceWorker.workerRegistrationUpdated", (e) => {
      const r = e.registrations.find((x) => !x.isDeleted && x.scopeURL === `${base}/`);
      if (r) resolve(r.registrationId);
    });
  });
  await cdp.send("ServiceWorker.enable");
  const registrationId = await registered;
  const payload = {
    title: "Needs you",
    body: "Bash: cargo publish --dry-run",
    pane,
    tag: `pane-${pane}`,
    daemon,
    approve: { id: "toolu_push", title: "Bash: cargo publish --dry-run" },
    reason: { kind: "ask", actions: ["allow", "deny", "dismiss"] },
  };
  // A notification exists only once its icon has loaded (from control, here),
  // and that takes as long as the machine is busy: wait for the worker's
  // showNotification to finish rather than polling getNotifications against
  // expect's few seconds. (The worker stays up while DevTools is attached.)
  const worker = ctx.serviceWorkers()[0] ?? (await ctx.waitForEvent("serviceworker"));
  await worker.evaluate(() => {
    const g = self as unknown as { registration: ServiceWorkerRegistration; shown?: Promise<string> };
    const r = g.registration;
    const show = r.showNotification.bind(r);
    g.shown = new Promise((resolve) => {
      r.showNotification = (title, options) => show(title, options).then(() => resolve(title));
    });
  });
  await cdp.send("ServiceWorker.deliverPushMessage", { origin: base, registrationId, data: JSON.stringify(payload) });
  expect(await worker.evaluate(() => (self as unknown as { shown?: Promise<string> }).shown)).toBe("Needs you");
  expect(
    await worker.evaluate(async () => (await (self as unknown as { registration: ServiceWorkerRegistration }).registration.getNotifications()).map((n) => n.title)),
  ).toContain("Needs you");
  await worker.evaluate(async () => {
    const g = self as unknown as { registration: ServiceWorkerRegistration; dispatchEvent(e: Event): boolean };
    const [n] = await g.registration.getNotifications({ tag: undefined });
    const Ev = (globalThis as unknown as { NotificationEvent: new (t: string, i: unknown) => Event }).NotificationEvent;
    g.dispatchEvent(new Ev("notificationclick", { notification: n, action: "approve" }));
  });
  await expect(jake.locator(`[data-pane="${pane}"] [data-answered="toolu_push"] .answered-by`)).toHaveText(/^Allowed by sam/, {
    timeout: 15_000,
  });
  await expect.poll(() => askOf(jake, pane)).toBeNull();
});
