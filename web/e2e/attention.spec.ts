// M24: attention reasons and actions in the client. A command that fails
// in a tab nobody is looking at says why on its tab and pane; dismissing
// it on one client clears it on the other; and its push notification
// offers Dismiss (delivered through the DevTools protocol, as S18 did).

import { expect, test, type Page } from "@playwright/test";
import { open, reset, closeContexts } from "./helpers";
import type { PaneId } from "../src/proto";

test.afterAll(closeContexts);

/** A shell in a new tab that isn't shown (so nobody is looking at it). */
async function hiddenShell(page: Page): Promise<PaneId> {
  return page.evaluate(async () => {
    const res = await fetch("/api/run", { method: "POST", headers: { "Content-Type": "application/json" }, body: "{}" });
    return (await res.json()).pane as number;
  });
}

async function send(page: Page, pane: PaneId, text: string) {
  await page.evaluate(
    async ({ pane, text }) => {
      await fetch(`/api/panes/${pane}/send`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ text, enter: true }),
      });
    },
    { pane, text },
  );
}

const reasonOf = (page: Page, pane: PaneId) =>
  page.evaluate((p) => window.__illogical.client.info(p)?.reason ?? null, pane);

test("a failure says why, and dismissing it on one client clears it on the other", async ({ browser }) => {
  const a = await browser.newPage();
  const b = await browser.newPage();
  await reset(a);
  await open(b);
  const shown = await a.evaluate(() => window.__illogical.client.tab!);
  const pane = await hiddenShell(a);
  await expect.poll(() => a.evaluate((p) => !!window.__illogical.client.info(p)?.running, pane)).toBe(true);
  // a made it, so a shows it: back to the first tab.
  await a.evaluate((t) => window.__illogical.client.selectTab(t), shown);
  await a.waitForTimeout(500);
  await send(a, pane, "cargo() { sleep 3.2; echo 'test result: FAILED'; return 101; }");
  await send(a, pane, "cargo test");
  await expect.poll(() => reasonOf(a, pane), { timeout: 15_000 }).toMatchObject({ kind: "failed", exit: 101, command: "cargo test" });
  const r = (await reasonOf(b, pane))!;
  expect(r.headline).toMatch(/^cargo test failed \(exit 101\) after 3s/);
  expect(r.bundle).toBe("failed:here");

  // Its tab says so, with the headline as the title.
  const badge = a.locator(".tab .att.failed");
  await expect(badge).toHaveCount(1);
  await expect(badge).toHaveAttribute("title", r.headline);

  // Dismissed on b: gone on a.
  expect(await b.evaluate((p) => window.__illogical.client.act({ action: "dismiss", pane: p }), pane)).toBe(true);
  await expect.poll(() => reasonOf(a, pane)).toBeNull();
  await expect(a.locator(".tab .att")).toHaveCount(0);
  await a.close();
  await b.close();
});

test("a push for a failure offers Dismiss", async ({ browser }) => {
  const context = await browser.newContext();
  await context.grantPermissions(["notifications"]);
  const page = await context.newPage();
  await open(page);
  await expect.poll(() => page.evaluate(async () => !!(await navigator.serviceWorker.getRegistration()))).toBe(true);
  await page.evaluate(() => navigator.serviceWorker.ready);
  const cdp = await context.newCDPSession(page);
  await cdp.send("ServiceWorker.enable");
  const registrationId = await new Promise<string>((resolve) => {
    cdp.on("ServiceWorker.workerRegistrationUpdated", (e) => {
      const r = e.registrations.find((x) => !x.isDeleted);
      if (r) resolve(r.registrationId);
    });
  });
  // What the daemon sends for a failure (see crates/daemon/tests/attention.rs).
  const payload = {
    title: "Failed",
    body: "cargo test failed (exit 101) after 3s",
    pane: 7,
    tag: "pane-7",
    reason: { kind: "failed", actions: ["rerun", "dismiss"], bundle: "failed:here" },
  };
  await cdp.send("ServiceWorker.deliverPushMessage", {
    origin: new URL(page.url()).origin,
    registrationId,
    data: JSON.stringify(payload),
  });
  const shown = () =>
    page.evaluate(async () => {
      const reg = await navigator.serviceWorker.getRegistration();
      const ns = (await reg?.getNotifications()) ?? [];
      return ns.map((n) => ({ title: n.title, body: n.body, actions: (n as unknown as { actions: { action: string }[] }).actions.map((x) => x.action) }));
    });
  // M11: Rerun from the notification, as from the phone's Needs you.
  await expect.poll(shown).toEqual([{ title: "Failed", body: payload.body, actions: ["rerun", "dismiss"] }]);
  await context.close();
});
