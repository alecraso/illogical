// A local illogical control with a fake GitHub sign-in, people signed in on
// it, and machines joined to it, as team-swarm.spec.ts sets them up (M30).
// For the phone specs: a person can be any browser context (a Pixel 7 in
// Chrome, an iPhone in WebKit).

import { spawn, type ChildProcess } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { createServer, type Server } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, type Browser, type BrowserContextOptions, type Page } from "@playwright/test";
import { ANY, controlPort, listen } from "./ports";

export class TeamControl {
  base = "";
  github = "";
  private procs: ChildProcess[] = [];
  private dirs: string[] = [];
  private gh?: Server;

  constructor(private what: string) {}

  temp(what: string) {
    const d = mkdtempSync(join(tmpdir(), `illogical-e2e-${this.what}-${what}-`));
    this.dirs.push(d);
    return d;
  }

  /** `extra`: more arguments for control. */
  async start(extra: string[] = []) {
    this.gh = createServer((req, res) => {
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
    this.github = `http://127.0.0.1:${await listen(this.gh)}`;
    const db = join(this.temp("db"), "control.db");
    this.procs.push(
      spawn(
        "../target/debug/illogical-control",
        [
          ...["--listen", ANY, "--public-url", "http://127.0.0.1:0", "--db", db],
          ...["--github-client-id", "id", "--github-client-secret", "s", "--static-dir", "dist"],
          ...["--github-url", this.github, "--github-api", this.github],
          ...extra,
        ],
        { stdio: "ignore" },
      ),
    );
    this.base = `http://127.0.0.1:${await controlPort(db, this.procs.at(-1))}`;
    for (let i = 0; i < 100; i++) {
      try {
        if ((await fetch(`${this.base}/control.json`)).ok) return;
      } catch {
        // not yet
      }
      await new Promise((r) => setTimeout(r, 100));
    }
  }

  stop() {
    for (const p of this.procs) p.kill("SIGKILL");
    this.gh?.closeAllConnections();
    this.gh?.close();
    for (const d of this.dirs) rmSync(d, { recursive: true, force: true });
  }

  /** `login` signed in on control (stored codes, no passkey), in a new
   * context of `browser` with `options` (a phone's, say). */
  async person(browser: Browser, login: string, options: BrowserContextOptions = {}): Promise<Page> {
    const ctx = await browser.newContext({ ...options, baseURL: this.base });
    await ctx.addCookies([{ name: "as", value: login, url: this.github }]);
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
  async addMachine(page: Page, name: string, team?: string) {
    const state = this.temp(name);
    const joining = spawn("../target/debug/illogicald", ["join", this.base, "--name", name, "--state-dir", state, ...(team ? ["--team", team] : [])], {
      stdio: ["pipe", "pipe", "ignore"],
    });
    this.procs.push(joining);
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
    const account = await page.locator("[data-join-account]").getAttribute("data-join-account");
    await page.locator("[data-approve-join]").click();
    joining.stdin!.end(`${account}\n`);
    expect(await exited).toBe(0);
    this.procs.push(
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
}

export async function home(page: Page) {
  await page.goto("/");
  await page.waitForFunction(() => window.__illogical?.control?.phase === "ready");
}

/** Show `host` in the tab view, connected. */
export async function show(page: Page, host: string) {
  await expect.poll(() => page.evaluate(() => window.__illogical.hosts.names), { timeout: 30_000 }).toContain(host);
  await page.evaluate((h) => window.__illogical.hosts.select(h), host);
  await expect
    .poll(() => page.evaluate((h) => window.__illogical.hosts.current === h && window.__illogical.client.connected && !!window.__illogical.client.state, host), {
      timeout: 30_000,
    })
    .toBe(true);
}

/** A request to the shown host's API, through its channel. */
export const call = (page: Page, method: string, path: string, body?: unknown) =>
  page.evaluate(
    async ([m, p, b]) => {
      const r = await window.__illogical.client.request(m as string, p as string, b);
      return { ok: r.ok, status: r.status, body: await r.json<Record<string, unknown>>().catch(() => null) };
    },
    [method, path, body] as const,
  );

export const keys = (page: Page) => page.evaluate(() => (window.__illogical?.fleet?.panes ?? []).map((p) => p.key).sort());
export const live = (page: Page, hosts: string[]) =>
  page.evaluate((hs) => hs.every((h) => window.__illogical?.fleet?.host(h)?.state === "connected"), hosts);
export const groups = (page: Page) =>
  page.evaluate(() => window.__illogical.fleet.byPerson().map((g) => `${g.person.kind}:${g.person.name}:${new Set(g.panes.map((p) => p.host)).size}`));
