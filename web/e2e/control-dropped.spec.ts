// #325: a machine control dropped says so on its page: a banner with Join
// again, which opens Getting started's join; and the host menu says
// whether and where it's joined. The test daemon isn't joined to any
// control, so these answer `control_state` themselves (on the real
// `/api/host`); the daemon's side is
// crates/daemon/tests/integration/control_state.rs. In the desktop app a
// drop opens the join by itself (#326).

import { expect, test, type Page } from "@playwright/test";
import { reset } from "./helpers";

const CONTROL = "https://control.arugula.io";
const dropped = {
  state: "dropped",
  url: CONTROL,
  kind: "team",
  name: "arugula",
  connected: false,
  said: "not an enrolled daemon (left, or revoked?)",
  dropped_ms: Date.UTC(2026, 9, 3, 6, 21),
};

/** `/api/host` and `/api/setup` as the daemon says them, with this state
 * (and `/api/setup`'s control part with `extra`). */
async function standing(page: Page, state: object, extra: object = {}) {
  await page.route("**/api/host", async (r) => {
    const res = await r.fetch();
    await r.fulfill({ response: res, json: { ...(await res.json()), control: CONTROL, control_state: state } });
  });
  await page.route(/\/api\/setup(\?part=control)?$/, async (r) => {
    const res = await r.fetch();
    const body = await res.json();
    await r.fulfill({ response: res, json: { ...body, control: { ...body.control, url: CONTROL, state, ...extra } } });
  });
}

// The page keeps polling /api/setup: a fetch still routed when a test ends
// isn't a failure.
test.afterEach(({ page }) => page.unrouteAll({ behavior: "ignoreErrors" }));

test("dropped by control: a banner, the host menu's line, and Join again", async ({ page }) => {
  await standing(page, dropped);
  await reset(page);
  const banner = page.locator("[data-control-dropped]");
  await expect(banner).toContainText("This machine is no longer in the team arugula on control.arugula.io");
  await expect(banner).toContainText("not an enrolled daemon (left, or revoked?)");

  // The host menu: what happened, and the way back.
  await page.locator(".host-button").click();
  await expect(page.getByText("Dropped by control: no longer in the team arugula on control.arugula.io")).toBeVisible();
  await expect(page.getByText("Join again…")).toBeVisible();
  await page.keyboard.press("Escape");

  // Join again: Getting started's cloud step, which joins the same control again.
  await banner.locator("[data-control-rejoin]").click();
  const start = page.getByRole("dialog", { name: "Getting started" });
  await expect(start.locator("[data-getting-started]")).toHaveAttribute("data-step", "cloud");
  await expect(start.locator("[data-start-dropped]")).toContainText("control dropped it");
  await expect(start.locator("[data-start-connect]")).toHaveText("Join control.arugula.io again");
  await expect(start.locator("[data-start-joined]")).toBeHidden();
  await start.getByRole("button", { name: "Close" }).click();

  // Not now: gone for this tab.
  await banner.locator("[data-control-dropped-hide]").click();
  await expect(banner).toBeHidden();
});

// #330: control removed the key, so the daemon made a new one and asks to
// join again by itself: the banner has the code, and Getting started the
// code to approve and why it has a new key.
test("a removed key: the join it asked for, in the banner and Getting started", async ({ page }) => {
  const code = "ABCDE-FGHIJ";
  const approve = `${CONTROL}/#join=${code}`;
  await standing(
    page,
    { ...dropped, kind: "account", name: "lex00", said: "this machine was removed from its account", code, approve },
    {
      pending: { code, approve, expires_ms: Date.now() + 900_000 },
      removed: { said: "this machine was removed from its account", by: "laptop", old_key: "1111-2222", kept: "/state/daemon.key.removed-1", new_key: "3333-4444" },
    },
  );
  await reset(page);
  const banner = page.locator("[data-control-dropped]");
  await expect(banner).toContainText("This machine is no longer in lex00's account on control.arugula.io");
  await expect(banner.locator("[data-control-dropped-code]")).toContainText(code);
  await banner.locator("[data-control-rejoin]").click();
  const start = page.getByRole("dialog", { name: "Getting started" });
  await expect(start.locator("[data-start-pending] [data-start-code]")).toHaveText(code);
  await expect(start.locator("[data-start-approve]")).toHaveAttribute("href", approve);
  await expect(start.locator("[data-start-dropped]")).toContainText("it made a new one (3333-4444)");
  await start.getByRole("button", { name: "Close" }).click();
});

test("in the app, a drop opens the join itself, once, leading with the machine (#326)", async ({ page }) => {
  await page.addInitScript(() => Object.assign(window, { __arugulaApp: { name: "Arugula app on test-mac" } }));
  await standing(page, dropped);
  await reset(page);
  const start = page.getByRole("dialog", { name: "Getting started" });
  await expect(start.locator("[data-getting-started]")).toHaveAttribute("data-step", "cloud");
  await expect(start.locator("[data-start-cloud-title]")).toContainText("back in your account or team");
  // One approval, of the machine's code, on a device the person uses.
  await expect(start.locator("[data-start-join-how]")).toContainText("that one approval joins it");
  await start.getByRole("button", { name: "Close" }).click();
  // Once per drop: not again on a reload.
  await page.reload();
  await expect(page.locator("[data-control-dropped]")).toBeVisible();
  await expect(start).toBeHidden();
});

test("joined and connected: no banner, and the host menu says where", async ({ page }) => {
  await standing(page, { state: "joined", url: CONTROL, kind: "account", name: "lex00", connected: true, seen_ms: Date.now() });
  await reset(page);
  await page.locator(".host-button").click();
  await expect(page.getByText("In lex00's account on control.arugula.io: connected")).toBeVisible();
  await expect(page.locator("[data-control-dropped]")).toBeHidden();
});

test("the real daemon: not joined, said only to its owner", async ({ page }) => {
  await reset(page);
  const host = await page.evaluate(() => fetch("/api/host").then((r) => r.json()));
  expect(host.control_state).toMatchObject({ state: "not_joined", connected: false });
  const setup = await page.evaluate(() => fetch("/api/setup?part=control").then((r) => r.json()));
  expect(setup.control.state.state).toBe("not_joined");
  await expect(page.locator("[data-control-dropped]")).toBeHidden();
});
