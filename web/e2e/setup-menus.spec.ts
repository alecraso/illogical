// #171, #180: a machine set up for nothing optional (no --block-listen, no
// wisp, no Fountain login or runner, no studio) offers nothing that would
// fail for setup nobody mentioned. VM, sandbox, Fountain and studio items
// aren't in its menus; Open a port… and Open in editor say how to turn
// block sites on, with a link, instead of failing. A Fountain login puts
// Fountain's item back the next time a menu opens.

import { spawn, type ChildProcess } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test, type Page } from "@playwright/test";
import { paneEl, panes, reset } from "./helpers";
import { ANY, daemonPort } from "./ports";
import { labs } from "./labs";

let APP = "";
test.use({ baseURL: async ({}, use) => use(APP) });
test.describe.configure({ mode: "serial" });

let state = "";
let daemon: ChildProcess | undefined;

test.beforeAll(async () => {
  state = mkdtempSync(join(tmpdir(), "ilg-e2e-setup-menus-"));
  daemon = spawn("../target/debug/arugulad", ["--listen", ANY, "--shell", "bash --norc --noprofile"], {
    stdio: "ignore",
    env: {
      ...process.env,
      ARUGULA_STATE_DIR: labs(state),
      ARUGULA_WISP_TOKEN_FILE: "/nonexistent",
      ARUGULA_FOUNTAIN_CREDENTIALS: join(state, "no-fountain-credentials"),
      ARUGULA_FOUNTAIN_UNIT_FILE: join(state, "no-fountain-runner.service"),
    },
  });
  APP = `http://127.0.0.1:${await daemonPort(state, daemon)}`;
});

test.afterAll(async () => {
  daemon?.kill("SIGKILL");
  if (state) rmSync(state, { recursive: true, force: true });
});

const items = async (page: Page) => (await page.getByRole("menuitem").allInnerTexts()).map((s) => s.trim());

test("menus offer only what this machine is set up for", async ({ page }) => {
  await reset(page);
  expect(await page.evaluate(() => fetch("/api/host").then((r) => r.json()).then((h) => h.features))).toEqual({
    labs: true,
    blocks: false,
    vms: false,
    fountain: false,
    studio: false,
    threads: true,
    calls: true,
  });
  const [term] = await panes(page);
  const hidden = ["New VM pane on the right", "New VM tab", "Sandboxes…", "Fountain agents…", "Fountain runner…", "Open a studio app…"];

  // A pane's menu.
  await paneEl(page, term).click({ button: "right", position: { x: 60, y: 60 } });
  await expect(page.getByRole("menuitem", { name: "Split right" })).toBeVisible();
  let got = await items(page);
  for (const h of hidden) expect(got).not.toContain(h);
  expect(got).toContain("Open a port…");
  expect(got).toContain("Open in editor");
  await page.keyboard.press("Escape");

  // The session menu, a tab's and the + button's.
  for (const open of [
    () => page.locator(".session-button").click(),
    () => page.locator(".tab").first().click({ button: "right" }),
    () => page.locator(".new-tab").click({ button: "right" }),
  ]) {
    await open();
    await expect(page.getByRole("menuitem").first()).toBeVisible();
    got = await items(page);
    for (const h of hidden) expect(got).not.toContain(h);
    await page.keyboard.press("Escape");
  }

  // Sharing says nothing about VMs it has no way to make (#208).
  await page.locator(".session-button").click();
  await page.getByRole("menuitem", { name: "Share session…" }).click();
  await expect(page.locator("[data-share-note]")).toHaveText(
    "People you share with see its panes but can't open their own here, and can't type on this machine unless you trust them with a pane.",
  );
});

test("Open a port… and Open in editor say how to turn block sites on", async ({ page }) => {
  await reset(page);
  const [term] = await panes(page);
  for (const [item, anchor] of [
    ["Open a port…", "#browser-blocks-on-ports"],
    ["Open in editor", "#editor-blocks"],
  ]) {
    await paneEl(page, term).click({ button: "right", position: { x: 60, y: 60 } });
    await page.getByRole("menuitem", { name: item }).click();
    const note = page.locator("[data-setup-notice]");
    await expect(note).toContainText("--block-listen");
    await expect(note.getByRole("link")).toHaveAttribute("href", new RegExp(`docs/advanced\\.md${anchor}$`));
    // No port asked for, no error, nothing opened.
    await expect(page.locator(".prompt input")).toHaveCount(0);
    await page.getByRole("button", { name: "OK" }).click();
    await expect(note).toHaveCount(0);
    expect(await panes(page)).toEqual([term]);
  }
  expect(await page.evaluate(() => window.__arugula.client.error)).toBeNull();
});

test("a Fountain login made since the page loaded shows the next time a menu opens", async ({ page }) => {
  await reset(page);
  const [term] = await panes(page);
  // As `fountain auth login` leaves it.
  writeFileSync(join(state, "no-fountain-credentials"), '[default]\napi_key = "ftn_test_e2e"\n');
  await paneEl(page, term).click({ button: "right", position: { x: 60, y: 60 } });
  await expect(page.getByRole("menuitem", { name: "Fountain agents…" })).toBeVisible();
  await expect(page.getByRole("menuitem", { name: "Fountain runner…" })).toHaveCount(0);
});
