// #421: a permission card's Allow button stays reachable however long the
// command or the file is. Two surfaces, each on a phone and on a desktop
// viewport: the agent block's own card (the scripted fake ACP agent asks to
// run a long command) and the card over a terminal (Claude Code's
// PermissionRequest hook, fed S18's recorded input, for a long command and
// for a Write of 2000 characters).

import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { devices, expect, test, type Locator, type Page } from "@playwright/test";
import { paneEl, panes, reset, text } from "./helpers";

const fake = fileURLToPath(new URL("../../crates/daemon/tests/fake_acp.py", import.meta.url));
const cli = resolve("../target/debug/arugula");
const fixtures = resolve("../crates/daemon/tests/fixtures");

/** A multi-line command well past a screen. */
const LONG_COMMAND = Array.from({ length: 40 }, (_, i) => `echo step-${i} && grep -rn "needle-${i}" src | head -${i + 1}`).join("\n");
/** A Write's content, as the card shows it: the first 2000 characters. */
const BIG_FILE = Array.from({ length: 80 }, (_, i) => `line ${i}: ${"x".repeat(20)}`).join("\n").slice(0, 2000);

function pick(d: (typeof devices)[string]) {
  const { viewport, userAgent, deviceScaleFactor, isMobile, hasTouch } = d;
  return { viewport, userAgent, deviceScaleFactor, isMobile, hasTouch };
}

const viewports = {
  phone: pick(devices["Pixel 7"]),
  desktop: { viewport: { width: 1280, height: 720 } },
};

/** The button is on screen, whole, in the page's viewport. */
async function inView(page: Page, button: Locator) {
  await expect(button).toBeVisible();
  const box = (await button.boundingBox())!;
  const vp = page.viewportSize()!;
  expect(box.y).toBeGreaterThanOrEqual(0);
  expect(box.y + box.height).toBeLessThanOrEqual(vp.height);
  expect(box.x).toBeGreaterThanOrEqual(0);
  expect(box.x + box.width).toBeLessThanOrEqual(vp.width);
}

for (const [name, use] of Object.entries(viewports)) {
  test.describe(`${name}: a permission card's buttons are reachable`, () => {
    test.use(use);

    for (const [what, prompt] of [
      ["a long multi-line command", `run ${LONG_COMMAND}`],
      ["a 2000-character command", `run ${BIG_FILE}`],
    ] as const) {
      test(`agent block, ${what}`, async ({ page }) => {
        await reset(page);
        if (name === "phone") {
          await page.locator(".sheet-button").click();
          await page.getByRole("button", { name: "New agent" }).click();
        } else {
          const [term] = await panes(page);
          await paneEl(page, term).click({ button: "right", position: { x: 60, y: 60 } });
          await page.getByRole("menuitem", { name: "Start an agent…" }).click();
        }
        const dialog = page.getByRole("dialog", { name: "Start an agent" });
        await expect(dialog).toBeVisible();
        await dialog.locator("select[name=agent]").selectOption("acp");
        await dialog.locator("input[name=acp]").fill(`python3 ${fake}`);
        await dialog.locator("textarea[name=prompt]").fill(prompt);
        await dialog.getByRole("button", { name: "Start" }).click();
        await expect(dialog).toBeHidden();
        const agent = () => page.evaluate(() => window.__arugula.client.state!.panes.find((p) => p.type === "agent")?.id ?? null);
        await expect.poll(agent).not.toBeNull();
        const block = paneEl(page, (await agent())!);
        const approve = block.getByRole("button", { name: "Approve" });
        await inView(page, approve);
        // The command scrolls in its own box.
        const cmd = block.locator(".agent-perm-cmd");
        expect(await cmd.evaluate((e) => e.scrollHeight > e.clientHeight)).toBe(true);
        await approve.click();
        await expect(block.locator(".agent-perm")).toBeHidden();
        await expect(block.locator(".agent-msg").last()).toHaveText("Ran it.");
      });
    }

    for (const [what, input] of [
      ["a long multi-line command", { command: LONG_COMMAND, description: "Look for needles" }],
      ["a Write of 2000 characters", { file_path: "/work/tui/big.txt", content: BIG_FILE }],
    ] as const) {
      test(`terminal card, ${what}`, async ({ page }) => {
        await reset(page);
        const [term] = await panes(page);
        const dir = mkdtempSync(join(tmpdir(), "arugula-e2e-perm421-"));
        try {
          const read = (n: string) => JSON.parse(readFileSync(join(fixtures, n), "utf8"));
          const tool = "command" in input ? "Bash" : "Write";
          const pre = { ...read("s18-hook-pretooluse-bash.json"), tool_name: tool, tool_input: input, tool_use_id: "toolu_421" };
          const perm = { ...read("s18-hook-permission.json"), tool_name: tool, tool_input: input };
          writeFileSync(join(dir, "pre.json"), JSON.stringify(pre));
          writeFileSync(join(dir, "perm.json"), JSON.stringify(perm));
          await page.evaluate(
            async ([pane, line]) => {
              const r = await window.__arugula.client.request("POST", `/api/panes/${pane}/send`, { text: line, enter: true });
              if (!r.ok) throw new Error(`send: ${r.status}`);
            },
            [term, `${cli} hook < ${dir}/pre.json; ${cli} hook < ${dir}/perm.json; echo hook-done-$((6*7))`] as const,
          );
          const card = paneEl(page, term).locator(".ask.perm");
          await expect(card).toBeVisible({ timeout: 15_000 });
          const allow = card.getByRole("button", { name: "Allow", exact: true });
          await inView(page, allow);
          await allow.click();
          await expect(card).toBeHidden();
          // The hook returned: the shell ran the rest of its line.
          await expect.poll(() => text(page, term)).toContain("hook-done-42");
        } finally {
          rmSync(dir, { recursive: true, force: true });
        }
      });
    }
  });
}
