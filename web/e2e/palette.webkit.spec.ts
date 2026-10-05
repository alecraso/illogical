// #139: the command palette in WebKit (Safari's engine, and the desktop
// app's on macOS), with Cmd as well as Ctrl, and on an iPhone. The checks
// are in palette-steps.ts, shared with palette.spec.ts.

import { devices, test } from "@playwright/test";
import { desktop, phone } from "./palette-steps";

// Playwright's WebKit stops loading the page once the service worker is
// registered (real Safari doesn't), so none here.
test.use({ serviceWorkers: "block" });

test("Ctrl+Shift+P opens the palette from a terminal; it runs every pane-menu action", async ({ page }) => {
  await desktop(page, "Control");
});

test("Cmd+Shift+P does too", async ({ page }) => {
  await desktop(page, "Meta");
});

test.describe("on an iPhone", () => {
  const { viewport, userAgent, deviceScaleFactor, isMobile, hasTouch } = devices["iPhone 13"];
  test.use({ viewport, userAgent, deviceScaleFactor, isMobile, hasTouch });
  test("a full-height sheet from the sheet's Commands button", async ({ page }) => {
    await phone(page);
  });
});
