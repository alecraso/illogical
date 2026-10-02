// M33 in the client: Claude Code conversations from a terminal or the
// desktop app, picked from a pane's menu (desktop) or the sheet (phone),
// shown as an agent block, continued; one still open elsewhere forked
// instead. The Claude directory is the run's own (playwright.config.ts)
// and Claude Code's adapter is the fake ACP agent, so nothing here costs
// anything.

import { mkdirSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { devices, expect, test, type Page } from "@playwright/test";
import { menu, paneEl, panes, reset } from "./helpers";

const claude = process.env.CLAUDE_CONFIG_DIR!;

/** A terminal session in its own folder: "remember WORD", a reply, a
 * command and its output. Its id. */
function seed(word: string, title: string): { id: string; cwd: string } {
  const cwd = mkdtempSync(join(tmpdir(), `illogical-e2e-conv-${word}-`));
  const id = crypto.randomUUID();
  const dir = join(claude, "projects", cwd.replace(/[/.]/g, "-"));
  mkdirSync(dir, { recursive: true });
  const base = (type: string, uuid: string, parentUuid: string | null) => ({
    type, uuid, parentUuid, sessionId: id, cwd, gitBranch: "main", entrypoint: "cli", version: "2.1.288",
    timestamp: new Date().toISOString(), isSidechain: false,
  });
  const lines = [
    { ...base("user", "u1", null), message: { role: "user", content: `remember ${word}` } },
    { ...base("assistant", "a1", "u1"), message: { id: "m1", role: "assistant", model: "claude-haiku-4-5-20251001", content: [{ type: "text", text: "Noted." }] } },
    { ...base("assistant", "a2", "a1"), message: { id: "m1", role: "assistant", model: "claude-haiku-4-5-20251001", content: [{ type: "tool_use", id: `toolu_${word}`, name: "Bash", input: { command: `echo ${word}` } }] } },
    { ...base("user", "u2", "a2"), message: { role: "user", content: [{ type: "tool_result", tool_use_id: `toolu_${word}`, content: word }] } },
    { type: "ai-title", aiTitle: title, sessionId: id },
  ];
  writeFileSync(join(dir, `${id}.jsonl`), lines.map((l) => JSON.stringify(l)).join("\n") + "\n");
  return { id, cwd };
}

/** This test's process holds the session, as a running Claude Code would. */
function hold(id: string) {
  const stat = readFileSync(`/proc/${process.pid}/stat`, "utf8");
  const procStart = stat.slice(stat.lastIndexOf(")") + 1).trim().split(/\s+/)[19];
  writeFileSync(
    join(claude, "sessions", `${process.pid}.json`),
    JSON.stringify({ pid: process.pid, sessionId: id, procStart, kind: "interactive", entrypoint: "cli", status: "idle" }),
  );
}

const agentBlock = (page: Page) =>
  page.evaluate(() => window.__illogical.client.state!.panes.find((p) => p.type === "agent")?.id ?? null);

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
    await expect(el.locator(".agent-note", { hasText: "Continued in illogical" })).toBeVisible();

    // Picking it again goes to the block.
    await menu(page, paneEl(page, term), "Claude Code conversations…");
    await expect(picker(page).locator(`[data-conversation="${id}"] .host-tag`)).toHaveText(`%${block}`);
    await picker(page).locator(`[data-conversation="${id}"]`).click();
    await expect(picker(page)).toBeHidden();
    expect(await panes(page)).toEqual([term, block]);
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
