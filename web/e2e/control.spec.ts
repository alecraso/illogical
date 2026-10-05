// M17/M18: illogical control in a browser. A stranger with no tailnet signs
// in (a fake GitHub), the browser becomes the account's first device, two
// machines join by code, and both are listed and usable: one directly,
// one only through the relay. A second browser (the phone) can't reach
// anything until the first approves it, and loses access when removed.

import { spawn, type ChildProcess } from "node:child_process";
import { createHash, randomBytes } from "node:crypto";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { createServer, type Server } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test, type Browser, type Page } from "@playwright/test";
import { closeContexts, controlPanel, pasteFile, ready, run, text, uploadedPath } from "./helpers";
import { ANY, controlPort, daemonPort, listen } from "./ports";

test.afterAll(closeContexts);

let base = "";
const procs: ChildProcess[] = [];
const dirs: string[] = [];
let gh: Server;
/** Each machine's state dir, by name. */
const states: Record<string, string> = {};

test.describe.configure({ mode: "serial" });
test.use({ baseURL: async ({}, use) => use(base) });

function temp(what: string) {
  const d = mkdtempSync(join(tmpdir(), `illogical-e2e-control-${what}-`));
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
      res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify({ id: 7, login: "stranger" }));
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

/** `illogicald join`, approved from `page`; then the daemon runs. */
async function addMachine(page: Page, name: string, direct: boolean) {
  const state = temp(name);
  states[name] = state;
  const joining = spawn("../target/debug/illogicald", ["join", base, "--name", name, "--state-dir", state], { stdio: ["pipe", "pipe", "ignore"] });
  procs.push(joining);
  const link = await new Promise<string>((res) => {
    let out = "";
    joining.stdout!.on("data", (d) => {
      out += d;
      const m = out.match(/(http\S+#join=[A-Z0-9-]+)/);
      if (m) res(m[1]);
    });
  });
  const code = link.split("#join=")[1];
  const exited = new Promise<number | null>((r) => joining.on("exit", r));
  await page.goto(link);
  await expect(page.locator("[data-join-code]")).toHaveText(code);
  // The machine asks whether the account is the one this browser shows.
  const account = await page.locator("[data-join-account]").getAttribute("data-join-account");
  await page.locator("[data-approve-join]").click();
  joining.stdin!.end(`${account}\n`);
  expect(await exited).toBe(0);
  procs.push(
    spawn(
      "../target/debug/illogicald",
      [
        ...["--listen", ANY, "--name", name, "--state-dir", state],
        ...["--shell", "bash --norc --noprofile", "--no-manager-env", "--tailscale-socket", "/nonexistent/sock"],
        ...(direct ? ["--direct-url", "http://127.0.0.1:0"] : []),
      ],
      { stdio: "ignore" },
    ),
  );
}

/** The page has booted and is signed in and enrolled. */
async function booted(page: Page) {
  await page.waitForFunction(() => window.__illogical?.control?.phase === "ready", null, { timeout: 20_000 });
}

const hostNames = async (page: Page) => (await booted(page), page.evaluate(() => window.__illogical.hosts.names));
const connected = (page: Page) => page.evaluate(() => window.__illogical.client.connected);

async function showHost(page: Page, name: string) {
  await page.evaluate((n) => window.__illogical.hosts.select(n), name);
  await expect.poll(() => connected(page), { timeout: 20_000 }).toBe(true);
}

async function shell(page: Page, marker: string) {
  await expect.poll(() => page.evaluate(() => window.__illogical.client.state?.panes.length ?? 0)).toBeGreaterThan(0);
  const pane = await page.evaluate(() => window.__illogical.client.active()!);
  await ready(page, pane);
  await run(page, pane, `echo ${marker}-$((6*7))`, `${marker}-42`);
  expect(await text(page, pane)).toContain(`${marker}-42`);
}

let laptop: Page;
let phone: Page;

let recoveryCodes: string[] = [];

test("a stranger signs up and becomes the first device", async ({ browser }) => {
  laptop = await (await browser.newContext({ permissions: ["clipboard-read", "clipboard-write"] })).newPage();
  await signIn(laptop);
  // Recovery codes, once, each with Copy, and both to copy or download (#99).
  await expect(laptop.locator("[data-recovery-code]")).toHaveCount(2);
  recoveryCodes = await laptop.locator("[data-recovery-code]").allTextContents();
  const clipboard = () => laptop.evaluate(() => navigator.clipboard.readText());
  // (The Add a machine screen waits under the codes, with its own Copy.)
  await laptop.locator(".prompt .copy-text [data-copy]").first().click();
  await expect(laptop.locator(".prompt .copy-text [data-copy]").first()).toHaveText("Copied");
  expect(await clipboard()).toBe(recoveryCodes[0]);
  await laptop.getByRole("button", { name: "Copy both" }).click();
  expect(await clipboard()).toBe(recoveryCodes.join("\n") + "\n");
  const saved = laptop.waitForEvent("download");
  await laptop.locator("[data-download-codes]").click();
  expect((await saved).suggestedFilename()).toBe("illogical-recovery-codes.txt");
  // Continue only once they're stored (#106).
  await expect(laptop.locator("[data-saved-codes]")).toBeDisabled();
  await laptop.locator("[data-stored-codes]").check();
  await laptop.locator("[data-saved-codes]").click();
  // No machine yet (#98): install, join (by full path) and approve, as
  // numbered steps, each command with Copy.
  await expect(laptop.getByRole("heading", { name: "Add a machine" })).toBeVisible();
  await expect(laptop.locator(".control-steps > li")).toHaveCount(3);
  await expect(laptop.locator("[data-install]")).toHaveText("curl -fsSL https://illogical.widgets.wtf/install.sh | sh");
  const join = `~/.local/bin/illogicald join ${base}`;
  await expect(laptop.locator("[data-join-cmd]")).toHaveText(join);
  await expect(laptop.locator(".control-steps")).toContainText("Codes last 15 minutes.");
  await expect(laptop.locator(".control-add")).toContainText("a device (this browser, your phone) reaches them");
  const copyJoin = laptop.locator("[data-join-cmd] + [data-copy]");
  await copyJoin.click();
  expect(await clipboard()).toBe(join);
  // With the clipboard blocked, Copy selects the command instead.
  await laptop.evaluate(() => {
    navigator.clipboard.writeText = () => Promise.reject(new DOMException("blocked", "NotAllowedError"));
    document.execCommand = () => false;
  });
  await copyJoin.click();
  await expect(copyJoin).toHaveText("Selected");
  expect(await laptop.evaluate(() => getSelection()!.toString())).toBe(join);
});

test("with no machine yet, the account's menu is there (#97)", async () => {
  await expect(laptop.locator("[data-signed-in-as]")).toHaveText("stranger");
  // Devices: this browser, and a passkey could be added.
  await laptop.getByRole("button", { name: "Devices and machines…" }).click();
  await expect(laptop.getByRole("heading", { name: "Devices and machines" })).toBeVisible();
  await expect(laptop.locator(".control-devices li")).toHaveCount(1);
  await laptop.locator(".prompt").getByRole("button", { name: "Done" }).click();
  // A team.
  await laptop.getByRole("button", { name: "Teams…" }).click();
  await laptop.getByLabel("Team name").fill("Solo");
  await laptop.getByRole("button", { name: "Make a team" }).click();
  await expect(laptop.locator("[data-team] h3")).toHaveText("Solo");
  await laptop.locator(".prompt").getByRole("button", { name: "Done" }).click();
  // The plan, where billing is on (faked here: no Stripe in tests).
  await laptop.route("**/api/billing", (r) =>
    r.fulfill({
      json: { billing: true, plan: "personal", relay: { bytes: 0, allowance: 1e9, warning: false, slowed: false }, sandbox_minutes: 0, teams: [] },
    }),
  );
  await laptop.evaluate(() => window.__illogical.control!.refresh());
  await laptop.getByRole("button", { name: "Plan and usage…" }).click();
  await expect(laptop.getByRole("heading", { name: "Plan and usage" })).toBeVisible();
  await laptop.locator(".prompt").getByRole("button", { name: "Done" }).click();
  await laptop.unroute("**/api/billing");
  // Owning one, step 2 mentions --team.
  await expect(laptop.locator(".control-steps")).toContainText("--team");
  // Signing out, and the sign-in page says why a #join= link came here
  // (#103); signing back in keeps this browser's place.
  await laptop.locator("[data-account-bar]").getByRole("button", { name: "Sign out" }).click();
  await expect(laptop.locator("[data-signin=github]")).toBeVisible();
  // Signing out goes to "/" itself: let that load land first, or it can
  // overtake the link's.
  await laptop.goto("/");
  await laptop.goto("/#join=ABCDE-FGHIJ");
  await expect(laptop.locator("[data-why=join]")).toHaveText("Sign in to approve this machine.");
  await laptop.goto("/");
  await signIn(laptop);
  await booted(laptop);
  await expect(laptop.locator("[data-signed-in-as]")).toHaveText("stranger");
});

test("two machines join by code; one direct, one only through the relay", async () => {
  await addMachine(laptop, "box", true);
  await addMachine(laptop, "mac", false);
  await laptop.goto("/");
  await expect.poll(() => hostNames(laptop), { timeout: 20_000 }).toEqual(["box", "mac"]);
  await expect
    .poll(() => laptop.evaluate(() => window.__illogical.control!.daemons.every((d) => d.online)), { timeout: 20_000 })
    .toBe(true);

  await showHost(laptop, "box");
  await shell(laptop, "box");
  expect(await laptop.evaluate(() => window.__illogical.client.path)).toBe("direct");

  await expect(laptop.locator(".host-button [data-path]")).toHaveText("direct");

  await showHost(laptop, "mac");
  await shell(laptop, "mac");
  expect(await laptop.evaluate(() => window.__illogical.client.path)).toBe("relayed");
  await expect(laptop.locator(".host-button [data-path]")).toHaveText("relayed");

  // M70: a pasted image goes through the relay in chunks (2.5 MB: three),
  // inside the end-to-end channel, and lands on the machine whole.
  const pane = await laptop.evaluate(() => window.__illogical.client.active()!);
  const png = Buffer.from(Array.from({ length: 2_500_000 }, (_, i) => (i * 7) % 256));
  await pasteFile(laptop, pane, png, "relayed.png", "image/png");
  expect(readFileSync(await uploadedPath(laptop, pane))).toEqual(png);
  // The path waits on the prompt line: clear it for what's typed next.
  await laptop.evaluate((p) => window.__illogical.client.input(p, new TextEncoder().encode("\x15")), pane);
});

async function phoneContext(browser: Browser) {
  return browser.newContext({ viewport: { width: 390, height: 760 }, isMobile: true, hasTouch: true });
}

test("a phone needs the laptop's approval", async ({ browser }) => {
  phone = await (await phoneContext(browser)).newPage();
  await signIn(phone);
  await expect(phone.getByText("Approve this browser")).toBeVisible();
  // It says where to approve it (#105).
  await expect(phone.locator("[data-control-url]")).toHaveText(base);
  await expect(phone.locator("[data-sign-out]")).toBeVisible();
  const fp = await phone.locator("[data-fingerprint]").getAttribute("data-fingerprint");
  // The laptop is asked, and shows the same fingerprint.
  await expect(laptop.locator(`[data-pending="${fp}"]`)).toBeVisible({ timeout: 20_000 });
  await laptop.locator("[data-approve]").click();
  await expect.poll(() => hostNames(phone), { timeout: 20_000 }).toEqual(["box", "mac"]);
  await showHost(phone, "mac");
  await shell(phone, "phone");
});

test("the desktop app signs in through the browser, then is approved as a device (M48)", async ({ browser }) => {
  // The app keeps a verifier, listens on a loopback port for its grant,
  // asks for a ticket, and opens its page in the person's browser.
  const verifier = randomBytes(32).toString("hex");
  const challenge = createHash("sha256").update(verifier).digest("hex");
  let grant = "";
  const loop = createServer((req, res) => {
    const u = new URL(req.url!, "http://127.0.0.1");
    if (u.pathname === "/illogical-signin") grant = u.searchParams.get("grant") ?? "";
    res.writeHead(303, { location: `${base}/#app-done` }).end();
  });
  await new Promise<void>((r) => loop.listen(0, "127.0.0.1", r));
  const port = (loop.address() as { port: number }).port;
  // An app from before the loopback hand-over is told to update.
  const old = await fetch(`${base}/auth/app`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ name: "illogical app on test-mac" }),
  });
  expect(old.status).toBe(400);
  expect(((await old.json()) as { error: string }).error).toContain("update it");
  const ask = await fetch(`${base}/auth/app`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ name: "illogical app on test-mac", challenge, port }),
  });
  expect(ask.status).toBe(200);
  const t = (await ask.json()) as { ticket: string; code: string; url: string };
  expect(t.url).toBe(`${base}/#app=${t.ticket}`);
  const redeem = (body: unknown, origin = base) =>
    fetch(`${base}/auth/app/${t.ticket}/redeem`, { method: "POST", headers: { "content-type": "application/json", origin }, body: JSON.stringify(body) });
  // A redeem before the person allows it gets nothing; nor does a GET.
  expect((await redeem({ grant: "00", verifier })).status).toBe(404);
  expect((await fetch(`${base}/auth/app/${t.ticket}/redeem?grant=00&verifier=${verifier}`, { redirect: "manual" })).status).toBe(405);

  // In the browser (signed in), the same code, and Allow: the grant goes
  // to the app's port, and the browser comes back to control's page.
  await laptop.goto(t.url);
  await expect(laptop.locator("[data-app-login-name]")).toHaveText("illogical app on test-mac");
  await expect(laptop.locator("[data-app-login-code]")).toHaveText(t.code);
  await expect(laptop.locator("[data-app-login-elsewhere]")).toHaveCount(0);
  await laptop.locator("[data-app-login-allow]").click();
  await expect(laptop.locator("[data-app-login-done]")).toBeVisible();
  expect(grant).toMatch(/^[0-9a-f]{64}$/);
  loop.close();
  // The grant alone (someone who saw the link) or from another site does nothing.
  expect((await redeem({ grant, verifier: "0".repeat(64) })).status).toBe(404);
  expect((await redeem({ grant, verifier }, "https://elsewhere.example")).status).toBe(403);
  // In a browser rather than the app's window, #app-redeem doesn't sign in.
  const other = await (await browser.newContext()).newPage();
  await other.goto(`${base}/#app-redeem=${t.ticket}.${grant}.${verifier}`);
  await expect(other.locator("[data-signin]").first()).toBeVisible();
  await other.context().close();

  // The app's window redeems it: signed in, then a new device to approve.
  const app = await (await browser.newContext()).newPage();
  await app.addInitScript(() => Object.assign(window, { __illogicalApp: { name: "illogical app on test-mac" } }));
  await app.goto(`${base}/#app-redeem=${t.ticket}.${grant}.${verifier}`);
  await expect(app.getByText("Approve this browser")).toBeVisible();
  const fp = await app.locator("[data-fingerprint]").getAttribute("data-fingerprint");
  await expect(laptop.locator(`[data-pending="${fp}"]`)).toBeVisible({ timeout: 20_000 });
  await expect(laptop.locator(".prompt")).toContainText("illogical app on test-mac");
  await laptop.locator("[data-approve]").click();
  await expect.poll(() => hostNames(app), { timeout: 20_000 }).toEqual(["box", "mac"]);
  await showHost(app, "mac");
  await shell(app, "app");

  // Single use.
  expect((await redeem({ grant, verifier })).status).toBe(404);

  // Removing the app's device in control cuts it off at once.
  const id = await app.evaluate(() => window.__illogical.control!.keys.id);
  await laptop.evaluate((d) => window.__illogical.control!.revoke(d), id);
  await expect.poll(() => connected(app), { timeout: 5_000, intervals: [200] }).toBe(false);
  await app.context().close();
});

test("with every device lost, a recovery code lets a new browser in, once", async ({ browser }) => {
  const fresh = await (await browser.newContext()).newPage();
  await signIn(fresh);
  await expect(fresh.getByText("Approve this browser")).toBeVisible();
  await fresh.locator("[data-use-recovery]").click();
  await fresh.getByLabel("Recovery code").fill(recoveryCodes[0]);
  await fresh.getByRole("button", { name: "Use it" }).click();
  await expect.poll(() => hostNames(fresh), { timeout: 20_000 }).toEqual(["box", "mac"]);
  // The code is spent: another browser can't use it again.
  const again = await (await browser.newContext()).newPage();
  await signIn(again);
  await again.locator("[data-use-recovery]").click();
  await again.getByLabel("Recovery code").fill(recoveryCodes[0]);
  await again.getByRole("button", { name: "Use it" }).click();
  await expect(again.locator(".control-error")).toContainText("isn't one of this account's recovery codes");
  // The laptop (still enrolled) turns the second browser down, and the
  // browser hears which device did (#105). It can ask again.
  const laptopName = await laptop.evaluate(() => window.__illogical.control!.enrollment!.cert.name);
  await laptop.locator("[data-reject]").click();
  await expect(again.locator("[data-turned-down]")).toContainText(`${laptopName} turned this browser down`, { timeout: 10_000 });
  await again.locator("[data-try-again]").click();
  await expect(again.getByText("Approve this browser")).toBeVisible();
  await expect(laptop.locator("[data-pending]")).toBeVisible({ timeout: 20_000 });
  await laptop.locator("[data-reject]").click();
  await expect(again.locator("[data-turned-down]")).toBeVisible({ timeout: 10_000 });
});

test("add a phone or browser: the control URL as a QR code and a link", async () => {
  await controlPanel(laptop, "add-device");
  await expect(laptop.getByRole("heading", { name: "Add a phone or browser" })).toBeVisible();
  await expect(laptop.locator("svg[data-qr]")).toHaveAttribute("data-qr", base);
  expect((await laptop.locator("svg[data-qr] path").getAttribute("d"))!.length).toBeGreaterThan(100);
  await expect(laptop.locator(".prompt [data-control-url]")).toHaveText(base);
  await expect(laptop.locator(".control-steps li")).toHaveCount(3);
  await laptop.getByRole("button", { name: "Done" }).click();
});

test("devices and machines, grouped; new recovery codes retire the old", async ({ browser }) => {
  await controlPanel(laptop, "devices");
  await expect(laptop.locator("[data-account]")).toHaveText("stranger");
  const machines = laptop.locator("[data-machines] li");
  await expect(machines).toHaveCount(2);
  await expect(machines.filter({ hasText: "box" }).locator("[data-status]")).toContainText("online · direct");
  await expect(machines.filter({ hasText: "mac" }).locator("[data-status]")).toContainText("online · relayed");
  // Removing a machine says what happens to it (not confirmed here).
  await machines.filter({ hasText: "mac" }).locator("[data-remove]").click();
  await expect(laptop.locator("[data-remove-explain]")).toContainText("keeps running on it, reachable only locally");
  // The laptop, the phone and the browser the recovery code let in.
  await expect(laptop.locator("[data-browsers] li")).toHaveCount(3);
  await expect(laptop.locator("[data-browsers] li").filter({ hasText: "(this browser)" })).toHaveCount(1);
  // One code was spent.
  await expect(laptop.locator("[data-recovery-left]")).toHaveAttribute("data-recovery-left", "1");
  await laptop.locator("[data-new-codes]").click();
  await laptop.locator("[data-new-codes]").click();
  await expect(laptop.locator("[data-recovery-code]")).toHaveCount(2);
  const fresh = await laptop.locator("[data-recovery-code]").allTextContents();
  expect(fresh).not.toContain(recoveryCodes[1]);
  await laptop.locator("[data-stored-codes]").check();
  await laptop.locator("[data-saved-codes]").click();
  await expect(laptop.locator("[data-recovery-left]")).toHaveAttribute("data-recovery-left", "2");
  await laptop.getByRole("button", { name: "Done" }).click();

  // The old code (never used) no longer works; a new one does.
  const other = await (await browser.newContext()).newPage();
  await signIn(other);
  await other.locator("[data-use-recovery]").click();
  await other.getByLabel("Recovery code").fill(recoveryCodes[1]);
  await other.getByRole("button", { name: "Use it" }).click();
  await expect(other.locator(".control-error")).toContainText("isn't one of this account's recovery codes");
  await other.getByLabel("Recovery code").fill(fresh[0]);
  await other.getByRole("button", { name: "Use it" }).click();
  await expect.poll(() => hostNames(other), { timeout: 20_000 }).toEqual(["box", "mac"]);
});

test("removing the phone cuts it off", async () => {
  const id = await phone.evaluate(() => window.__illogical.control!.keys.id);
  await laptop.evaluate((d) => window.__illogical.control!.revoke(d), id);
  // Control nudges the daemons, which refresh, close its channel and
  // refuse it from then on.
  await expect.poll(() => connected(phone), { timeout: 5_000, intervals: [200] }).toBe(false);
  await new Promise((r) => setTimeout(r, 3000));
  expect(await connected(phone)).toBe(false);
});

test("Getting started asks to check the account's fingerprint before the machine trusts it", async ({ browser }) => {
  const state = temp("starter");
  procs.push(
    spawn(
      "../target/debug/illogicald",
      [
        ...["--listen", ANY, "--name", "starter", "--state-dir", state, "--control", base],
        ...["--shell", "bash --norc --noprofile", "--no-manager-env", "--tailscale-socket", "/nonexistent/sock"],
      ],
      { stdio: "ignore" },
    ),
  );
  const local = `http://127.0.0.1:${await daemonPort(state, procs.at(-1))}`;
  const page = await (await browser.newContext()).newPage();
  await page.goto(local);
  await expect(page.locator(".session-button")).toBeVisible();
  const start = page.getByRole("dialog", { name: "Getting started" });
  const cloud = async () => {
    if (!(await start.isVisible())) {
      await page.locator(".session-button").click();
      await page.getByRole("menuitem", { name: "Getting started" }).click();
    }
    await start.locator('[data-start-seg="cloud"]').click();
  };
  // The daemon's --control (#207): the button asks this test's control
  // for a code. Approves it on the laptop from the link the panel shows,
  // and returns the account the laptop showed.
  expect((await (await page.request.get(`${local}/api/setup?part=control`)).json()).control.url).toBe(base);
  const approve = async () => {
    await cloud();
    const connect = start.locator("[data-start-connect]");
    await expect(connect).toHaveText(`Connect to ${new URL(base).host}`);
    await connect.click();
    await expect(start.locator("[data-start-approve]")).toBeVisible({ timeout: 20_000 });
    const link = (await start.locator("[data-start-approve]").getAttribute("href"))!;
    expect(link.startsWith(`${base}/#join=`)).toBe(true);
    await laptop.goto(link);
    const account = (await laptop.locator("[data-join-account]").getAttribute("data-join-account"))!;
    await laptop.locator("[data-approve-join]").click();
    return account;
  };
  const shown = () => start.locator("[data-start-account]").getAttribute("data-start-account").then((a) => a?.replaceAll("-", ""));

  // Approved, but not saved until the person says the account is theirs.
  // "They don't": nothing is pinned.
  const account = await approve();
  await expect.poll(shown, { timeout: 20_000 }).toBe(account);
  await expect(start.locator("[data-start-confirm]")).toContainText("Is this your account?");
  await start.locator("[data-start-different]").click();
  await expect(start.locator("[data-start-error]")).toContainText("Not joined");
  expect((await (await page.request.get(`${local}/api/setup?part=control`)).json()).control.joined).toBeUndefined();

  // Again (as a new key: the turned-down one is dropped), and "They
  // match": joined.
  expect(await approve()).toBe(account);
  await expect.poll(shown, { timeout: 20_000 }).toBe(account);
  await start.locator("[data-start-same]").click();
  await expect(start.locator("[data-start-joined]")).toHaveText("Joined to your account", { timeout: 20_000 });
  await page.context().close();
  // The laptop sees it in the account.
  await laptop.goto("/");
  await expect.poll(() => hostNames(laptop), { timeout: 20_000 }).toContain("starter");
  // The page may switch machine as the new one arrives, mounting the
  // overlay afresh: ask for the panel until it's there.
  await expect(async () => {
    await controlPanel(laptop, "devices");
    await expect(laptop.locator("[data-account-fingerprint]")).toHaveAttribute("data-account-fingerprint", account, { timeout: 1000 });
  }).toPass({ timeout: 15_000 });
  await laptop.getByRole("button", { name: "Done" }).click();
});

test("sessions: where you're signed in, and signing out everywhere (#173)", async () => {
  await controlPanel(laptop, "account");
  await expect(laptop.getByRole("heading", { name: "Sign-in and account" })).toBeVisible();
  // This browser, the phone, and the others that signed in above.
  await expect(laptop.locator("[data-session]")).not.toHaveCount(0);
  expect(await laptop.locator("[data-session]").count()).toBeGreaterThanOrEqual(2);
  // The system Chrome's own platform: where the tests run.
  await expect(laptop.locator("[data-sessions]")).toContainText(`Chrome on ${process.platform === "darwin" ? "macOS" : "Linux"}`);
  await expect(laptop.locator("[data-sessions]")).toContainText("(this one)");
  // Sign one other out: it's gone from the list.
  const n = await laptop.locator("[data-session]").count();
  const other = laptop.locator("[data-end-session]").first();
  await other.click();
  await other.click();
  await expect(laptop.locator("[data-session]")).toHaveCount(n - 1);
  // Everywhere: this one too.
  await laptop.locator("[data-end-all]").click();
  await laptop.locator("[data-end-all]").click();
  await expect(laptop.locator("[data-signin=github]")).toBeVisible();
  expect((await phone.request.get(`${base}/api/me`)).status()).toBe(401);
});

test("deleting the account: type its login; its machines and team go (#173)", async () => {
  await signIn(laptop);
  await booted(laptop);
  const before = await laptop.evaluate(() => window.__illogical.control!.account);
  await controlPanel(laptop, "account");
  await laptop.locator("[data-delete-account]").click();
  await expect(laptop.getByRole("heading", { name: "Delete your account" })).toBeVisible();
  await expect(laptop.locator("[data-disband]")).toContainText("Solo");
  await expect(laptop.locator("[data-delete-what]")).toContainText(/your \d+ machines/);
  const go = laptop.locator("[data-delete-go]");
  await expect(go).toBeDisabled();
  await laptop.locator("[data-delete-confirm]").fill("someone");
  await expect(go).toBeDisabled();
  await laptop.locator("[data-delete-confirm]").fill("stranger");
  await go.click();
  await expect(laptop.locator("[data-signin=github]")).toBeVisible();
  // Its machine, asked to join again, says the account is gone rather than
  // that it's still in it (#208).
  const rejoin = spawn("../target/debug/illogicald", ["join", base, "--name", "box", "--state-dir", states.box], { stdio: ["ignore", "ignore", "pipe"] });
  let said = "";
  rejoin.stderr!.on("data", (d) => (said += d));
  expect(await new Promise((r) => rejoin.on("exit", r))).not.toBe(0);
  expect(said).toContain("control doesn't know it any more (this machine's account was deleted); run `illogicald leave`");
  // The same GitHub account signing in again starts afresh: a new
  // account, a new first device, no machines.
  await signIn(laptop);
  await expect(laptop.locator("[data-recovery-code]")).toHaveCount(2);
  const after = await laptop.evaluate(() => window.__illogical.control!.account);
  expect(after).not.toBe(before);
  const dir = await laptop.request.get(`${base}/api/directory`);
  expect((await dir.json()).daemons).toEqual([]);
});
