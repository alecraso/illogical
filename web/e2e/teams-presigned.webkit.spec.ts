// #137: the one-click presigned invite in WebKit (Safari's engine). The
// owner signs the invite with a device key read back after a reload (#94);
// the invitee opens the link signed out, signs in with GitHub, and the
// one-time key waits in sessionStorage across the OAuth redirect, never
// sent to control. Back on the page, one click and she's in.

import { spawn, type ChildProcess } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { createServer, type Server } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test, type Browser, type Page } from "@playwright/test";
import { controlPanel } from "./helpers";
import { ANY, controlPort, listen } from "./ports";

let base = "";
let github = "";
const procs: ChildProcess[] = [];
const dirs: string[] = [];
let gh: Server;

test.describe.configure({ mode: "serial" });
// Playwright's WebKit stops loading the page once the service worker is
// registered (real Safari doesn't), so none here.
test.use({ baseURL: async ({}, use) => use(base), serviceWorkers: "block" });

test.beforeAll(async () => {
  // A fake GitHub: whoever the `as` cookie names.
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
  const dir = mkdtempSync(join(tmpdir(), "illogical-e2e-presigned-"));
  dirs.push(dir);
  const db = join(dir, "control.db");
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
});

test.afterAll(() => {
  for (const p of procs) p.kill("SIGKILL");
  gh?.close();
  for (const d of dirs) rmSync(d, { recursive: true, force: true });
});

const phase = (page: Page) => page.evaluate(() => window.__illogical?.control?.phase).catch(() => undefined);

/** A WebKit browser signed in with GitHub as `login`, starting wherever
 * `before` leaves it (the app's page by default). */
async function person(browser: Browser, login: string, before?: (page: Page) => Promise<void>): Promise<Page> {
  const ctx = await browser.newContext({ baseURL: base, serviceWorkers: "block" });
  await ctx.addCookies([{ name: "as", value: login, url: github }]);
  const page = await ctx.newPage();
  if (before) await before(page);
  else await page.goto("/");
  await page.locator("[data-signin=github]").click();
  await page.locator("[data-stored-codes]").check();
  await page.locator("[data-saved-codes]").click();
  await expect.poll(() => phase(page), { timeout: 20_000 }).toBe("ready");
  return page;
}

test("a presigned invite survives GitHub sign-in in WebKit, and joins in one click", async ({ browser }) => {
  const alice = await person(browser, "alice");
  await alice.evaluate(() => window.__illogical.control!.createTeam("Acme"));
  const team = await alice.evaluate(() => window.__illogical.control!.teams[0].team);
  // The invite is signed with the device key as WebKit reads it back.
  await alice.reload();
  await expect.poll(() => phase(alice), { timeout: 20_000 }).toBe("ready");
  const section = alice.locator(`[data-team="${team}"]`);
  await controlPanel(alice, "teams");
  await expect(section).toBeVisible();
  await section.locator(`[data-invite-role="${team}"]`).selectOption("viewer");
  await expect(section.locator("[data-invite-ask-first]")).not.toBeChecked();
  await alice.locator(`[data-invite="${team}"]`).click();
  await expect(section).toContainText("One person can join with this link, within a day");
  const link = (await section.locator("[data-invite-link]").textContent())!;
  expect(link).toMatch(/#pinvite=[0-9a-f]{16}\.[0-9a-f]{64}$/);
  const seed = link.split(".").at(-1)!;
  await alice.getByRole("button", { name: "Done" }).click();

  // Carol has no account. She opens the link, is told who invited her,
  // and signs in with GitHub: a redirect away and back.
  const sent: string[] = [];
  const carol = await person(browser, "carol", async (page) => {
    page.on("request", (r) => sent.push(`${r.url()} ${r.postData() ?? ""}`));
    await page.goto(link);
    await expect(page.locator("[data-why=invite]")).toContainText("alice invited you to Acme.");
  });
  expect(sent.some((r) => r.includes("/auth/github"))).toBe(true);
  expect(sent.some((r) => r.startsWith(github))).toBe(true);
  // Back from GitHub, the invite is on screen: one button.
  await expect(carol.locator("[data-invite-team]")).toHaveText("Acme");
  await expect(carol.locator(".control-prompt")).toContainText("Joining adds you right away");
  expect(await carol.evaluate(() => sessionStorage.getItem("illogical:presigned-invite"))).toBeNull();
  await carol.locator("[data-accept-invite]").click();
  await expect(carol.locator("[data-invite-joined]")).toBeVisible({ timeout: 15_000 });
  expect(await carol.evaluate(() => window.__illogical.control!.teams.map((t) => `${t.roster.name}:${t.role}`))).toEqual(["Acme:viewer"]);
  // The seed stayed in the browser: no request carried it.
  expect(sent.filter((r) => r.includes(seed))).toEqual([]);

  // Alice's WebKit checks the version Carol's wrote.
  await alice.evaluate(() => window.__illogical.control!.refresh());
  await expect
    .poll(() => alice.evaluate(() => window.__illogical.control!.teams.find((t) => t.roster.name === "Acme")!.roster.members.map((m) => `${m.name}:${m.role}`)))
    .toEqual(["alice:owner", "carol:viewer"]);
});
