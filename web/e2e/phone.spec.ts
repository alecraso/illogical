// M1 on a phone: one pane at a time, a sheet to switch, and a key bar.

import { createServer } from "node:http";
import type { AddressInfo } from "node:net";
import { devices, expect, test } from "@playwright/test";
import { active, ready, reset, text, type } from "./helpers";

const { viewport, userAgent, deviceScaleFactor, isMobile, hasTouch } = devices["Pixel 7"];
test.use({ viewport, userAgent, deviceScaleFactor, isMobile, hasTouch });

test("one pane at a time, switch from the sheet, extra keys work", async ({ page }) => {
  await reset(page);
  // A split made on the phone.
  await page.locator(".sheet-button").click();
  await page.getByRole("button", { name: "Split pane" }).click();
  await expect.poll(() => page.evaluate(() => window.__arugula.client.state!.panes.length)).toBe(2);
  // Only the active pane is drawn, filling the screen.
  await expect.poll(() => page.locator(".pane").count()).toBe(1);
  const shown = await active(page);
  await ready(page, shown);
  const fill = await page.locator(".pane").boundingBox();
  expect(fill!.width).toBeGreaterThan(380);

  // Ctrl from the key bar, then "c", interrupts a running command.
  await type(page, shown, "sleep 100\n");
  await page.getByRole("button", { name: "Ctrl" }).click();
  await expect(page.getByRole("button", { name: "Ctrl" })).toHaveAttribute("aria-pressed", "true");
  await page.keyboard.type("c");
  await expect(page.getByRole("button", { name: "Ctrl" })).toHaveAttribute("aria-pressed", "false");
  await page.keyboard.type("echo back-$((8+1))\n");
  await expect.poll(() => text(page, shown)).toContain("back-9");
  // Drawn on screen, not just in the terminal's buffer.
  await expect(page.locator(".pane .xterm-rows")).toContainText("back-9");

  // Up arrow from the key bar recalls the last command.
  await page.getByRole("button", { name: "↑" }).click();
  await page.getByRole("button", { name: "Tab" }).click();
  await page.keyboard.press("Enter");
  await expect.poll(async () => (await text(page, shown)).match(/back-9/g)?.length).toBe(2);

  // The sheet lists both panes; pick the other one.
  await page.locator(".sheet-button").click();
  const other = page.locator(".sheet-item.sheet-pane:not(.current)");
  await expect(other).toHaveCount(1);
  await other.click();
  await expect.poll(() => active(page)).not.toBe(shown);
  await expect.poll(() => page.locator(".pane").count()).toBe(1);
});

// #95: the README's phone step, on a phone: Notify this device is in the
// sheet, turns on, and the daemon sends this device a test notification.
// Headless Chrome has no push service, so the browser's subscription is a
// stand-in whose endpoint is ours: what the daemon sends lands here.
test("notify this device from the sheet", async ({ page, context }) => {
  const got: { encoding?: string; bytes: number }[] = [];
  const service = createServer((req, res) => {
    let bytes = 0;
    req.on("data", (c: Buffer) => (bytes += c.length));
    req.on("end", () => {
      got.push({ encoding: req.headers["content-encoding"] as string | undefined, bytes });
      res.writeHead(201).end();
    });
  });
  await new Promise<void>((r) => service.listen(0, "127.0.0.1", r));
  const endpoint = `http://127.0.0.1:${(service.address() as AddressInfo).port}/push/e2e`;
  await page.addInitScript((endpoint) => {
    let sub: PushSubscription | null = null;
    const b64 = (b: ArrayBuffer) => btoa(String.fromCharCode(...new Uint8Array(b))).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
    PushManager.prototype.subscribe = async function () {
      const pair = (await crypto.subtle.generateKey({ name: "ECDH", namedCurve: "P-256" }, true, ["deriveBits"])) as CryptoKeyPair;
      const keys = { p256dh: b64(await crypto.subtle.exportKey("raw", pair.publicKey)), auth: b64(crypto.getRandomValues(new Uint8Array(16)).buffer) };
      sub = { endpoint, toJSON: () => ({ endpoint, keys }), unsubscribe: async () => ((sub = null), true) } as unknown as PushSubscription;
      return sub;
    };
    PushManager.prototype.getSubscription = async () => sub;
  }, endpoint);
  await context.grantPermissions(["notifications"]);
  await reset(page);
  await page.locator(".sheet-button").click();
  const notify = page.getByRole("button", { name: "Notify this device" });
  await expect(notify).toHaveAttribute("aria-pressed", "false");
  await notify.click();
  await expect(notify).toHaveAttribute("aria-pressed", "true");
  await expect.poll(() => got.length).toBeGreaterThan(0);
  expect(got[0].encoding).toBe("aes128gcm");
  expect(got[0].bytes).toBeGreaterThan(0);
  // And off again.
  await notify.click();
  await expect(notify).toHaveAttribute("aria-pressed", "false");
  service.close();
});
