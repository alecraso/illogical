// M26/M30 on phones (#214 section 6): two teammates, each on a phone. Alice
// is on a Pixel 7 (Chrome) on a slow network (Chrome's network emulation:
// 150 ms each way, 1.6 Mbit/s down); Bob is on an iPhone (WebKit). Alice
// has a team box and a machine of her own, and shares a session on it with
// Bob; Bob has a machine. Each phone's swarm holds the team's panes, grouped
// by person, with the cards along the bottom; a build that fails on Alice's
// machine is a card on Bob's iPhone, Bob reruns it from there, and Alice's
// phone sees it run again. A tap on a tile opens the pane.
//
// "Different networks" for real is the testnet's job: the test at the end
// runs a machine in a container on its own Docker network with `tc netem`,
// joined to this control, and the phones reach it only through the relay.
// It needs Docker and fails without it; see there.

import { execFileSync, spawn, spawnSync } from "node:child_process";
import { existsSync, writeFileSync } from "node:fs";
import { connect, createServer } from "node:net";
import { join, resolve } from "node:path";
import { expect, test, type Browser, type Page } from "@playwright/test";
import { iphone, launchWebkit, pixel7 } from "./phones";
import { listen } from "./ports";
import { call, groups, home, keys, live, show, TeamControl } from "./team-fixture";
import { closeContexts } from "./helpers";

test.afterAll(closeContexts);

const control = new TeamControl("tswarm-phones");
let webkit: Browser;
let alice: Page;
let bob: Page;
let team = "";
// The netem box and its network, removed in afterAll too: a timeout never
// reaches the test's finally.
let netemCleanup = () => {};

test.describe.configure({ mode: "serial" });

test.beforeAll(async () => {
  await control.start();
  webkit = await launchWebkit();
});

test.afterAll(async () => {
  await webkit?.close().catch((e) => console.log(`webkit close: ${e}`));
  netemCleanup();
  control.stop();
});

/** A phone's swarm, with `want` among its panes. */
async function swarm(page: Page, want: string[]) {
  await page.goto("/#swarm");
  await expect(page.locator(".swarm")).toBeVisible({ timeout: 20_000 });
  await expect.poll(() => keys(page), { timeout: 60_000 }).toEqual(expect.arrayContaining(want));
}

const reason = (page: Page, key: string) =>
  page.evaluate((k) => window.__illogical.fleet.panes.find((p) => p.key === k)?.info.reason?.kind ?? null, key);
const capture = (page: Page, host: string, pane: number) =>
  page.evaluate(
    async ([h, p]) => (await (await window.__illogical.fleet.request(h, "GET", `/api/panes/${p}/capture?format=text`)).text!()),
    [host, pane] as const,
  );

test("two teammates on phones, a team box, a machine each", async ({ browser }) => {
  test.setTimeout(180_000);
  alice = await control.person(browser, "alice", pixel7);
  await alice.evaluate(() => window.__illogical.control!.createTeam("Acme"));
  team = await alice.evaluate(() => window.__illogical.control!.teams[0].team);
  const link = await alice.evaluate((t) => window.__illogical.control!.invite(t, "editor", true), team);
  bob = await control.person(webkit, "bob", iphone);
  await bob.goto(link);
  await bob.locator("[data-accept-invite]").tap();
  await expect(bob.locator("[data-invite-pending]")).toBeVisible();
  await alice.evaluate(() => window.__illogical.control!.refresh());
  await expect(alice.locator("[data-admit-yes]")).toBeVisible({ timeout: 15_000 });
  await alice.locator("[data-admit-yes]").tap();
  await expect.poll(() => alice.evaluate(() => window.__illogical.control!.teams[0].roster.members.length), { timeout: 15_000 }).toBe(2);

  await control.addMachine(alice, "buildbox", team);
  await control.addMachine(alice, "a1");
  await control.addMachine(bob, "b1");
  for (const p of [alice, bob]) await home(p);
  await expect.poll(() => live(alice, ["buildbox", "a1"]), { timeout: 30_000 }).toBe(true);
  await expect.poll(() => live(bob, ["buildbox", "b1"]), { timeout: 30_000 }).toBe(true);

  // a1's first session, shared with Bob.
  const bobAcct = await alice.evaluate(() => window.__illogical.control!.person("bob"));
  await show(alice, "a1");
  expect(
    (await call(alice, "POST", "/api/acl", { session: 1, principal: `account:${bobAcct.account}`, role: "editor", root: bobAcct.root, name: "bob" })).ok,
  ).toBe(true);
  await expect
    .poll(async () => (await bob.evaluate(() => window.__illogical.control!.refresh()), live(bob, ["a1", "buildbox", "b1"])), { timeout: 60_000 })
    .toBe(true);  // Alice's tab view goes back to the team box, so nobody looks at a1.
  await show(alice, "buildbox");
});

test("both phones hold the team's swarm, by person, with the cards along the bottom", async () => {
  test.setTimeout(120_000);
  // Alice's phone on a slow network from here on.
  const cdp = await alice.context().newCDPSession(alice);
  await cdp.send("Network.enable");
  await cdp.send("Network.emulateNetworkConditions", { offline: false, latency: 150, downloadThroughput: 200_000, uploadThroughput: 90_000 });

  await swarm(alice, ["buildbox:1", "a1:1"]);
  await swarm(bob, ["buildbox:1", "a1:1", "b1:1"]);
  // Safari's once-only hint to add it to the Home Screen (#96) sits over the
  // card strip until it's dismissed, as a person would the first time.
  await bob.locator("[data-install-hint] .install-hint-close").tap();
  await expect(bob.locator("[data-install-hint]")).toHaveCount(0);
  // What they share is the same on both phones.
  const shared = (ks: string[]) => ks.filter((k) => k.startsWith("buildbox:") || k === "a1:1");
  expect(shared(await keys(bob))).toEqual(shared(await keys(alice)));
  expect((await keys(alice)).some((k) => k.startsWith("b1:"))).toBe(false);
  // (The page draws "me" as "you", whatever name the fleet has for it.)
  const byPerson = async (page: Page) => (await groups(page)).map((g) => g.replace(/^me:[^:]*:/, "me:"));
  await expect.poll(() => byPerson(alice)).toEqual(["me:1", "team:Acme:1"]);
  await expect.poll(() => byPerson(bob)).toEqual(["me:1", "person:alice:1", "team:Acme:1"]);
  for (const [page, height] of [
    [alice, pixel7.viewport.height],
    [bob, iphone.viewport.height],
  ] as const) {
    const rail = await page.locator(".swarm-rail").boundingBox();
    expect(rail!.y).toBeGreaterThan(height / 2);
  }
});

test("a build fails on Alice's machine; Bob reruns it from his iPhone's card, and Alice's phone sees it", async () => {
  test.setTimeout(120_000);
  // A new tab in the shared session, which nobody shows.
  const pane = await alice.evaluate(async () => (await (await window.__illogical.fleet.request("a1", "POST", "/api/run", { session: "1" })).json<{ pane: number }>()).pane);
  const key = `a1:${pane}`;
  await expect.poll(() => keys(bob), { timeout: 30_000 }).toContain(key);
  const send = (text: string) =>
    alice.evaluate(([p, t]) => window.__illogical.fleet.request("a1", "POST", `/api/panes/${p}/send`, { text: t, enter: true }), [pane, text] as const);
  await send("build() { sleep 3.2; echo TEAM-BUILD-$((40+2)); return 2; }");
  await send("build");
  await expect.poll(() => reason(bob, key), { timeout: 30_000 }).toBe("failed");
  const card = bob.locator(`.swarm-card[data-panes~="${key}"]`);
  await expect(card).toBeVisible({ timeout: 20_000 });
  await card.scrollIntoViewIfNeeded();
  await card.locator("[data-rerun]").tap();
  // On Alice's phone, over its slow network: the build ran twice.
  await expect.poll(async () => (await capture(alice, "a1", pane)).match(/^TEAM-BUILD-42/gm)?.length ?? 0, { timeout: 30_000 }).toBe(2);
  await expect.poll(() => reason(alice, key), { timeout: 30_000 }).toBe("failed");
});

test("a tap on a tile opens the pane, on both phones", async () => {
  test.setTimeout(60_000);
  for (const page of [alice, bob]) {
    await page.locator("[data-fit]").tap();
    await page.waitForTimeout(1500);
    const pos = await page.evaluate(() => (window.__illogical.swarm as unknown as { screenOf(k: string): { x: number; y: number } | null }).screenOf("buildbox:1"));
    expect(pos).not.toBeNull();
    await page.touchscreen.tap(pos!.x, pos!.y);
    await expect(page.locator(".swarm")).toHaveCount(0, { timeout: 15_000 });
    await expect.poll(() => page.evaluate(() => window.__illogical.hosts.current), { timeout: 30_000 }).toBe("buildbox");
    await expect.poll(() => page.evaluate(() => window.__illogical.client.active())).toBe(1);
  }
});

// The same team, with a machine on another network: a Debian container on
// a Docker network of its own, its link slowed and made lossy with
// `tc netem`, running the static illogicald (`just static aarch64` or
// x86_64, per Docker's architecture). It joins this control, approved from
// Alice's phone, and both phones' swarms reach it through the relay only
// (the container's address isn't routable from here). It needs Docker
// and the static binary, and fails without them; only
// ILLOGICAL_SKIP_DOCKER=1 skips it, saying so.
test("a machine on another network, behind netem, in both phones' swarms", async () => {
  if (process.env.ILLOGICAL_SKIP_DOCKER === "1") console.log("SKIP: team-swarm-phones across networks (ILLOGICAL_SKIP_DOCKER=1)");
  test.skip(process.env.ILLOGICAL_SKIP_DOCKER === "1", "ILLOGICAL_SKIP_DOCKER=1");
  test.setTimeout(300_000);
  let arch: string;
  try {
    arch = execFileSync("docker", ["info", "--format", "{{.Architecture}}"], { encoding: "utf8" }).trim().replace("arm64", "aarch64");
  } catch (e) {
    throw new Error(`this test needs Docker (ILLOGICAL_SKIP_DOCKER=1 skips it): ${e}`);
  }
  const bin = resolve(`../target/${arch}-unknown-linux-musl/release/illogicald`);
  expect(existsSync(bin), `no ${bin}: run just static ${arch}`).toBe(true);
  const project = process.env.COMPOSE_PROJECT_NAME ?? "illo-b4";
  const name = `${project}-netem-box`;
  const net = `${project}-netem`;
  const docker = (...args: string[]) => execFileSync("docker", args, { encoding: "utf8" }).trim();
  const quiet = (...args: string[]) => spawnSync("docker", args, { stdio: "ignore" });
  netemCleanup = () => {
    quiet("rm", "-f", name);
    quiet("network", "rm", net);
  };
  netemCleanup();
  // Control listens on the host's loopback, which the box can't reach: a
  // forwarder on the box network's gateway address brings it there.
  const forward = createServer((c) => {
    const up = connect(Number(new URL(control.base).port), "127.0.0.1");
    up.on("error", () => c.destroy());
    c.on("error", () => up.destroy());
    c.pipe(up).pipe(c);
  });
  try {
    const dir = control.temp("netem");
    writeFileSync(join(dir, "Dockerfile"), "FROM debian:bookworm-slim\nRUN apt-get update && apt-get install -y --no-install-recommends iproute2 socat && rm -rf /var/lib/apt/lists/*\n");
    docker("build", "-q", "-t", `${project}-netem`, dir);
    docker("network", "create", net);
    const gateway = docker("network", "inspect", net, "--format", "{{(index .IPAM.Config 0).Gateway}}");
    const via = await listen(forward, gateway);
    // In the box, socat brings the forwarder to control's own address, so
    // the URLs control hands out work there.
    const port = new URL(control.base).port;
    docker(
      "run", "-d", "--name", name, "--network", net, "--cap-add", "NET_ADMIN",
      "-v", `${bin}:/usr/local/bin/illogicald:ro`, `${project}-netem`,
      "sh", "-c", `socat TCP-LISTEN:${port},bind=127.0.0.1,fork,reuseaddr TCP:${gateway}:${via} & sleep infinity`,
    );
    docker("exec", name, "tc", "qdisc", "add", "dev", "eth0", "root", "netem", "delay", "120ms", "40ms", "loss", "1%");
    expect(docker("exec", name, "tc", "qdisc", "show", "dev", "eth0")).toContain("netem");

    await home(alice);
    const joining = spawn("docker", ["exec", "-i", name, "illogicald", "join", control.base, "--name", "far", "--state-dir", "/root/state", "--team", team], {
      stdio: ["pipe", "pipe", "ignore"],
    });
    const exited = new Promise<number | null>((r) => joining.on("exit", r));
    const link = await new Promise<string>((res, rej) => {
      let out = "";
      joining.stdout!.on("data", (d) => {
        out += d;
        const m = out.match(/(http\S+#join=[A-Z0-9-]+)/);
        if (m) res(m[1]);
      });
      void exited.then((code) => rej(new Error(`illogicald join in the box exited (${code}) before printing a link`)));
    });
    await alice.goto(link);
    const account = await alice.locator("[data-join-account]").getAttribute("data-join-account");
    await alice.locator("[data-approve-join]").tap();
    joining.stdin!.end(`${account}\n`);
    expect(await exited).toBe(0);
    // With labs, like the rest of the suite's daemons (see labs.ts).
    docker("exec", name, "touch", "/root/state/labs");
    spawn("docker", ["exec", "-d", name, "illogicald", "--listen", "127.0.0.1:0", "--name", "far", "--state-dir", "/root/state", "--shell", "bash --norc --noprofile", "--no-manager-env"], {
      stdio: "ignore",
    });

    await swarm(alice, ["far:1"]);
    await swarm(bob, ["far:1"]);
    const t0 = Date.now();
    await alice.evaluate(() => window.__illogical.fleet.request("far", "POST", "/api/panes/1/send", { text: "echo FAR-$((40+2))", enter: true }));
    await expect.poll(async () => (await capture(bob, "far", 1)).includes("FAR-42"), { timeout: 30_000 }).toBe(true);
    console.log(`netem box: typed on Alice's phone, read on Bob's iPhone in ${Date.now() - t0} ms`);
  } finally {
    forward.close();
    netemCleanup();
  }
});
