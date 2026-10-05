// M6's block contract in the client: a browser block opens beside a
// terminal, draws its page in a frame (or a card for sites that refuse to
// be framed), and moves and closes like any pane.

import { createServer, type Server } from "node:http";
import type { AddressInfo } from "node:net";
import { expect, test } from "@playwright/test";
import { menu, paneEl, panes, reset, closeContexts } from "./helpers";

test.afterAll(closeContexts);

let server: Server;
let base = "";
test.beforeAll(async () => {
  server = createServer((req, res) => {
    if (req.url === "/no-frame") res.setHeader("X-Frame-Options", "DENY");
    res.setHeader("Content-Type", "text/html");
    res.end(`<html><head><title>Page ${req.url}</title></head><body><h1 id="h">hello from ${req.url}</h1></body></html>`);
  });
  await new Promise<void>((r) => server.listen(0, "127.0.0.1", r));
  base = `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
});
test.afterAll(() => server.close());

test("a browser block opens beside a terminal, frames its page, and closes", async ({ page }) => {
  await reset(page);
  const [term] = await panes(page);
  await menu(page, paneEl(page, term), "Open a web page…");
  await page.locator(".prompt input").fill(`${base}/one`);
  await page.keyboard.press("Enter");
  await expect.poll(async () => (await panes(page)).length).toBe(2);
  const block = (await panes(page)).find((p) => p !== term)!;
  expect(await page.evaluate((b) => window.__illogical.client.info(b)?.type, block)).toBe("browser");

  const frame = page.frameLocator(`[data-pane="${block}"] iframe`);
  await expect(frame.locator("#h")).toHaveText("hello from /one");
  // The tab is named after the page once that block is active.
  await page.evaluate((b) => window.__illogical.client.setActive(b), block);
  await expect(page.locator(".tab.selected .tab-label")).toHaveText("Page /one");

  // A site that refuses framing gets a card instead.
  const input = paneEl(page, block).locator(".browser-url input");
  await input.fill(`${base}/no-frame`);
  await input.press("Enter");
  await expect(paneEl(page, block).locator(".browser-card")).toContainText("doesn't allow being shown");
  await expect(paneEl(page, block).getByRole("link", { name: "Open in new tab" })).toHaveAttribute("href", `${base}/no-frame`);
  await paneEl(page, block).getByTitle("Back").click();
  await expect(frame.locator("#h")).toHaveText("hello from /one");

  await page.evaluate((b) => window.__illogical.client.intent({ op: "close_pane", pane: b }), block);
  await expect.poll(() => panes(page)).toEqual([term]);
});
