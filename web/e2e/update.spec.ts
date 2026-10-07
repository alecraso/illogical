// #176: a newer release shows as a chip in the top bar, with the command
// that updates this install, or (#391) Update now where the daemon updates
// itself. The daemon's check is off for a build run
// from target/, so these answer `/api/update` themselves.

import { expect, test, type Page } from "@playwright/test";
import { reset } from "./helpers";

const status = (over: Record<string, unknown> = {}) => ({
  current: "0.16.0",
  latest: "0.17.0",
  newer: true,
  enabled: true,
  kind: "script",
  command: "curl -fsSL https://illogical.widgets.wtf/install.sh | sh",
  url: "https://github.com/arugula-salad/illogical/releases/tag/v0.17.0",
  ...over,
});

async function serve(page: Page, body: object) {
  await page.route("**/api/update", (r) => r.fulfill({ json: body }));
}

test("a newer release: the chip, the command, and Not now", async ({ page }) => {
  await serve(page, status());
  await reset(page);
  const chip = page.locator("[data-update-chip]");
  await expect(chip).toHaveText("Update 0.17.0");
  await chip.click();
  const pop = page.getByRole("dialog", { name: "Update Arugula" });
  await expect(pop).toContainText("this daemon is 0.16.0");
  await expect(pop.locator("[data-update-command]")).toHaveText("curl -fsSL https://illogical.widgets.wtf/install.sh | sh");
  await expect(pop.getByRole("link", { name: "What's new" })).toHaveAttribute("href", /\/tag\/v0\.17\.0$/);

  // Dismissed for this version: gone, and still gone after a reload.
  await pop.locator("[data-update-dismiss]").click();
  await expect(chip).toBeHidden();
  await page.reload();
  await expect(page.locator(".session-button")).toBeVisible();
  await expect(chip).toBeHidden();
  await page.evaluate(() => localStorage.removeItem("arugula.update.dismissed"));
});

test("Homebrew's command, the app's, and nothing when up to date", async ({ page }) => {
  await serve(page, status({ kind: "brew", command: "brew upgrade arugula && arugulad install" }));
  await reset(page);
  await page.locator("[data-update-chip]").click();
  await expect(page.locator("[data-update-command]")).toHaveText("brew upgrade arugula && arugulad install");

  await page.unroute("**/api/update");
  await serve(page, status({ kind: "app" }));
  await page.reload();
  await page.locator("[data-update-chip]").click();
  await expect(page.locator("[data-update-command]")).toHaveText("curl -fsSL https://illogical.widgets.wtf/install.sh | sh");
  await expect(page.locator("[data-update-now]")).toHaveCount(0);

  await page.unroute("**/api/update");
  await serve(page, status({ latest: "0.16.0", newer: false }));
  await page.reload();
  await expect(page.locator(".session-button")).toBeVisible();
  await expect(page.locator("[data-update-chip]")).toBeHidden();
});

test("Update now: progress, then a failure says why and offers the command", async ({ page }) => {
  let now: object = status({ apply: true });
  await page.route("**/api/update", (r) => r.fulfill({ json: now }));
  let asked = 0;
  await page.route("**/api/update/apply", (r) => {
    asked++;
    now = status({ apply: true, applying: { to: "0.17.0", stage: "installing" } });
    return r.fulfill({ json: { to: "0.17.0", stage: "downloading" } });
  });
  await reset(page);
  await page.locator("[data-update-chip]").click();
  await expect(page.locator("[data-update-command]")).toHaveCount(0);
  await page.locator("[data-update-now]").click();
  await expect(page.locator("[data-update-progress]")).toHaveText("Installing it…");
  expect(asked).toBe(1);

  now = status({
    apply: true,
    applying: { to: "0.17.0", stage: "failed", error: "arugula-0.17.0.tar.gz doesn't match SHA256SUMS" },
  });
  await expect(page.locator("[data-update-error]")).toContainText("doesn't match SHA256SUMS");
  await expect(page.locator("[data-update-command]")).toBeVisible();
  await expect(page.locator("[data-update-now]")).toBeVisible();
});

test("Update now: the page reloads once the new daemon answers", async ({ page }) => {
  let now: object = status({ apply: true });
  await page.route("**/api/update", (r) => r.fulfill({ json: now }));
  await page.route("**/api/update/apply", (r) => r.fulfill({ json: { to: "0.17.0", stage: "downloading" } }));
  await reset(page);
  await page.locator("[data-update-chip]").click();
  await page.locator("[data-update-now]").click();
  // The daemon restarting: nothing answers for a moment.
  now = { gone: true };
  await page.unroute("**/api/update");
  await page.route("**/api/update", (r) => r.abort());
  await expect(page.locator("[data-update-progress]")).toHaveText("Restarting the daemon…");
  // Back, as 0.17.0: the page reloads (and the chip goes, as it's current).
  await page.unroute("**/api/update");
  await page.route("**/api/update", (r) => r.fulfill({ json: status({ current: "0.17.0", newer: false }) }));
  await page.waitForEvent("load");
  await expect(page.locator(".session-button")).toBeVisible();
  await expect(page.locator("[data-update-chip]")).toBeHidden();
});

test("the real daemon answers: off for a build run from target/", async ({ page }) => {
  await reset(page);
  const got = await page.evaluate(() => fetch("/api/update").then((r) => r.json()));
  expect(got).toMatchObject({ enabled: false, newer: false, kind: "source", apply: false });
  await expect(page.locator("[data-update-chip]")).toBeHidden();
  const r = await page.evaluate(() => fetch("/api/update/apply", { method: "POST" }).then((r) => r.status));
  expect(r).toBe(409);
});

test("any open page reloads when its daemon comes back as another version (#419)", async ({ page }) => {
  // The first connection passes through; the next one's hello says a new
  // version, as the daemon would after an update restarted it.
  let n = 0;
  const socks: { close: () => Promise<void> }[] = [];
  await page.routeWebSocket(/\/ws(\?|$)/, (ws) => {
    const server = ws.connectToServer();
    const conn = ++n;
    socks.push({ close: () => ws.close() });
    server.onMessage((m) => {
      if (conn > 1 && typeof m === "string" && m.includes('"type":"hello"')) {
        const msg = JSON.parse(m) as { version: string };
        msg.version = "99.0.0";
        return ws.send(JSON.stringify(msg));
      }
      ws.send(m);
    });
  });
  await reset(page);
  await expect(page.locator(".session-button")).toBeVisible();
  const reloaded = page.waitForEvent("load");
  await socks[0].close();
  await reloaded;
  expect(n).toBeGreaterThan(1);
});
