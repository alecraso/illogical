// M6c in the client: questions and forms from agents as cards. An agent
// block's AskUserQuestion (several questions, a multi-select, "Other",
// previews), an MCP form, a sign-in link, Stop with a question open; and
// Claude Code in a terminal asking through its hook (`arugula ask`, fed
// the hook input S13 recorded), answered from the card beside the terminal
// or left to the terminal. Desktop and phone. The agent is the scripted fake
// ACP server, so nothing here costs anything.

import { fileURLToPath } from "node:url";
import { devices, expect, test, type Page } from "@playwright/test";
import { paneEl, panes, reset, text } from "./helpers";
import type { PaneId } from "../src/proto";

const fake = fileURLToPath(new URL("../../crates/daemon/tests/fake_acp.py", import.meta.url));
const hookInput = fileURLToPath(new URL("../../crates/daemon/tests/fixtures/s13-hook-ask.json", import.meta.url));
const cli = fileURLToPath(new URL("../../target/debug/arugula", import.meta.url));

/** An agent block on the fake server beside the terminal, shown. */
async function agent(page: Page, prompt: string): Promise<PaneId> {
  const [term] = await panes(page);
  const id = await page.evaluate(
    async ({ fake, prompt, term }) => {
      const c = window.__arugula.client;
      const config = { agent: "acp", command: ["python3", fake], cwd: "/tmp", prompt };
      const res = await fetch(`${c.base}/api/blocks`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ type: "agent", config, split: term }),
      });
      return (await res.json()).block as number;
    },
    { fake, prompt, term },
  );
  await expect.poll(() => panes(page)).toContain(id);
  await page.evaluate((id) => window.__arugula.client.setActive(id), id);
  return id;
}

/** Type a line into a terminal (through the API, as on any device). */
async function send(page: Page, pane: PaneId, line: string) {
  await page.evaluate(
    async ({ pane, line }) => {
      const c = window.__arugula.client;
      await fetch(`${c.base}/api/panes/${pane}/send`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ text: line, enter: true }),
      });
    },
    { pane, line },
  );
}

/** Claude Code's hook, run in a terminal: prints each answer on a line. */
const hook = (tag: string) =>
  `out=$(${cli} ask < ${hookInput}); echo "${tag}[\${#out}]"; echo "$out" | grep -o '"Which [a-z ]*?":"[^"]*"'`;

test.describe("desktop", () => {
  test("an agent's questions, form and sign-in link as cards", async ({ page }) => {
    await reset(page);
    const id = await agent(page, "ask");
    const block = paneEl(page, id);
    const card = block.getByRole("dialog", { name: "Which colour do you prefer?" });
    await expect(card).toBeVisible();
    await expect(block.locator(".agent-status")).toHaveText("Needs you");
    await expect(card.locator(".ask-message")).toHaveText("Please answer the following questions.");
    await expect(card.locator(".ask-header")).toHaveText(["Colour", "Fruit", "Pet"]);
    const submit = card.getByRole("button", { name: "Submit" });
    await expect(submit).toBeDisabled();

    // A pick with a note; two of three plus "Other"; "Other" alone.
    const q = (n: number) => card.locator(`[data-question="${n}"]`);
    await q(0).getByRole("radio", { name: /^Red/ }).click();
    await expect(q(0).getByRole("radio", { name: /^Red/ })).toHaveAttribute("aria-checked", "true");
    await q(0).locator(".ask-other").fill("dark red please");
    await q(1).getByRole("checkbox", { name: /Apple/ }).check();
    await q(1).getByRole("checkbox", { name: /Plum/ }).check();
    await q(1).locator(".ask-other").fill("kiwi");
    await q(2).locator(".ask-other").fill("a parrot");
    await submit.click();
    await expect(card).toBeHidden();
    await expect(block.locator(".agent-msg").last()).toHaveText(
      "You answered: Which colour do you prefer? Red (dark red please); Which fruits do you like? Apple, Plum, kiwi; Which pet do you prefer? a parrot",
    );
    await expect(block.locator(".agent-note")).toContainText([/^Answered: Which colour do you prefer\? → Red \(dark red please\)/]);
    await expect(block.locator(".agent-tool .agent-tool-icon").first()).toHaveText("?");

    // A preview shows in monospace when its option is picked.
    const composer = block.locator(".agent-composer textarea");
    await composer.fill("ask preview");
    await composer.press("Enter");
    const layout = block.getByRole("dialog", { name: "Which layout?" });
    await layout.getByRole("radio", { name: /^Sidebar/ }).click();
    await expect(layout.locator(".ask-preview")).toContainText("| Nav  | Content Area |");
    await layout.getByRole("button", { name: "Skip" }).click();
    await expect(block.locator(".agent-msg").last()).toHaveText("You didn't answer the question.");

    // An MCP server's form: enum + enumNames, a number, a checkbox; Submit
    // waits for what's required.
    await composer.fill("form");
    await composer.press("Enter");
    const form = block.getByRole("dialog", { name: "Order details" });
    await expect(form.getByRole("button", { name: "Submit" })).toBeDisabled();
    await form.getByRole("radio", { name: "Medium" }).click();
    await form.locator("input[name=qty]").fill("2");
    await form.getByRole("checkbox").check();
    await form.getByRole("button", { name: "Submit" }).click();
    await expect(block.locator(".agent-msg").last()).toHaveText('Order: {"action": "accept", "content": {"gift": true, "qty": 2, "size": "M"}}');

    // A sign-in link: it opens, and the card goes when the agent says so.
    await composer.fill("signin");
    await composer.press("Enter");
    const link = block.getByRole("dialog", { name: "Sign in to Fake" });
    await expect(link.locator(".ask-url")).toHaveText("https://example.com/fake-signin");
    const popup = page.waitForEvent("popup");
    await link.getByRole("button", { name: "Open link" }).click();
    await (await popup).close();
    await expect(link).toBeHidden();
    await expect(block.locator(".agent-msg").last()).toHaveText("Signed in.");

    // Stop with a question open ends the turn at once.
    await composer.fill("ask");
    await composer.press("Enter");
    const again = block.getByRole("dialog", { name: "Which colour do you prefer?" });
    await again.getByRole("button", { name: "Stop" }).click();
    await expect(again).toBeHidden({ timeout: 2000 });
    await expect(block.locator(".agent-status")).toHaveText("Ready");
    await expect(block.locator(".agent-note").last()).toHaveText("The question was withdrawn");
  });

  test("Claude Code in a terminal asks beside it; or in the terminal", async ({ page }) => {
    await reset(page);
    const [term] = await panes(page);
    const pane = paneEl(page, term);
    await send(page, term, hook("ONE"));
    const card = pane.locator(".pane-ask").getByRole("dialog", { name: "Which colour do you prefer?" });
    await expect(card).toBeVisible();
    await expect.poll(() => page.evaluate((t) => window.__arugula.client.info(t)?.attention, term)).toBe("needs_input");
    // Tucked away to see the terminal, and back.
    await pane.locator(".pane-ask").getByRole("button", { name: "Hide" }).click();
    await pane.getByRole("button", { name: "Claude Code asks…" }).click();
    await card.locator('[data-question="0"]').getByRole("radio", { name: /^Blue/ }).click();
    await card.locator('[data-question="1"]').getByRole("checkbox", { name: /Pear/ }).check();
    await card.locator('[data-question="2"]').getByRole("radio", { name: /^Cat/ }).click();
    await card.getByRole("button", { name: "Submit" }).click();
    await expect(card).toBeHidden();
    await expect.poll(() => text(page, term)).toContain('"Which colour do you prefer?":"Blue"');
    const out = await text(page, term);
    expect(out).toContain('"Which fruits do you like?":"Pear"');
    expect(out).toContain('"Which pet do you prefer?":"Cat"');

    // "Answer in terminal": the hook prints nothing.
    await send(page, term, hook("TWO"));
    await pane.getByRole("button", { name: "Answer in terminal" }).click();
    await expect(pane.locator(".pane-ask")).toBeHidden();
    await expect.poll(() => text(page, term)).toContain("TWO[0]");
  });
});

test.describe("phone", () => {
  const { viewport, userAgent, deviceScaleFactor, isMobile, hasTouch } = devices["Pixel 7"];
  test.use({ viewport, userAgent, deviceScaleFactor, isMobile, hasTouch });

  test("two questions answered with a thumb: a multi-select, and Other", async ({ page }) => {
    await reset(page);
    const id = await agent(page, "ask two");
    await expect.poll(() => page.evaluate(() => window.__arugula.client.active())).toBe(id);
    const block = paneEl(page, id);
    const card = block.getByRole("dialog", { name: "Which fruits do you like?" });
    await expect(card).toBeVisible();
    const pear = card.locator('[data-question="0"]').getByRole("checkbox", { name: /Pear/ });
    await pear.tap();
    await card.locator('[data-question="0"]').getByRole("checkbox", { name: /Plum/ }).tap();
    await expect(pear).toBeChecked();
    await card.locator('[data-question="1"] .ask-other').fill("a parrot");
    const submit = card.getByRole("button", { name: "Submit" });
    expect((await submit.boundingBox())!.height).toBeGreaterThanOrEqual(40);
    await submit.tap();
    await expect(block.locator(".agent-msg").last()).toHaveText(
      "You answered: Which fruits do you like? Pear, Plum; Which pet do you prefer? a parrot",
    );
    await expect(block.locator(".agent-tool .agent-tool-status").last()).toHaveText("completed");
  });

  test("Claude Code's question in a terminal, answered from the phone", async ({ page }) => {
    await reset(page);
    const [term] = await panes(page);
    await send(page, term, hook("PHONE"));
    const card = paneEl(page, term).getByRole("dialog", { name: "Which colour do you prefer?" });
    await expect(card).toBeVisible();
    // The phone's "needs you" dot.
    await expect(page.locator(".sheet-button .att.needs_input")).toBeVisible();
    await card.locator('[data-question="0"]').getByRole("radio", { name: /^Red/ }).tap();
    await card.locator('[data-question="2"] .ask-other').fill("a parrot");
    await card.getByRole("button", { name: "Submit" }).tap();
    await expect(card).toBeHidden();
    await expect.poll(() => text(page, term)).toContain('"Which pet do you prefer?":"a parrot"');
  });
});
