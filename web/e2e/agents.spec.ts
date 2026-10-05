// M6b in the client: an agent block started from the pane menu (desktop)
// and from the sheet (phone), with its permission card, tool-call output in
// a read-only terminal, and the composer. The agent is the scripted fake
// ACP server the daemon's tests use, so nothing here costs anything.

import { rmSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { devices, expect, test, type Page } from "@playwright/test";
import { menu, paneEl, panes, reset, text } from "./helpers";

const fake = fileURLToPath(new URL("../../crates/daemon/tests/fake_acp.py", import.meta.url));

async function startFake(page: Page, prompt: string) {
  const dialog = page.getByRole("dialog", { name: "Start an agent" });
  await expect(dialog).toBeVisible();
  await dialog.locator("select[name=agent]").selectOption("acp");
  await dialog.locator("input[name=acp]").fill(`python3 ${fake}`);
  await dialog.locator("textarea[name=prompt]").fill(prompt);
  await dialog.getByRole("button", { name: "Start" }).click();
  await expect(dialog).toBeHidden();
}

const agentBlock = (page: Page) =>
  page.evaluate(() => window.__illogical.client.state!.panes.find((p) => p.type === "agent")?.id ?? null);

test.describe("desktop", () => {
  test("an agent block asks, runs, shows its output, and takes messages", async ({ page }) => {
    await reset(page);
    const [term] = await panes(page);
    await menu(page, paneEl(page, term), "Start an agent…");
    await startFake(page, "run ls --color");
    await expect.poll(() => agentBlock(page)).not.toBeNull();
    const id = (await agentBlock(page))!;
    expect(await panes(page)).toEqual([term, id]);
    const block = paneEl(page, id);

    // The permission card; approving runs it.
    const card = block.getByRole("alertdialog", { name: "Allow ls --color?" });
    await expect(card).toContainText("Bash wants to run");
    await expect(block.locator(".agent-status")).toHaveText("Needs you");
    await card.getByRole("button", { name: "Approve" }).click();
    await expect(card).toBeHidden();
    // Its output, in a read-only terminal (colours and all).
    const tool = block.locator(".agent-tool").first();
    await expect(tool.locator(".agent-tool-status")).toHaveText("completed");
    await expect(tool.locator(".xterm-rows")).toContainText("ran: ls --color");
    await expect(block.locator(".agent-msg")).toContainText("Ran it.");
    await expect(block.locator(".agent-cost")).toContainText("$0.01");

    // The composer.
    await block.locator(".agent-composer textarea").fill("hello");
    await block.locator(".agent-composer textarea").press("Enter");
    await expect(block.locator(".agent-user").last()).toHaveText("hello");
    await expect(block.locator(".agent-msg").last()).toHaveText("Hello! I am fake.");
    await expect(block.locator(".agent-cost")).toContainText("$0.02 (last $0.01)");

    // Deny, then Stop a slow turn.
    await block.locator(".agent-composer textarea").fill("run rm -rf /tmp/nope");
    await block.getByRole("button", { name: "Send" }).click();
    await block.getByRole("button", { name: "Deny", exact: true }).click();
    await expect(block.locator(".agent-msg").last()).toHaveText("Not allowed.");
    await block.locator(".agent-composer textarea").fill("slow");
    await block.getByRole("button", { name: "Send" }).click();
    await expect(block.locator(".agent-msg").last()).toContainText("tick 2");
    await block.getByRole("button", { name: "Stop" }).click();
    await expect(block.getByRole("button", { name: "Stop" })).toBeHidden();
    await expect(block.locator(".agent-status")).toHaveText("Ready");

    // The tab is named after it once it's active; it closes like any pane.
    await page.evaluate((b) => window.__illogical.client.intent({ op: "close_pane", pane: b }), id);
    await expect.poll(() => panes(page)).toEqual([term]);
  });
});

// #111: Codex's adapter isn't in the run's agents directory (Claude Code's
// is the fake), and Install runs a stand-in npm (playwright.config.ts).
test("an adapter that isn't installed: its command to copy, and Install in a pane", async ({ page }) => {
  rmSync(join(process.env.ILLOGICAL_AGENTS_DIR!, "codex"), { recursive: true, force: true });
  await reset(page);
  const status = () =>
    page.evaluate(async () => {
      const r = await window.__illogical.client.request("GET", "/api/agents/adapters");
      return (await r.json<{ adapters: { kind: string; state: string; on_path?: boolean; npm: string }[] }>()).adapters.find((a) => a.kind === "codex")!;
    });
  const before = await status();
  test.skip(before.state === "installed", "codex-acp is on this machine's PATH");
  expect(before.state).toBe("missing");
  const pinned = /^npm install --omit=optional --prefix \S+\/codex @agentclientprotocol\/codex-acp@\d+\.\d+\.\d+$/;
  expect(before.npm).toMatch(pinned);
  const [term] = await panes(page);

  // A block started anyway says why once, with the command and Install.
  await page.evaluate((t) => window.__illogical.client.newAgent({ config: { agent: "codex" }, vm: false, split: t, from: t }), term);
  await expect.poll(() => agentBlock(page)).not.toBeNull();
  const id = (await agentBlock(page))!;
  const block = paneEl(page, id);
  await expect(block.locator(".agent-error")).toHaveText("the agent couldn't start: Codex's adapter isn't installed");
  await expect(block.locator("[data-adapter-npm]")).toHaveText(pinned);
  await expect(block.getByRole("button", { name: "Install" })).toBeVisible();
  await expect(block.getByRole("button", { name: "Copy" })).toBeVisible();

  // The dialog says so before Start, which waits for it.
  await menu(page, paneEl(page, term), "Start an agent…");
  const dialog = page.getByRole("dialog", { name: "Start an agent" });
  await dialog.locator("select[name=agent]").selectOption("codex");
  await expect(dialog.locator(".adapter-help")).toContainText("Codex's adapter isn't installed");
  await expect(dialog.locator("[data-adapter-npm]")).toHaveText(pinned);
  await expect(dialog.getByRole("button", { name: "Start" })).toBeDisabled();
  // Claude Code's is installed: nothing to say.
  await dialog.locator("select[name=agent]").selectOption("claude");
  await expect(dialog.locator(".adapter-help")).toBeHidden();
  await expect(dialog.getByRole("button", { name: "Start" })).toBeEnabled();
  // On a VM it installs its own (where VMs are set up: wisp, #180).
  await dialog.locator("select[name=agent]").selectOption("codex");
  if (await page.evaluate(() => window.__illogical.client.has("vms"))) {
    await dialog.locator("input[name=vm]").check();
    await expect(dialog.locator(".adapter-help")).toBeHidden();
    await dialog.locator("input[name=vm]").uncheck();
  } else {
    await expect(dialog.locator("input[name=vm]")).toHaveCount(0);
  }

  // Install: the command runs in a new pane to watch.
  await dialog.getByRole("button", { name: "Install" }).click();
  await expect(dialog).toBeHidden();
  await expect.poll(() => panes(page)).toHaveLength(3);
  const pane = (await panes(page)).find((p) => p !== term && p !== id)!;
  // (The pane is narrow: its lines wrap.)
  await expect.poll(async () => (await text(page, pane)).replace(/\s/g, ""), { timeout: 15_000 }).toContain("Installed@agentclientprotocol/codex-acp@");
  await expect.poll(() => text(page, pane)).toContain("exited with code 0");
  expect((await status()).state).toBe("installed");

  // Then the block starts.
  await block.getByRole("button", { name: "Resume" }).click();
  await expect(block.locator(".agent-status")).toHaveText("Ready");
  await expect(block.locator(".agent-error")).toBeHidden();
  await expect(block.locator(".adapter-help")).toBeHidden();
});

test.describe("phone", () => {
  const { viewport, userAgent, deviceScaleFactor, isMobile, hasTouch } = devices["Pixel 7"];
  test.use({ viewport, userAgent, deviceScaleFactor, isMobile, hasTouch });

  test("start an agent from the sheet and approve it with a thumb", async ({ page }) => {
    await reset(page);
    await page.locator(".sheet-button").click();
    await page.getByRole("button", { name: "New agent" }).click();
    await startFake(page, "run make deploy");
    await expect.poll(() => agentBlock(page)).not.toBeNull();
    const id = (await agentBlock(page))!;
    // It's what the phone shows, full screen, without the terminal key bar.
    await expect.poll(() => page.evaluate(() => window.__illogical.client.active())).toBe(id);
    const block = paneEl(page, id);
    await expect(page.locator(".keybar")).toBeHidden();
    const approve = block.getByRole("button", { name: "Approve" });
    await expect(approve).toBeVisible();
    const box = (await approve.boundingBox())!;
    expect(box.height).toBeGreaterThanOrEqual(40);
    await approve.tap();
    await expect(block.locator(".agent-tool .xterm-rows")).toContainText("ran: make deploy");
    await expect(block.locator(".agent-msg").last()).toHaveText("Ran it.");
    // Needs-you shows in the sheet while it waits, and goes when answered.
    await block.locator(".agent-composer textarea").fill("run git push");
    await block.getByRole("button", { name: "Send" }).tap();
    await expect(block.getByRole("button", { name: "Approve" })).toBeVisible();
    await page.evaluate(() => window.__illogical.client.setActive(window.__illogical.client.state!.panes[0].id));
    await page.locator(".sheet-button").click();
    await expect(page.locator(".needs-you")).toContainText("Needs you");
    await page.locator(".needs-you .sheet-item").first().click();
    await paneEl(page, id).getByRole("button", { name: "Deny", exact: true }).tap();
    await expect(paneEl(page, id).locator(".agent-msg").last()).toHaveText("Not allowed.");
  });
});
