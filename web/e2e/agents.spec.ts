// M6b in the client: an agent block started from the pane menu (desktop)
// and from the sheet (phone), with its permission card, tool-call output in
// a read-only terminal, and the composer. The agent is the scripted fake
// ACP server the daemon's tests use, so nothing here costs anything.

import { rmSync, writeFileSync } from "node:fs";
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

// M71: an image pasted into the composer goes with the prompt as an image
// (the fake agent says it takes them), and the transcript shows it; one
// dropped on the block waits beside it until it's removed.
test("an image pasted into the composer reaches the agent and shows in the transcript", async ({ page }) => {
  await reset(page);
  const [term] = await panes(page);
  await menu(page, paneEl(page, term), "Start an agent…");
  const dialog = page.getByRole("dialog", { name: "Start an agent" });
  await dialog.locator("select[name=agent]").selectOption("acp");
  await dialog.locator("input[name=acp]").fill(`python3 ${fake} --images`);
  await dialog.locator("textarea[name=prompt]").fill("hello");
  await dialog.getByRole("button", { name: "Start" }).click();
  await expect.poll(() => agentBlock(page)).not.toBeNull();
  const block = paneEl(page, (await agentBlock(page))!);
  await expect(block.locator(".agent-msg").last()).toHaveText("Hello! I am fake.");

  const png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";
  const give = (selector: string, event: "paste" | "drop", name: string) =>
    block.locator(selector).evaluate(
      (el, [b64, event, name]) => {
        const dt = new DataTransfer();
        dt.items.add(new File([Uint8Array.from(atob(b64), (c) => c.charCodeAt(0))], name, { type: "image/png" }));
        el.dispatchEvent(
          event === "paste"
            ? new ClipboardEvent("paste", { clipboardData: dt, bubbles: true, cancelable: true })
            : new DragEvent("drop", { dataTransfer: dt, bubbles: true, cancelable: true }),
        );
      },
      [png, event, name] as const,
    );
  const composer = block.locator(".agent-composer");
  // A long-press or right-click in the box is the browser's (its Paste
  // pastes an image on a phone); elsewhere on the block, the pane's menu.
  await composer.locator("textarea").click({ button: "right" });
  await expect(page.getByRole("menuitem", { name: "Start an agent…" })).toHaveCount(0);
  await block.locator(".agent-log").click({ button: "right" });
  await expect(page.getByRole("menuitem").first()).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("menuitem")).toHaveCount(0);
  await give(".agent-composer textarea", "paste", "shot.png");
  await expect(composer.locator(".agent-attached-file img")).toHaveCount(1);
  await expect(composer.locator("textarea")).toHaveValue("");
  await give(".agent-log", "drop", "other.png");
  await expect(composer.locator(".agent-attached-file")).toHaveCount(2);
  await composer.getByRole("button", { name: "Remove other.png" }).click();
  await expect(composer.locator(".agent-attached-file")).toHaveCount(1);

  await composer.locator("textarea").fill("look");
  await composer.locator("textarea").press("Enter");
  await expect(block.locator(".agent-msg").last()).toHaveText("Saw 1 image(s) ['image/png']; text []");
  await expect(composer.locator(".agent-attached-file")).toHaveCount(0);
  const shown = block.locator(".agent-user").last().locator(".agent-image img");
  await expect(shown).toBeVisible();
  expect(await shown.evaluate((img: HTMLImageElement) => img.naturalWidth)).toBe(1);
});

// #166: "From now on…" on a card makes a standing rule the daemon keeps:
// the next agent block started there never asks, the session menu lists
// the rule, and forgetting it there brings the card back.
test("a standing rule outlives its block and is forgotten from the session menu", async ({ page }) => {
  await reset(page);
  await page.evaluate(() => window.__illogical.client.request("DELETE", "/api/rules"));
  const [term] = await panes(page);
  const agents = () => page.evaluate(() => window.__illogical.client.state!.panes.filter((p) => p.type === "agent").map((p) => p.id));

  await menu(page, paneEl(page, term), "Start an agent…");
  await startFake(page, "run make -j4");
  await expect.poll(agents).toHaveLength(1);
  const first = paneEl(page, (await agents())[0]);
  const card = first.getByRole("alertdialog", { name: "Allow make -j4?" });
  await card.getByRole("button", { name: "From now on…" }).click();
  const form = card.getByRole("form", { name: "A standing rule" });
  // Bash's first word by default; this directory by default.
  await expect(form.locator("input[name=prefix]")).toHaveValue("make");
  await expect(form.locator("select[name=scope]")).toHaveValue("cwd");
  await form.getByRole("button", { name: "Allow from now on" }).click();
  await expect(card).toBeHidden();
  await expect(first.locator(".agent-note").filter({ hasText: /Allowed make -j4, and from now on: Bash make… in / })).toBeVisible();

  // Another block in the same place: no card.
  await menu(page, paneEl(page, term), "Start an agent…");
  await startFake(page, "run make test");
  await expect.poll(agents).toHaveLength(2);
  const second = paneEl(page, (await agents())[1]);
  await expect(second.locator(".agent-msg")).toContainText("Ran it.");
  await expect(second.locator(".agent-note").filter({ hasText: /Allowed make test \(standing rule: Bash make… in / })).toBeVisible();

  // The session menu lists it; Forget, and the next request asks again.
  await page.locator(".session-button").click();
  await page.getByRole("menuitem", { name: "Permission rules…" }).click();
  const dialog = page.getByRole("dialog", { name: "Permission rules" });
  await expect(dialog.locator("[data-rule]")).toHaveCount(1);
  await expect(dialog.locator("[data-rule=\"0\"] .rule-text")).toContainText("Bash make… in ");
  await dialog.locator("[data-rule=\"0\"]").getByRole("button", { name: "Forget" }).click();
  await expect(dialog.locator("[data-rule]")).toHaveCount(0);
  await expect(dialog).toContainText("None yet");
  await dialog.getByRole("button", { name: "Close" }).click();
  await second.locator(".agent-composer textarea").fill("run make install");
  await second.getByRole("button", { name: "Send" }).click();
  await expect(second.getByRole("alertdialog", { name: "Allow make install?" })).toBeVisible();
  await second.getByRole("button", { name: "Deny", exact: true }).click();
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

// #335: Getting started's Agents step says each adapter's state next to
// Start an agent, and one click installs one (`POST /api/setup/agents/…`,
// the daemon waiting for npm: the stand-in here), then says what changed;
// an install older than the pin is out of date, and the same click
// updates it. Codex's, since Claude Code's would also add illogical's MCP
// server to the Claude Code on the test machine.
test("Getting started: each adapter's state, and one click that installs or updates it", async ({ page }) => {
  const dir = join(process.env.ILLOGICAL_AGENTS_DIR!, "codex");
  rmSync(dir, { recursive: true, force: true });
  await reset(page);
  type A = { kind: string; state: string; outdated?: boolean; found: boolean; version?: string; pinned: string };
  const codex = () =>
    page.evaluate(async () => {
      const r = await fetch("/api/setup?part=agents");
      return ((await r.json()) as { adapters: A[] }).adapters.find((a) => a.kind === "codex")!;
    });
  const before = await codex();
  test.skip(before.state === "installed", "codex-acp is on this machine's PATH");
  expect(before.state).toBe("missing");
  expect(typeof before.found).toBe("boolean");

  // Claude Code's (the fake, no version) is next to Start an agent.
  await page.evaluate(() => dispatchEvent(new CustomEvent("illogical:getting-started", { detail: "agents" })));
  const panel = page.getByRole("dialog", { name: "Getting started" });
  await expect(panel.locator("[data-start-progress]")).toHaveText("Step 4 / 5 · Agents");
  await expect(panel.locator('[data-start-adapter="claude"]')).toContainText("Claude Code's adapter");
  await expect(panel.locator("[data-start-agent]")).toBeVisible();
  await panel.getByRole("button", { name: "Close" }).click();

  // One click: installed, and said so.
  const use = () => page.evaluate(async () => (await fetch("/api/setup/agents/codex", { method: "POST", headers: { "content-type": "application/json" }, body: "{}" })).json());
  let o = await use();
  expect(o.ok).toBe(true);
  expect(o.done).toEqual([`Installed Codex's adapter (${before.pinned}): Codex runs as agent panes here.`]);
  expect(await codex()).toMatchObject({ state: "installed", version: before.pinned, outdated: false });
  // Again: nothing to do.
  o = await use();
  expect(o.done).toEqual(["Already set up: Codex runs as agent panes here."]);

  // An older install is out of date, in the status and the step.
  writeFileSync(join(dir, "node_modules/@agentclientprotocol/codex-acp/package.json"), '{"version": "0.0.1"}');
  expect(await codex()).toMatchObject({ state: "installed", version: "0.0.1", outdated: true });
  if (before.found) {
    await page.evaluate(() => dispatchEvent(new CustomEvent("illogical:getting-started", { detail: "agents" })));
    await expect(panel.locator('[data-start-adapter="codex"]')).toHaveText(`Codex's adapter 0.0.1: out of date (illogical uses ${before.pinned})`);
    await expect(panel.locator('.adapter-help[data-adapter="outdated"]')).toBeVisible();
    await panel.getByRole("button", { name: "Close" }).click();
  }
  // The same click updates it.
  o = await use();
  expect(o.done).toEqual([`Updated Codex's adapter from 0.0.1 to ${before.pinned}: agent panes use it from their next start.`]);
  expect(await codex()).toMatchObject({ state: "installed", version: before.pinned, outdated: false });

  // No such agent: said.
  const bad = await page.evaluate(async () => (await fetch("/api/setup/agents/nope", { method: "POST" })).json());
  expect(bad.ok).toBe(false);
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
