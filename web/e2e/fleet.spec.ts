// M25: the fleet in one page. The page holds a summaries-only connection to
// every host on the home daemon's list, merged into one list of panes:
// three machines (stand-ins for geek, jake-mini and a resident sandbox)
// show together, on the laptop and on the phone. A machine that stops
// answering greys within 10 s, its panes still listed, and comes back when
// it does. Twenty machines reconnect after a wake without a burst of
// failures.

import { spawn, type ChildProcess } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { devices, expect, test, type Page } from "@playwright/test";
import { ANY, daemonPort } from "./ports";
import { closeContexts } from "./helpers";

test.afterAll(closeContexts);

let homeUrl = "";

const states: string[] = [];
const daemons = new Map<string, ChildProcess>();
const stateOf = new Map<string, string>();
const portOf = new Map<string, number>();

test.use({ baseURL: async ({}, use) => use(homeUrl) });
test.describe.configure({ mode: "serial" });

/** Start (or restart, on the same port) a daemon; its port. */
async function startDaemon(name: string, extra: string[] = []) {
  let state = stateOf.get(name);
  if (!state) {
    state = mkdtempSync(join(tmpdir(), `ilg-e2e-fleet-${name}-`));
    states.push(state);
    stateOf.set(name, state);
  }
  const listen = portOf.has(name) ? `127.0.0.1:${portOf.get(name)}` : ANY;
  const d = spawn(
    "../target/debug/illogicald",
    [
      ...["--listen", listen, "--name", name, "--state-dir", state],
      ...["--shell", "bash --norc --noprofile", "--no-manager-env", "--tailscale-socket", "/nonexistent/tailscaled.sock"],
      ...extra,
    ],
    { stdio: "ignore" },
  );
  daemons.set(name, d);
  const port = portOf.get(name) ?? (await daemonPort(state, d));
  portOf.set(name, port);
  for (let i = 0; i < 100; i++) {
    try {
      if ((await fetch(`http://127.0.0.1:${port}/api/host`)).ok) return port;
    } catch {
      // not yet
    }
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error(`daemon ${name} did not start`);
}

async function addHost(name: string) {
  const res = await fetch(`${homeUrl}/api/hosts`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ name, urls: [`http://127.0.0.1:${portOf.get(name)}`] }),
  });
  expect(res.ok).toBe(true);
}

test.beforeAll(async () => {
  homeUrl = `http://127.0.0.1:${await startDaemon("geek")}`;
  await startDaemon("jake-mini", ["--allow-origin", homeUrl]);
  await startDaemon("sandbox", ["--allow-origin", homeUrl]);
  await addHost("jake-mini");
  await addHost("sandbox");
  // Something to tell them apart by.
  for (const [name, word] of [
    ["jake-mini", "mini"],
    ["sandbox", "box"],
  ] as const) {
    await fetch(`http://127.0.0.1:${portOf.get(name)}/api/run`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ cwd: "/tmp", command: `echo ${word}; sleep 600` }),
    });
  }
});

test.afterAll(() => {
  for (const d of daemons.values()) d.kill("SIGKILL");
  for (const s of states) rmSync(s, { recursive: true, force: true });
});

const hostStates = (page: Page) =>
  page.evaluate(() => Object.fromEntries((window.__illogical?.fleet?.list ?? []).map((h) => [h.name, h.state])));
const panesByHost = (page: Page) =>
  page.evaluate(() => {
    const out: Record<string, { n: number; stale: boolean }> = {};
    for (const p of window.__illogical?.fleet?.panes ?? []) {
      const e = (out[p.host] ??= { n: 0, stale: p.stale });
      e.n++;
    }
    return out;
  });

async function allThree(page: Page) {
  await page.goto("/");
  await expect
    .poll(() => hostStates(page), { timeout: 15_000 })
    .toEqual({ geek: "connected", "jake-mini": "connected", sandbox: "connected" });
  await expect.poll(async () => Object.keys(await panesByHost(page)).sort()).toEqual(["geek", "jake-mini", "sandbox"]);
  const by = await panesByHost(page);
  expect(by["jake-mini"].n).toBe(2);
  // Each pane is (host, pane id): the same id on two hosts is two panes.
  const keys = await page.evaluate(() => window.__illogical.fleet.panes.map((p) => p.key));
  expect(new Set(keys).size).toBe(keys.length);
  expect(keys).toContain("jake-mini:1");
  expect(keys).toContain("sandbox:1");
}

test("every machine's panes in one page, on the laptop and the phone", async ({ page, browser }) => {
  await allThree(page);
  // The tab view still shows one host, connected for real.
  await expect.poll(() => page.evaluate(() => window.__illogical.client.connected)).toBe(true);
  expect(await page.evaluate(() => window.__illogical.client.panes.size)).toBeGreaterThan(0);
  // Summary connections make no terminals and don't show as people.
  expect(await page.evaluate(() => window.__illogical.client.others().length)).toBe(0);
  // The host menu says what each is doing.
  await page.locator(".host-button").click();
  await expect(page.getByRole("menuitem", { name: /jake-mini\s+· 2 panes · live/ })).toBeVisible();
  await page.keyboard.press("Escape");

  // Opening a pane from the fleet shows it in its tab, attached for real.
  // The one that printed "mini" (the newest).
  const mini = await page.evaluate(() => Math.max(...window.__illogical.fleet.panes.filter((p) => p.host === "jake-mini").map((p) => p.id)));
  await page.evaluate((id) => window.__illogical.fleet.open("jake-mini", id), mini);
  await expect.poll(() => page.evaluate(() => window.__illogical.hosts.current)).toBe("jake-mini");
  await expect.poll(() => page.evaluate(() => window.__illogical.client.active())).toBe(mini);
  await expect.poll(() => page.evaluate((p) => window.__illogical.text(p), mini)).toContain("mini");
  await page.evaluate(() => window.__illogical.hosts.select("geek"));

  const phone = await (await browser.newContext({ ...devices["Pixel 7"], baseURL: homeUrl })).newPage();
  await allThree(phone);
  await phone.context().close();
});

test("a machine that stops answering greys within 10 s, and comes back", async ({ page }) => {
  await allThree(page);
  // Killed: its socket closes.
  daemons.get("jake-mini")!.kill("SIGKILL");
  const t0 = Date.now();
  await expect.poll(() => panesByHost(page).then((b) => b["jake-mini"]?.stale), { timeout: 10_000 }).toBe(true);
  expect(Date.now() - t0).toBeLessThan(10_000);
  // Its panes stay in view, from the last summary.
  expect((await panesByHost(page))["jake-mini"].n).toBe(2);
  expect((await hostStates(page))["jake-mini"]).toBe("stale");
  expect((await panesByHost(page)).sandbox.stale).toBe(false);
  await startDaemon("jake-mini", ["--allow-origin", homeUrl]);
  await expect.poll(() => hostStates(page).then((s) => s["jake-mini"]), { timeout: 15_000 }).toBe("connected");
  await expect.poll(() => panesByHost(page).then((b) => b["jake-mini"]?.stale)).toBe(false);

  // Unplugged: the socket stays open but nothing answers (a stopped
  // process stands in for a machine gone off the network).
  const box = daemons.get("sandbox")!;
  box.kill("SIGSTOP");
  const t1 = Date.now();
  try {
    await expect.poll(() => panesByHost(page).then((b) => b.sandbox?.stale), { timeout: 10_000 }).toBe(true);
    expect(Date.now() - t1).toBeLessThan(10_000);
    // The others carry on.
    expect((await hostStates(page))["jake-mini"]).toBe("connected");
  } finally {
    box.kill("SIGCONT");
  }
  await expect.poll(() => hostStates(page).then((s) => s.sandbox), { timeout: 15_000 }).toBe("connected");
});

test("twenty machines come back after a wake without a burst of failures", async ({ page }) => {
  test.setTimeout(120_000);
  const many: string[] = [];
  for (let i = 0; i < 20; i++) {
    const name = `m${String(i).padStart(2, "0")}`;
    await startDaemon(name, ["--allow-origin", homeUrl]);
    await addHost(name);
    many.push(name);
  }
  await page.goto("/");
  const live = () => page.evaluate(() => (window.__illogical?.fleet?.list ?? []).filter((h) => h.state === "connected").length);
  const t0 = Date.now();
  await expect.poll(live, { timeout: 30_000 }).toBe(23);
  const firstMs = Date.now() - t0;

  const wakes: { allBackMs: number; failures: number; started: number }[] = [];
  for (let round = 0; round < 3; round++) {
    await page.evaluate(() => {
      window.__illogical.fleet.sleepAll();
      window.__illogical.fleet.wake();
    });
    try {
      await expect.poll(live, { timeout: 30_000 }).toBe(23);
    } catch (e) {
      console.log("not back:", JSON.stringify(await hostStates(page)));
      throw e;
    }
    await expect.poll(() => page.evaluate(() => window.__illogical.fleet.stats.allBackMs)).not.toBeNull();
    wakes.push(
      await page.evaluate(() => {
        const f = window.__illogical.fleet;
        return { allBackMs: f.stats.allBackMs!, failures: f.failures, started: f.stats.started };
      }),
    );
  }
  console.log(`fleet: 23 hosts first connected in ${firstMs} ms; wakes: ${JSON.stringify(wakes)}`);
  for (const w of wakes) {
    expect(w.failures).toBe(0);
    // S16 saw 2.2–4.6 s for 20 at once; spread and limited, it's well
    // under that.
    expect(w.allBackMs).toBeLessThan(5000);
  }
  // The cap leaves room: no notice.
  expect(await page.evaluate(() => window.__illogical.fleet.notice)).toBeNull();
  for (const name of many) expect((await hostStates(page))[name]).toBe("connected");
});

test("a sleeping sandbox isn't woken to be counted; past the cap, the rest wait with a notice", async ({ page }) => {
  await page.goto("/");
  await expect.poll(() => page.evaluate(() => window.__illogical?.fleet?.connections ?? 0)).toBeGreaterThan(0);
  const r = await page.evaluate(() => {
    const f = window.__illogical.fleet;
    const before = f.connections;
    const refs = f.list.map((h) => ({ name: h.name, id: h.id, transport: h.transport }));
    // A sandbox its provider says is cold: listed, never connected.
    f.setHosts([...refs, { name: "vm", transport: "provider", status: "cold" }]);
    const vm = f.host("vm")!.state;
    const after = f.connections;
    // Thirty more than the cap allows.
    const more = Array.from({ length: 30 }, (_, i) => ({ name: `x${i}`, transport: "tailnet" }));
    f.setHosts([...refs, ...more]);
    return { vm, before, after, capped: f.list.filter((h) => h.state === "capped").length, connections: f.connections, notice: f.notice };
  });
  expect(r.vm).toBe("asleep");
  expect(r.after).toBe(r.before);
  expect(r.connections).toBe(24);
  expect(r.capped).toBe(r.before + 30 - 24);
  expect(r.notice).toContain("24 of");
});
