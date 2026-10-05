// `just screenshots`, the swarm (M26): a fleet of six real daemons
// (e2e/fake-fleet.ts) with about 360 real panes between them, each a
// process doing scripted work at its own pace (builds, tests, servers, log
// tails, editors, idle shells) in a handful of throwaway git repositories
// and plain directories, under a scratch HOME. Agents ask through Claude
// Code's real hooks, and a batch of tests fails on one machine. Every name
// is made up.

import { createServer } from "node:net";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { devices, expect, test, type Page } from "@playwright/test";
import { FakeFleet } from "../e2e/fake-fleet";

const out = join(dirname(fileURLToPath(import.meta.url)), "../../site/img");
const MACHINES = ["workstation", "laptop", "build-01", "build-02", "build-03", "team-box"];
const PROJECTS = ["api", "web", "mobile", "infra", "docs"];
/** Panes per machine, about 360 in all. */
const PER_MACHINE = 60;

// The fleet's daemons inherit this: no "a newer release is out" chip.
process.env.ILLOGICAL_NO_UPDATE_CHECK = "true";
// Nor this machine's Fountain runner, if it is one.
process.env.ILLOGICAL_FOUNTAIN_UNIT_FILE = "/nonexistent/fountain-runner.service";

let fake: FakeFleet;
let base = "";

/** A free port from the kernel (well away from the e2e tests' 7750–7789). */
function freePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const s = createServer();
    s.once("error", reject);
    s.listen(0, "127.0.0.1", () => {
      const port = (s.address() as { port: number }).port;
      s.close(() => resolve(port));
    });
  });
}

const pick = <T,>(a: T[]): T => a[Math.floor(Math.random() * a.length)];

/** What one pane runs: mostly projects, some plain directories; a mix of
 * busy and quiet work, and a few shells at their prompt. */
function work(): [string, string | undefined] {
  const where = Math.random() < 0.8 ? pick(PROJECTS) : pick(["scratch", "var"]);
  const command = pick([
    "cargo build", "cargo test", "cargo build", "go build ./...", "go test ./...", "make -j8",
    "pytest -x", "pytest -x", "npm run dev", "uvicorn app:main --reload", "docker compose logs -f",
    "tail -f worker.log", "journalctl -f", "nvim src/main.rs", "vim notes.md",
    undefined, undefined, undefined, undefined,
  ]);
  return [where, command];
}

/** Up to `n` of `jobs` at once. */
async function batch<T>(jobs: (() => Promise<T>)[], n = 8) {
  const queue = [...jobs];
  await Promise.all(Array.from({ length: n }, async () => {
    for (let job = queue.shift(); job; job = queue.shift()) await job();
  }));
}

test.describe.configure({ mode: "serial" });

test.beforeAll(async () => {
  test.setTimeout(240_000);
  fake = new FakeFleet(PROJECTS);
  for (const m of MACHINES) await fake.machine(m, await freePort());
  base = fake.machines[0].url;
  await batch(MACHINES.flatMap((m) => Array.from({ length: PER_MACHINE }, () => () => {
    const [where, command] = work();
    return fake.run(m, where, command);
  })));
  // What needs someone: agents asking, a failed batch, a long build done.
  await fake.agentAsks("workstation", "api", "cargo test -p api-server");
  await fake.agentAsks("laptop", "api", "git push origin main");
  await fake.agentAsks("team-box", "infra", "terraform plan -out=tfplan");
  await fake.trouble("build-02", 4);
  await fake.finish("build-01");
});

test.afterAll(() => fake?.stop());

async function swarm(page: Page, by?: string) {
  await page.goto(`${base}/#swarm`);
  await expect
    .poll(() => page.evaluate(() => window.__illogical?.fleet.list.filter((h) => h.state === "connected").length ?? 0), { timeout: 30_000 })
    .toBe(MACHINES.length);
  await expect.poll(() => page.evaluate(() => window.__illogical.fleet.panes.length), { timeout: 30_000 }).toBeGreaterThanOrEqual(350);
  await expect(page.locator(".swarm-card[data-bundle]").first()).toBeVisible({ timeout: 30_000 });
  if (by) await page.getByRole("button", { name: by, exact: true }).click();
  // Let the field settle and the activity come through, then fit it.
  await page.waitForTimeout(8000);
  await page.getByRole("button", { name: "Fit", exact: true }).click();
  await page.waitForTimeout(2500);
}

test.describe("desktop", () => {
  test.use({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: 2 });
  test("the swarm, with what needs you on the rail", async ({ page }) => {
    await swarm(page);
    await page.screenshot({ path: join(out, "swarm.png") });
  });
});

test.describe("phone", () => {
  const { viewport, userAgent, deviceScaleFactor, isMobile, hasTouch } = devices["Pixel 7"];
  test.use({ viewport, userAgent, deviceScaleFactor, isMobile, hasTouch });
  test("the swarm on a phone: cards along the bottom", async ({ page }) => {
    await swarm(page);
    await page.screenshot({ path: join(out, "swarm-phone.png") });
  });
});
