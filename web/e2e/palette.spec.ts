// #139: the command palette in Chrome, on a desktop and a phone. The
// checks are in palette-steps.ts, shared with palette.webkit.spec.ts.

import { devices, test } from "@playwright/test";
import { desktop, phone } from "./palette-steps";

test("the chord opens the palette from a terminal; it runs every pane-menu action", async ({ page }) => {
  await desktop(page);
});

test.describe("on a phone", () => {
  const { viewport, userAgent, deviceScaleFactor, isMobile, hasTouch } = devices["Pixel 7"];
  test.use({ viewport, userAgent, deviceScaleFactor, isMobile, hasTouch });
  test("a full-height sheet from the sheet's Commands button", async ({ page }) => {
    await phone(page);
  });
});
