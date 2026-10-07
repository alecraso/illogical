import { readFileSync } from "node:fs";
import { homedir } from "node:os";
import { devices, expect, test } from "@playwright/test";
import { reset } from "./helpers";

// VM panes need a wispd (and its token) on this host; elsewhere these skip.
const WISP = process.env.ARUGULA_WISP_URL ?? "http://127.0.0.1:7788";
const token = (() => {
  try {
    return readFileSync(process.env.ARUGULA_WISP_TOKEN_FILE ?? `${homedir()}/.local/share/wisp/token`, "utf8").trim();
  } catch {
    return "";
  }
})();

async function sprites(prefix: string): Promise<string[]> {
  const r = await fetch(`${WISP}/v1/sprites?prefix=${prefix}`, { headers: { Authorization: `Bearer ${token}` } });
  return ((await r.json()).sprites ?? []).map((s: { name: string }) => s.name);
}

// A test that fails midway leaves its machine behind (the daemon is killed,
// not restarted, so its sweep never runs): delete what this file made.
const made: string[] = [];
test.afterAll(async () => {
  for (const name of made) await fetch(`${WISP}/v1/sprites/${name}`, { method: "DELETE", headers: { Authorization: `Bearer ${token}` } });
});

test.describe("phone", () => {
  const { viewport, userAgent, deviceScaleFactor, isMobile, hasTouch } = devices["Pixel 7"];
  test.use({ viewport, userAgent, deviceScaleFactor, isMobile, hasTouch });

  test("a VM tab from the sheet runs on its own machine, which goes when it closes", async ({ page }) => {
    test.skip(!token, "no wisp token on this host");
    test.setTimeout(60_000);
    await reset(page);

    await page.locator(".sheet-button").click();
    await page.getByRole("button", { name: "New VM tab" }).click();
    const pane = await expect
      .poll(() => page.evaluate(() => window.__arugula.client.state!.panes.find((p) => p.host !== null)?.id ?? null))
      .not.toBeNull()
      .then(() => page.evaluate(() => window.__arugula.client.state!.panes.find((p) => p.host !== null)!.id));
    const sprite = await page.evaluate((p) => window.__arugula.client.machine(p)!.sprite, pane);
    made.push(sprite);
    // A VM tab: the tab carries the badge (here, the header), not its pane.
    await expect(page.locator(".phone-bar .host-tag")).toHaveText("VM");
    await expect(page.locator(`.pane[data-pane="${pane}"] .host-badge`)).toHaveCount(0);
    await expect(page.locator(".sheet-button")).toBeVisible();

    // The shell is the VM's, with the integration loaded.
    await expect.poll(() => page.evaluate((p) => window.__arugula.client.info(p)?.cwd ?? null, pane)).toBe("/home/sprite");
    await page.locator(`.pane[data-pane="${pane}"]`).click();
    await page.keyboard.type("echo vm-$((40+2)) on $(hostname)\r");
    await expect.poll(() => page.evaluate((p) => window.__arugula.screen(p), pane)).toContain(`vm-42 on ${sprite}`);
    expect(await sprites(sprite)).toEqual([sprite]);

    // Close it from the sheet: the machine is deleted, the session kept.
    await page.locator(".sheet-button").click();
    await page.getByRole("button", { name: "Close pane" }).click();
    await expect.poll(() => page.evaluate(() => window.__arugula.client.state!.machines.length)).toBe(0);
    await expect.poll(() => sprites(sprite)).toEqual([]);
    const tail = await page.evaluate((p) => fetch(`/api/panes/${p}/tail?from=0&text=1`).then((r) => r.text()), pane);
    expect(tail).toContain(`vm-42 on ${sprite}`);
  });
});
