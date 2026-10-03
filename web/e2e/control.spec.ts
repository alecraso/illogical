// M17/M18: illogical control in a browser. A stranger with no tailnet signs
// in (a fake GitHub), the browser becomes the account's first device, two
// machines join by code, and both are listed and usable: one directly,
// one only through the relay. A second browser (the phone) can't reach
// anything until the first approves it, and loses access when removed.

import { spawn, type ChildProcess } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { createServer, type Server } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test, type Browser, type Page } from "@playwright/test";
import { ready, run, text } from "./helpers";
import { ANY, controlPort, listen } from "./ports";

let base = "";
const procs: ChildProcess[] = [];
const dirs: string[] = [];
let gh: Server;

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
  const joining = spawn("../target/debug/illogicald", ["join", base, "--name", name, "--state-dir", state], { stdio: ["ignore", "pipe", "ignore"] });
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
  await page.locator("[data-approve-join]").click();
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
});

async function phoneContext(browser: Browser) {
  return browser.newContext({ viewport: { width: 390, height: 760 }, isMobile: true, hasTouch: true });
}

test("a phone needs the laptop's approval", async ({ browser }) => {
  phone = await (await phoneContext(browser)).newPage();
  await signIn(phone);
  await expect(phone.getByText("Approve this browser")).toBeVisible();
  const fp = await phone.locator("[data-fingerprint]").getAttribute("data-fingerprint");
  // The laptop is asked, and shows the same fingerprint.
  await expect(laptop.locator(`[data-pending="${fp}"]`)).toBeVisible({ timeout: 20_000 });
  await laptop.locator("[data-approve]").click();
  await expect.poll(() => hostNames(phone), { timeout: 20_000 }).toEqual(["box", "mac"]);
  await showHost(phone, "mac");
  await shell(phone, "phone");
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
  // The laptop (still enrolled) turns the second browser down.
  await laptop.locator("[data-reject]").click();
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
