// M33 in the client: Claude Code conversations from a terminal or the
// desktop app, picked from a pane's menu (desktop) or the sheet (phone),
// shown as an agent block, continued; one still open elsewhere forked
// instead. The Claude directory is the run's own (playwright.config.ts)
// and Claude Code's adapter is the fake ACP agent, so nothing here costs
// anything.

import { execFileSync } from "node:child_process";
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { devices, expect, test, type Page } from "@playwright/test";
import { menu, paneEl, panes, reset, seedConversation, type Base } from "./helpers";

const claude = process.env.CLAUDE_CONFIG_DIR!;

const seed = (word: string, title: string, more?: (base: Base) => object[]) => seedConversation(claude, word, title, more);

/** This test's process holds the session, as a running Claude Code would. */
function hold(id: string) {
  // Its start time as Claude Code writes it: field 22 of /proc/<pid>/stat
  // on Linux, `ps -o lstart=` in the C locale and UTC elsewhere.
  const proc = `/proc/${process.pid}/stat`;
  const stat = existsSync(proc) ? readFileSync(proc, "utf8") : null;
  const procStart = stat
    ? stat.slice(stat.lastIndexOf(")") + 1).trim().split(/\s+/)[19]
    : execFileSync("ps", ["-o", "lstart=", "-p", `${process.pid}`], { env: { ...process.env, LC_ALL: "C", TZ: "UTC" }, encoding: "utf8" }).trim();
  writeFileSync(
    join(claude, "sessions", `${process.pid}.json`),
    JSON.stringify({ pid: process.pid, sessionId: id, procStart, kind: "interactive", entrypoint: "cli", status: "idle" }),
  );
}

const agentBlock = (page: Page) =>
  page.evaluate(() => window.__arugula.client.state!.panes.find((p) => p.type === "agent")?.id ?? null);

const picker = (page: Page) => page.getByRole("dialog", { name: "Claude Code conversations" });

test.describe("desktop", () => {
  test("pick a conversation from the pane menu, read it, continue it", async ({ page }) => {
    const { id } = seed("kestrel", "Bird watching");
    await reset(page);
    const [term] = await panes(page);
    await menu(page, paneEl(page, term), "Claude Code conversations…");
    const dialog = picker(page);
    await expect(dialog).toBeVisible();
    const row = dialog.locator(`[data-conversation="${id}"]`);
    await expect(row).toContainText("Bird watching");
    await expect(row.locator(".conv-source")).toHaveText("Terminal");
    // Search narrows it.
    await dialog.locator(".picker-filter").fill("no such words at all");
    await expect(row).toBeHidden();
    await dialog.locator(".picker-filter").fill("bird");
    await row.click();
    await expect(dialog).toBeHidden();

    // A stopped block beside the pane: its transcript, nothing running.
    await expect.poll(() => agentBlock(page)).not.toBeNull();
    const block = (await agentBlock(page))!;
    expect(await panes(page)).toEqual([term, block]);
    const el = paneEl(page, block);
    await expect(el.locator(".agent-status")).toHaveText("Conversation");
    await expect(el.locator(".agent-import")).toContainText("from a terminal");
    await expect(el.locator(".agent-user").first()).toHaveText("remember kestrel");
    await expect(el.locator(".agent-tool .agent-tool-title")).toHaveText("echo kestrel");
    await expect(el.locator(".agent-tool .agent-tool-status")).toHaveText("completed");
    await expect(el.locator(".agent-composer textarea")).toHaveAttribute("placeholder", "Continue the conversation…");

    // Sending continues it, with its context.
    await el.locator(".agent-composer textarea").fill("recall");
    await el.locator(".agent-composer textarea").press("Enter");
    await expect(el.locator(".agent-msg").last()).toHaveText("You said kestrel.");
    await expect(el.locator(".agent-status")).toHaveText("Ready");
    await expect(el.locator(".agent-import")).toBeHidden();
    await expect(el.locator(".agent-note", { hasText: "Continued in Arugula" })).toBeVisible();

    // Picking it again goes to the block.
    await menu(page, paneEl(page, term), "Claude Code conversations…");
    await expect(picker(page).locator(`[data-conversation="${id}"] .host-tag`)).toHaveText(`%${block}`);
    await picker(page).locator(`[data-conversation="${id}"]`).click();
    await expect(picker(page)).toBeHidden();
    expect(await panes(page)).toEqual([term, block]);
  });

  test("what Continue won't remember is folded away (#79)", async ({ page }) => {
    // A rewind: "red" was sent, then "blue" from the same point instead.
    const reply = (base: Base, uuid: string, parent: string, m: string) => ({
      ...base("assistant", uuid, parent),
      message: { id: m, role: "assistant", model: "claude-haiku-4-5-20251001", content: [{ type: "text", text: "OK." }] },
    });
    seed("wren", "Colours", (base) => [
      { ...base("user", "u3", "u2"), message: { role: "user", content: "the colour is red" } },
      reply(base, "a3", "u3", "m3"),
      { type: "last-prompt", lastPrompt: "the colour is red", leafUuid: "a3" },
      { ...base("user", "u4", "u2"), message: { role: "user", content: "the colour is blue" } },
      reply(base, "a4", "u4", "m4"),
      { type: "last-prompt", lastPrompt: "the colour is blue", leafUuid: "a4" },
    ]);
    await reset(page);
    const [term] = await panes(page);
    await menu(page, paneEl(page, term), "Claude Code conversations…");
    await picker(page).locator(".picker-filter").fill("colours");
    await picker(page).locator("[data-conversation]").first().click();
    await expect.poll(() => agentBlock(page)).not.toBeNull();
    const el = paneEl(page, (await agentBlock(page))!);
    const fold = el.locator(".agent-forgotten");
    await expect(fold).toHaveCount(1);
    await expect(fold.locator("summary")).toHaveText("Not in what it remembers: Continue goes on from another branch (2 entries)");
    await expect(el.locator(".agent-user", { hasText: "red" })).toBeHidden();
    await expect(el.locator(".agent-user", { hasText: "blue" })).toBeVisible();
    await fold.locator("summary").click();
    await expect(fold.locator(".agent-user")).toHaveText("the colour is red");
  });

  test("one open elsewhere can't be continued, only forked", async ({ page }) => {
    const { id } = seed("plover", "Shore birds");
    hold(id);
    await reset(page);
    const [term] = await panes(page);
    await menu(page, paneEl(page, term), "Claude Code conversations…");
    const row = picker(page).locator(`[data-conversation="${id}"]`);
    await expect(row.locator(".conv-live")).toBeVisible();
    await row.click();
    await expect.poll(() => agentBlock(page)).not.toBeNull();
    const el = paneEl(page, (await agentBlock(page))!);
    await expect(el.locator(".agent-status")).toHaveText("Open elsewhere");
    await expect(el.locator("[data-continue]")).toBeDisabled();
    await expect(el.locator(".agent-import")).toContainText("Fork it to go on here");

    await el.locator("[data-fork]").click();
    await expect(el.locator(".agent-note", { hasText: "Forked into a new session" })).toBeVisible();
    await expect(el.locator(".agent-status")).toHaveText("Ready");
    await el.locator(".agent-composer textarea").fill("recall");
    await el.locator(".agent-composer textarea").press("Enter");
    await expect(el.locator(".agent-msg").last()).toHaveText("You said plover.");
  });
});

test.describe("phone", () => {
  const { viewport, userAgent, deviceScaleFactor, isMobile, hasTouch } = devices["Pixel 7"];
  test.use({ viewport, userAgent, deviceScaleFactor, isMobile, hasTouch });

  test("the sheet lists conversations and opens one", async ({ page }) => {
    const { id } = seed("heron", "Wading birds");
    await reset(page);
    await page.locator(".sheet-button").click();
    await page.locator("[data-conversations]").click();
    const dialog = picker(page);
    await expect(dialog.locator(".picker.phone")).toBeVisible();
    await dialog.locator(`[data-conversation="${id}"]`).click();
    await expect(dialog).toBeHidden();
    await expect.poll(() => agentBlock(page)).not.toBeNull();
    await expect(paneEl(page, (await agentBlock(page))!).locator(".agent-user").first()).toHaveText("remember heron");
  });
});
