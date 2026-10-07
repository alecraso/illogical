// M26: the swarm view, against a fake fleet (e2e/fake-fleet.ts): three
// daemons with scripted builds, tests, servers, logs, editors and agents
// that ask through Claude Code's real hooks. Clustering by each grouping
// (and the fallback groups for panes outside any git project), bundling on
// the "needs you" rail, and each action from the rail: allow, deny, answer,
// dismiss and a follow-up; done cards clearing themselves; a full rail;
// opening a pane, zooming to a cluster, the hover peek, a notification's
// deep link; and the phone's strip of cards, pinch and tap.

import { realpathSync } from "node:fs";
import { tmpdir } from "node:os";
import { devices, expect, test, type Page } from "@playwright/test";
import { FakeFleet } from "./fake-fleet";
import { closeContexts } from "./helpers";

test.afterAll(closeContexts);

let fake: FakeFleet;

test.use({ baseURL: async ({}, use) => use(fake?.machines[0]?.url) });
test.describe.configure({ mode: "serial" });

test.beforeAll(async () => {
  fake = new FakeFleet();
  await fake.machine("workstation");
  await fake.machine("build-01");
  await fake.machine("build-02");
  await fake.populate();
});

test.afterAll(() => fake?.stop());

/** The swarm, with every machine's panes in it. */
async function swarm(page: Page) {
  await page.goto("/#swarm");
  await expect
    .poll(() => page.evaluate(() => window.__arugula?.fleet.list.filter((h) => h.state === "connected").length ?? 0), { timeout: 20_000 })
    .toBe(3);
  await expect.poll(() => page.evaluate(() => window.__arugula.fleet.panes.length)).toBeGreaterThanOrEqual(15);
  await expect(page.locator(".swarm")).toBeVisible();
}

const clusters = (page: Page) =>
  page.evaluate(() => ((window.__arugula.swarm as { clusters: { name: string; n: number }[] } | null)?.clusters ?? []).map((c) => c.name).sort());

/** Dismiss whatever is on the rail, so tests start clean. */
async function clearRail(page: Page) {
  await page.evaluate(async () => {
    const f = window.__arugula.fleet;
    for (const p of f.panes) {
      if (!p.info.reason) continue;
      await f.request(p.host, "POST", "/api/attention/act", { action: "dismiss", pane: p.id }).catch(() => {});
    }
  });
  await expect(page.locator(".swarm-card[data-bundle]")).toHaveCount(0, { timeout: 10_000 });
}

/** A pane's screen, through its host's API. */
const capture = (page: Page, host: string, pane: number) =>
  page.evaluate(
    async ([h, p]) => (await (await window.__arugula.fleet.request(h, "GET", `/api/panes/${p}/capture?format=text`)).text!()).replace(/\n/g, ""),
    [host, pane] as const,
  );

test("clusters by project (with fallback groups), machine, kind, session and person, remembered", async ({ page }) => {
  await swarm(page);
  // By project: the git repositories, and directories for the rest, never
  // one "none" pile.
  // The fleet's other directory is under the temp dir: /tmp's group on
  // Linux, /private's on macOS (/var/folders is /private/var/folders).
  const temp = `/${realpathSync(tmpdir()).split("/")[1]}`;
  await expect.poll(() => clusters(page), { timeout: 10_000 }).toEqual(expect.arrayContaining(["api", "web", "infra", "~/scratch", temp]));
  expect(await clusters(page)).not.toContain("none");
  // Machines, kinds, sessions, people.
  await page.locator('[data-g="machine"]').click();
  await expect.poll(() => clusters(page)).toEqual(["build-01", "build-02", "workstation"]);
  await page.locator('[data-g="kind"]').click();
  await expect.poll(() => clusters(page)).toEqual(expect.arrayContaining(["build", "test", "server", "logs", "editor", "shell"]));
  await page.locator('[data-g="session"]').click();
  await expect.poll(async () => (await clusters(page)).every((c) => / · (workstation|build-01|build-02)$/.test(c))).toBe(true);
  await page.locator('[data-g="person"]').click();
  await expect.poll(() => clusters(page)).toEqual(["you"]);
  // Remembered on this device.
  await page.locator('[data-g="machine"]').click();
  await page.reload();
  await expect(page.locator('[data-g="machine"]')).toHaveAttribute("aria-pressed", "true");
  await expect.poll(() => clusters(page)).toEqual(["build-01", "build-02", "workstation"]);
  // By person (M30): yours, a teammate's and a team's, from the synthetic
  // fleet's owners beside these.
  const stop = await page.evaluateHandle(() => window.__arugula.swarmFake(40));
  await page.locator('[data-g="person"]').click();
  await expect.poll(() => clusters(page)).toEqual(["sam", "team infra", "you"]);
  await stop.evaluate((f) => f());
    // Tiles are coloured by kind: the legend has every kind.
  await expect(page.locator(".swarm-legend span")).toHaveCount(11);
});

test("failures on one machine bundle into one card, dismissed together", async ({ page }) => {
  await swarm(page);
  const panes = await fake.trouble("build-02", 3);
  const card = page.locator('.swarm-card[data-bundle="failed:build-02"]');
  await expect(card).toBeVisible({ timeout: 20_000 });
  await expect(card.locator(".ch b")).toHaveText("3 failed on build-02", { timeout: 10_000 });
  await expect(card.locator(".n")).toHaveText("×3");
  await expect(card.locator(".cq")).toContainText("cargo test -p relay failed (exit 101)");
  await card.getByRole("button", { name: "Dismiss all 3" }).click();
  await expect(card).toHaveCount(0);
  for (const id of panes) {
    await expect.poll(() => page.evaluate((k) => window.__arugula.fleet.panes.find((p) => p.key === k)?.info.attention, `build-02:${id}`)).toBe("idle");
  }
});

test("an agent's approval is allowed from the rail, and the follow-up wakes it", async ({ page }) => {
  await swarm(page);
  await clearRail(page);
  const pane = await fake.agentAsks("workstation", "api", "cargo test -p swarm");
  const card = page.locator(`.swarm-card[data-panes~="workstation:${pane}"]`);
  await expect(card).toBeVisible({ timeout: 20_000 });
  await expect(card.locator(".ch b")).toHaveText("Claude Code asks");
  await expect(card.locator(".agent-perm-cmd")).toHaveText("cargo test -p swarm");
  await card.getByRole("button", { name: "Allow", exact: true }).click();
  // The card says who allowed it, and offers a follow-up.
  const done = page.locator(`.swarm-card[data-answered][data-panes="workstation:${pane}"]`);
  await expect(done.locator(".answered-by")).toHaveText(/^Allowed by .+, \d\d:\d\d/);
  await expect.poll(() => capture(page, "workstation", pane)).toContain('"behavior":"allow"');
  await expect.poll(() => page.evaluate((k) => window.__arugula.fleet.panes.find((p) => p.key === k)?.info.inbox, `workstation:${pane}`)).toBe(true);
  await done.locator(".followup input").fill("now run the tests");
  await done.locator(".followup button").click();
  await expect(done.locator(".followup-sent")).toHaveText("Sent.");
  await expect.poll(() => capture(page, "workstation", pane)).toContain("(sent through Arugula): now run the tests");
  await fake.close("workstation", pane);
});

test("two agents asking in one project are one card: deny them both", async ({ page }) => {
  await swarm(page);
  await clearRail(page);
  const a = await fake.agentAsks("workstation", "web", "rm -rf build");
  const b = await fake.agentAsks("build-01", "web", "rm -rf build");
  const card = page.locator(`.swarm-card[data-kind="ask"][data-panes~="workstation:${a}"]`);
  await expect(card).toHaveAttribute("data-panes", new RegExp(`build-01:${b}`), { timeout: 20_000 });
  await expect(card.locator(".ch b")).toHaveText("2 agents ask");
  await card.getByRole("button", { name: "Deny all 2" }).click();
  await expect(card).toHaveCount(0);
  await expect.poll(() => capture(page, "workstation", a)).toContain('"behavior":"deny"');
  await expect.poll(() => capture(page, "build-01", b)).toContain('"behavior":"deny"');
  await fake.close("workstation", a);
  await fake.close("build-01", b);
});

test("a question is answered on its card", async ({ page }) => {
  await swarm(page);
  await clearRail(page);
  const pane = await fake.agentQuestion("build-02", "infra");
  const card = page.locator(`.swarm-card[data-bundle][data-panes~="build-02:${pane}"]`);
  await expect(card.locator(".cq")).toHaveText("Which colour do you prefer?", { timeout: 20_000 });
  // AskUserQuestion's card: three questions, answered in place.
  const q = (n: number) => card.locator(`[data-question="${n}"]`);
  await q(0).getByRole("radio", { name: /^Blue/ }).click();
  await q(1).getByRole("checkbox", { name: /Pear/ }).check();
  await q(2).getByRole("radio", { name: /^Cat/ }).click();
  await card.getByRole("button", { name: "Submit" }).click();
  await expect(card).toHaveCount(0);
  await expect(page.locator(`.swarm-card[data-answered][data-panes="build-02:${pane}"] .answered-by`)).toHaveText(/^Answered by /);
  await expect.poll(() => capture(page, "build-02", pane)).toContain("Blue");
  await fake.close("build-02", pane);
});

test("a finished build's card clears by itself", async ({ page }) => {
  test.setTimeout(60_000);
  await swarm(page);
  await clearRail(page);
  const pane = await fake.finish("workstation");
  const card = page.locator(`.swarm-card[data-kind="done"][data-panes~="workstation:${pane}"]`);
  await expect(card).toBeVisible({ timeout: 20_000 });
  await expect(card.locator(".cq")).toContainText("cargo build --release finished after 5s");
  await expect(card).toHaveCount(0, { timeout: 20_000 });
});

test("a full rail: the rest pulse in place, and the rail says how many", async ({ browser }) => {
  const page = await browser.newPage({ viewport: { width: 1000, height: 480 } });
  await swarm(page);
  await clearRail(page);
  await Promise.all([fake.trouble("workstation", 1), fake.trouble("build-01", 1), fake.trouble("build-02", 2)]);
  await expect(page.locator(".swarm-card[data-bundle]")).toHaveCount(2, { timeout: 20_000 });
  await expect(page.locator(".swarm-waiting")).toHaveAttribute("data-waiting", /^[12]$/);
  await clearRail(page);
  await expect(page.locator(".swarm-waiting")).toHaveCount(0);
  await page.close();
});

test("hover peeks, a cluster name zooms, a pane opens in its tab, a notification opens its card", async ({ page }) => {
  await swarm(page);
  await page.locator('[data-g="machine"]').click();
  await page.waitForTimeout(1500);
  // A tile: hover shows its last lines; click opens it, connected for real.
  const key = await page.evaluate(() => window.__arugula.fleet.panes.find((p) => p.host === "build-01" && p.info.kind === "test")!.key);
  const at = async () => (await page.evaluate((k) => (window.__arugula.swarm as { screenOf(k: string): { x: number; y: number } }).screenOf(k), key))!;
  await page.waitForTimeout(1500);
  let pos = await at();
  await page.mouse.move(pos.x, pos.y);
  await expect(page.locator(".swarm-peek pre")).toContainText("test vt::parser::case_", { timeout: 10_000 });
  // The cluster's name zooms in.
  const z0 = await page.evaluate(() => (window.__arugula.swarm as { cam: { tz: number } }).cam.tz);
  const label = (await page.evaluate(() => (window.__arugula.swarm as { labelOf(n: string): { x: number; y: number } }).labelOf("workstation")))!;
  await page.mouse.click(label.x, label.y);
  await expect.poll(() => page.evaluate(() => (window.__arugula.swarm as { cam: { tz: number } }).cam.tz)).toBeGreaterThan(z0);
  await page.locator("[data-fit]").click();
  await page.waitForTimeout(1200);
  pos = await at();
  await page.mouse.click(pos.x, pos.y);
  await expect(page.locator(".swarm")).toHaveCount(0);
  const id = Number(key.split(":")[1]);
  await expect.poll(() => page.evaluate(() => window.__arugula.hosts.current)).toBe("build-01");
  await expect.poll(() => page.evaluate(() => window.__arugula.client.active())).toBe(id);
  // A notification for a pane that wants you opens the swarm at its card.
  const [pane] = await fake.trouble("workstation", 1);
  await page.goto(`/#swarm=${pane}`);
  await expect(page.locator(`.swarm-card.focus[data-panes~="workstation:${pane}"]`)).toBeVisible({ timeout: 20_000 });
  await clearRail(page);
});

test.describe("phone", () => {
  const { viewport, userAgent, deviceScaleFactor, isMobile, hasTouch } = devices["Pixel 7"];
  test.use({ viewport, userAgent, deviceScaleFactor, isMobile, hasTouch });

  test("cluster names never overlap on a phone, by project or by machine", async ({ page }) => {
    await swarm(page);
    // Crowd it: the synthetic fleet adds projects and machines beside the
    // real ones, and a few need you, so labels carry their longest lines.
    const stop = await page.evaluateHandle(() => window.__arugula.swarmFake(300));
    await fake.trouble("build-02", 2);
    type Box = { name: string; x0: number; y0: number; x1: number; y1: number };
    const overlaps = async () => {
      const boxes: Box[] = await page.evaluate(() => (window.__arugula.swarm as unknown as { labelBoxes: Box[] }).labelBoxes);
      const out: string[] = [];
      for (let i = 0; i < boxes.length; i++)
        for (let j = i + 1; j < boxes.length; j++) {
          const [a, b] = [boxes[i], boxes[j]];
          if (a.x0 < b.x1 && b.x0 < a.x1 && a.y0 < b.y1 && b.y0 < a.y1) out.push(`${a.name} × ${b.name}`);
        }
      return { n: boxes.length, out };
    };
    for (const by of ["project", "machine"]) {
      await page.locator(`[data-g="${by}"]`).click();
      await page.locator("[data-fit]").tap();
      // Through the regroup's animation and after it settles.
      for (let i = 0; i < 12; i++) {
        await page.waitForTimeout(400);
        const { n, out } = await overlaps();
        expect(n, `${by}: clusters drawn`).toBeGreaterThanOrEqual(6);
        expect(out, `${by}: overlapping names`).toEqual([]);
      }
    }
    await stop.evaluate((f) => f());
    await clearRail(page);
  });

  test("cards along the bottom: allow and dismiss there; pinch zooms; a tap opens a pane", async ({ page }) => {
    await swarm(page);
    await clearRail(page);
    // The rail is a strip along the bottom.
    const rail = await page.locator(".swarm-rail").boundingBox();
    expect(rail!.y).toBeGreaterThan(viewport.height / 2);
    const ask = await fake.agentAsks("build-01", "infra", "npm publish --dry-run");
    const [failed] = await fake.trouble("workstation", 1);
    const askCard = page.locator(`.swarm-card[data-panes~="build-01:${ask}"]`);
    await expect(askCard).toBeVisible({ timeout: 20_000 });
    await askCard.getByRole("button", { name: "Allow", exact: true }).tap();
    await expect.poll(() => capture(page, "build-01", ask)).toContain('"behavior":"allow"');
    await fake.close("build-01", ask);
    const failCard = page.locator(`.swarm-card[data-panes~="workstation:${failed}"]`);
    await failCard.scrollIntoViewIfNeeded();
    await failCard.getByRole("button", { name: "Dismiss" }).tap();
    await expect(failCard).toHaveCount(0);
    // Pinch: two fingers apart zoom in.
    const z0 = await page.evaluate(() => (window.__arugula.swarm as { cam: { tz: number } }).cam.tz);
    await page.evaluate(() => {
      const cv = document.querySelector<HTMLCanvasElement>(".swarm-field")!;
      const ev = (type: string, id: number, x: number) =>
        cv.dispatchEvent(new PointerEvent(type, { pointerId: id, clientX: x, clientY: 300, bubbles: true, pointerType: "touch" }));
      ev("pointerdown", 1, 180);
      ev("pointerdown", 2, 210);
      ev("pointermove", 1, 120);
      ev("pointermove", 2, 290);
      ev("pointerup", 1, 120);
      ev("pointerup", 2, 290);
    });
    await expect.poll(() => page.evaluate(() => (window.__arugula.swarm as { cam: { tz: number } }).cam.tz)).toBeGreaterThan(z0 * 2);
    await page.locator("[data-fit]").tap();
    await page.waitForTimeout(1500);
    // A tap on a tile opens it.
    const key = await page.evaluate(() => window.__arugula.fleet.panes.find((p) => p.host === "workstation" && p.info.kind === "server")!.key);
    const pos = (await page.evaluate((k) => (window.__arugula.swarm as { screenOf(k: string): { x: number; y: number } }).screenOf(k), key))!;
    await page.touchscreen.tap(pos.x, pos.y);
    await expect(page.locator(".swarm")).toHaveCount(0);
    await expect.poll(() => page.evaluate(() => window.__arugula.client.active())).toBe(Number(key.split(":")[1]));
  });
});
