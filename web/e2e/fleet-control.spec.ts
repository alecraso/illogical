// M25 through illogical control: the fleet on a page served by control. Three
// machines join an account (one reachable directly, two only through the
// relay); the laptop and a phone each see all three machines' panes at
// once, and the relayed ones share one WebSocket to control, each
// daemon's end-to-end channel inside it. A relayed machine that goes away
// greys within 10 s and comes back with its daemon.

import { spawn, type ChildProcess } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { createServer, type Server } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test, type Page } from "@playwright/test";
import { ANY, controlPort, daemonPort, listen } from "./ports";

let base = "";
const procs: ChildProcess[] = [];
const dirs: string[] = [];
const daemonOf = new Map<string, { proc: ChildProcess; args: string[] }>();
let gh: Server;

test.describe.configure({ mode: "serial" });
test.use({ baseURL: async ({}, use) => use(base) });

function temp(what: string) {
  const d = mkdtempSync(join(tmpdir(), `illogical-e2e-fleetc-${what}-`));
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
      res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify({ id: 8, login: "fleet" }));
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
  for (const d of daemonOf.values()) d.proc.kill("SIGKILL");
  gh?.close();
  for (const d of dirs) rmSync(d, { recursive: true, force: true });
});

async function signIn(page: Page) {
  await page.goto("/");
  await page.locator("[data-signin=github]").click();
  await expect(page.locator(".control-center, .control-page, .app")).toBeVisible();
}

function runDaemon(name: string) {
  const d = daemonOf.get(name)!;
  d.proc = spawn("../target/debug/illogicald", d.args, { stdio: "ignore" });
}

/** `illogicald join`, approved from `page`; then the daemon runs. Its port. */
async function addMachine(page: Page, name: string, direct: boolean) {
  const state = temp(name);
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
  const args = [
    ...["--listen", ANY, "--name", name, "--state-dir", state],
    ...["--shell", "bash --norc --noprofile", "--no-manager-env", "--tailscale-socket", "/nonexistent/sock"],
    ...(direct ? ["--direct-url", "http://127.0.0.1:0"] : []),
  ];
  daemonOf.set(name, { proc: undefined as unknown as ChildProcess, args });
  runDaemon(name);
  return daemonPort(state, daemonOf.get(name)!.proc);
}

const ready = (page: Page) => page.waitForFunction(() => window.__illogical?.control?.phase === "ready", null, { timeout: 20_000 });
const hosts = (page: Page) =>
  page.evaluate(() =>
    Object.fromEntries((window.__illogical?.fleet?.list ?? []).map((h) => [h.name, { state: h.state, path: h.path }])),
  );
const fleetPanes = (page: Page) =>
  page.evaluate(() => {
    const out: Record<string, { n: number; stale: boolean }> = {};
    for (const p of window.__illogical?.fleet?.panes ?? []) (out[p.host] ??= { n: 0, stale: p.stale }).n++;
    return out;
  });

/** The WebSocket URLs a page opens, from now on. */
function sockets(page: Page): string[] {
  const urls: string[] = [];
  page.on("websocket", (ws) => urls.push(ws.url()));
  return urls;
}

async function everyMachine(page: Page) {
  await ready(page);
  await expect
    .poll(() => hosts(page), { timeout: 30_000 })
    .toEqual({
      box: { state: "connected", path: "direct" },
      mac: { state: "connected", path: "relayed" },
      sandbox: { state: "connected", path: "relayed" },
    });
  await expect.poll(async () => Object.keys(await fleetPanes(page)).sort()).toEqual(["box", "mac", "sandbox"]);
}

let laptop: Page;

test("the laptop sees every machine at once; relayed ones share one socket to control", async ({ browser }) => {
  laptop = await (await browser.newContext()).newPage();
  await signIn(laptop);
  await laptop.locator("[data-stored-codes]").check();
  await laptop.locator("[data-saved-codes]").click();
  const box = await addMachine(laptop, "box", true);
  await addMachine(laptop, "mac", false);
  await addMachine(laptop, "sandbox", false);
  const urls = sockets(laptop);
  await laptop.goto("/");
  await everyMachine(laptop);
  const relayed = urls.filter((u) => u.includes("/api/relay/"));
  // One socket to the relay carries both relayed daemons (and the tab
  // view's own channel, when it shows one of them).
  expect(relayed.filter((u) => u.endsWith("/api/relay/m"))).toHaveLength(1);
  expect(relayed.filter((u) => u.includes("/api/relay/c/"))).toHaveLength(0);
  // The direct one went straight to its machine.
  expect(urls.some((u) => u.startsWith(`ws://127.0.0.1:${box}/e2e`))).toBe(true);
  await laptop.evaluate(() => window.__illogical.hosts.select("mac"));
  await expect.poll(() => laptop.evaluate(() => window.__illogical.client.connected), { timeout: 20_000 }).toBe(true);
  expect(urls.filter((u) => u.includes("/api/relay/"))).toHaveLength(1);
});

test("so does a phone", async ({ browser }) => {
  const phone = await (await browser.newContext({ viewport: { width: 390, height: 760 }, isMobile: true, hasTouch: true })).newPage();
  await signIn(phone);
  await expect(phone.getByText("Approve this browser")).toBeVisible();
  const fp = await phone.locator("[data-fingerprint]").getAttribute("data-fingerprint");
  await expect(laptop.locator(`[data-pending="${fp}"]`)).toBeVisible({ timeout: 20_000 });
  await laptop.locator("[data-approve]").click();
  const urls = sockets(phone);
  await everyMachine(phone);
  expect(urls.filter((u) => u.endsWith("/api/relay/m")).length).toBeLessThanOrEqual(1);
  expect(urls.filter((u) => u.includes("/api/relay/c/"))).toHaveLength(0);
  await phone.context().close();
});

test("a relayed machine that goes away greys within 10 s, and comes back", async () => {
  await laptop.evaluate(() => window.__illogical.hosts.select("box"));
  await everyMachine(laptop);
  daemonOf.get("mac")!.proc.kill("SIGKILL");
  const t0 = Date.now();
  await expect.poll(() => fleetPanes(laptop).then((p) => p.mac?.stale), { timeout: 10_000 }).toBe(true);
  expect(Date.now() - t0).toBeLessThan(10_000);
  expect((await fleetPanes(laptop)).mac.n).toBeGreaterThan(0);
  expect((await fleetPanes(laptop)).sandbox.stale).toBe(false);
  runDaemon("mac");
  await expect.poll(() => hosts(laptop).then((h) => h.mac?.state), { timeout: 30_000 }).toBe("connected");
  await expect.poll(() => fleetPanes(laptop).then((p) => p.mac?.stale)).toBe(false);
});

test("twenty machines, nineteen of them relayed: one socket, back after a wake without failures", async () => {
  test.setTimeout(180_000);
  for (let i = 0; i < 17; i++) await addMachine(laptop, `r${String(i).padStart(2, "0")}`, false);
  await laptop.goto("/");
  await ready(laptop);
  const live = () => laptop.evaluate(() => (window.__illogical?.fleet?.list ?? []).filter((h) => h.state === "connected").length);
  await expect.poll(live, { timeout: 60_000 }).toBe(20);
  const urls = sockets(laptop);
  const wakes: { allBackMs: number; failures: number }[] = [];
  for (let round = 0; round < 3; round++) {
    const before = urls.length;
    await laptop.evaluate(() => {
      window.__illogical.fleet.sleepAll();
      window.__illogical.fleet.wake();
    });
    await expect.poll(live, { timeout: 30_000 }).toBe(20);
    await expect.poll(() => laptop.evaluate(() => window.__illogical.fleet.stats.allBackMs)).not.toBeNull();
    wakes.push(await laptop.evaluate(() => ({ allBackMs: window.__illogical.fleet.stats.allBackMs!, failures: window.__illogical.fleet.failures })));
    const opened = urls.slice(before).filter((u) => u.includes("/api/relay/"));
    // Nineteen daemons came back over one new socket to the relay.
    expect(opened).toEqual([`${base.replace("http", "ws")}/api/relay/m`]);
  }
  console.log(`fleet through control: 20 machines (19 relayed); wakes: ${JSON.stringify(wakes)}`);
  for (const w of wakes) {
    expect(w.failures).toBe(0);
    expect(w.allBackMs).toBeLessThan(5000);
  }
});
