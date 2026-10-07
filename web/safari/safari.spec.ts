// Real Safari, through safaridriver (#214 section 5): the device key probe
// (#94) and the one-click presigned invite across GitHub sign-in (#137).
// Playwright's WebKit covers the same ground in e2e/*.webkit.spec.ts; this
// is the browser people use. Control and a fake GitHub run here; the person
// already in the team is Chrome, driven by Playwright.
//
// Needs SAFARIDRIVER_URL (skipped without it). When Safari runs in a VM
// (testnet/macos/test.sh safari), SAFARI_TUNNEL is an ssh command into it:
// each server here is forwarded to the same port on the VM's loopback, so
// Safari's pages are on 127.0.0.1 too, a secure context for WebCrypto.

import { spawn, type ChildProcess } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { createServer, type Server } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test } from "@playwright/test";
import { ANY, controlPort, listen } from "../e2e/ports";
import { driverUrl, Safari } from "./webdriver";

let base = "";
let github = "";
const procs: ChildProcess[] = [];
const dirs: string[] = [];
let gh: Server;
let safari: Safari | undefined;

test.describe.configure({ mode: "serial" });
test.skip(!driverUrl, "SAFARIDRIVER_URL is not set (testnet/macos/test.sh safari sets it)");
test.use({ baseURL: async ({}, use) => use(base) });

/** Forwards these local ports to the same ports on Safari's machine. */
async function tunnel(ports: number[]) {
  const ssh = process.env.SAFARI_TUNNEL;
  if (!ssh) return;
  const fwd = ports.map((p) => `-R 127.0.0.1:${p}:127.0.0.1:${p}`).join(" ");
  const p = spawn("sh", ["-c", `exec ${ssh} -N -o ExitOnForwardFailure=yes ${fwd}`], { stdio: ["ignore", "ignore", "inherit"] });
  procs.push(p);
  // Up once the far end answers on the last port.
  const probe = ports.at(-1)!;
  for (let i = 0; i < 100; i++) {
    if (p.exitCode !== null) throw new Error(`the tunnel to Safari's machine exited (${p.exitCode})`);
    const ok = await new Promise<boolean>((res) => {
      const t = spawn("sh", ["-c", `${ssh} curl -s -o /dev/null http://127.0.0.1:${probe}/`], { stdio: "ignore" });
      t.on("exit", (c) => res(c === 0));
    });
    if (ok) return;
    await new Promise((r) => setTimeout(r, 300));
  }
  throw new Error("the tunnel to Safari's machine never came up");
}

test.beforeAll(async () => {
  // A fake GitHub: whoever the `as` cookie says.
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
    } else res.writeHead(200, { "content-type": "text/plain" }).end("fake GitHub");
  });
  github = `http://127.0.0.1:${await listen(gh)}`;
  const dir = mkdtempSync(join(tmpdir(), "arugula-safari-"));
  dirs.push(dir);
  const db = join(dir, "control.db");
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
    if (await fetch(`${base}/control.json`).then((r) => r.ok, () => false)) break;
    await new Promise((r) => setTimeout(r, 100));
  }
  await tunnel([Number(new URL(github).port), Number(new URL(base).port)]);
  safari = await Safari.start();
  test.info().annotations.push({ type: "safari", description: `Safari ${safari.version()} (${safari.capabilities.platformName})` });
});

test.afterAll(async () => {
  await safari?.quit();
  for (const p of procs) p.kill("SIGKILL");
  gh?.close();
  for (const d of dirs) rmSync(d, { recursive: true, force: true });
});

test("the key probe in real Safari: device keys survive a reload, or the fallback does (#94)", async () => {
  const s = safari!;
  await s.goto(`${base}/key-probe.html`);
  const verdict = await s.until<string>("the probe's verdict", `return document.getElementById("verdict").dataset.verdict;`);
  const result = JSON.parse(await s.run<string>(`return document.getElementById("result").textContent;`));
  test.info().annotations.push({ type: "probe", description: JSON.stringify(result) });
  console.log(`key probe in ${result.browser}: ${verdict}\n${JSON.stringify(result.after)}`);
  expect(result.browser).toMatch(/^Safari [\d.]+ on macOS$/);
  expect(["keys", "wrapped"]).toContain(verdict);
  expect(result.after.wrapped.ok).toBe(true);
});

test("a presigned invite in real Safari: signed out, through GitHub sign-in, joined in one click (#137)", async ({ browser }) => {
  // Alice (Chrome) has a team and makes a presigned link.
  const ctx = await browser.newContext();
  await ctx.addCookies([{ name: "as", value: "alice", url: github }]);
  const alice = await ctx.newPage();
  await alice.goto("/");
  await alice.locator("[data-signin=github]").click();
  await alice.locator("[data-stored-codes]").check();
  await alice.locator("[data-saved-codes]").click();
  await alice.waitForFunction(() => window.__arugula?.control?.phase === "ready");
  await alice.evaluate(() => window.__arugula.control!.createTeam("Acme"));
  const team = await alice.evaluate(() => window.__arugula.control!.teams[0].team);
  const link = await alice.evaluate((t) => window.__arugula.control!.invite(t, "viewer"), team);
  expect(link).toMatch(/#pinvite=[0-9a-f]{16}\.[0-9a-f]{64}$/);

  // Carol opens it in Safari, signed out. GitHub knows her as carol.
  const s = safari!;
  await s.goto(github);
  await s.cookie("as", "carol");
  await s.goto(link);
  expect(await s.shown("[data-why=invite]")).toContain("alice invited you to Acme.");
  // Signing in leaves the page for GitHub and back; the link's key waits
  // in sessionStorage, never in the URL control sees.
  await s.click("[data-signin=github]");
  await s.shown("[data-stored-codes]");
  await s.click("[data-stored-codes]");
  await s.click("[data-saved-codes]");
  expect(await s.shown("[data-invite-team]", 20_000)).toBe("Acme");
  await s.click("[data-accept-invite]");
  await s.shown("[data-invite-joined]", 20_000);
  const mine = await s.run<string[]>(`return window.__arugula.control.teams.map((t) => t.roster.name + ":" + t.role);`);
  expect(mine).toEqual(["Acme:viewer"]);

  // Alice's Chrome checks the version Carol's Safari signed.
  await alice.evaluate(() => window.__arugula.control!.refresh());
  await expect
    .poll(() => alice.evaluate((t) => window.__arugula.control!.teams.find((x) => x.team === t)!.roster.members.map((m) => `${m.name}:${m.role}`), team))
    .toEqual(["alice:owner", "carol:viewer"]);
  await ctx.close();
});
