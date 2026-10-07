// M26 with the real Claude Code (2.1.287 TUI, haiku): its tool permission
// prompt, through the team answers hooks (M29), lands on the swarm's rail;
// Allow there lets the tool run and the agent carry on to its reply. It
// costs a cent or so, so it runs only when asked:
//
//   ARUGULA_REAL_AGENTS=swarm npx playwright test e2e/swarm-real.spec.ts
//
// Claude Code reads only a settings file of its own here (your settings
// aren't touched), in target/m26-tui.

import { existsSync, mkdirSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { join, resolve } from "node:path";
import { expect, test } from "@playwright/test";
import { open } from "./helpers";

const wanted = (process.env.ARUGULA_REAL_AGENTS ?? "").split(",").includes("swarm");
const UNSET = ["CLAUDECODE", "CLAUDE_CODE_CHILD_SESSION", "CLAUDE_CODE_SESSION_ID", "CLAUDE_PID", "CLAUDE_EFFORT", "CLAUDE_CODE_MESSAGING_SOCKET", "CLAUDE_CODE_SESSION_ATTENDED", "CLAUDE_CODE_ENTRYPOINT", "CLAUDE_CODE_EXECPATH", "CLAUDE_CODE_MESSAGING_TOKEN", "AI_AGENT"];

test("a real Claude Code permission prompt, allowed from the rail", async ({ page }) => {
  test.skip(!wanted, "set ARUGULA_REAL_AGENTS=swarm to run (it costs money)");
  test.setTimeout(180_000);
  const cli = resolve("../target/debug/arugula");
  const dir = resolve("../target/m26-tui");
  mkdirSync(dir, { recursive: true });
  const settings = join(dir, "settings.json");
  const hook = (cmd: string) => [{ hooks: [{ type: "command", command: `${cli} ${cmd}`, timeout: 604800 }] }];
  const inbox = [{ hooks: [{ type: "command", command: `${cli} inbox`, asyncRewake: true, timeout: 86400 }] }];
  writeFileSync(
    settings,
    JSON.stringify({
      hooks: {
        PermissionRequest: hook("hook"),
        PreToolUse: hook("hook"),
        PostToolUse: hook("hook"),
        PostToolUseFailure: hook("hook"),
        UserPromptSubmit: hook("hook"),
        Stop: inbox,
        SessionStart: inbox,
      },
    }),
  );
  const claude = process.env.CLAUDE_BIN ?? join(homedir(), ".local/bin/claude");
  const prompt = "Use the Bash tool to run exactly this command: touch m26-marker.txt   Then reply with six times seven, in digits.";
  const command = `cd ${dir} && rm -f m26-marker.txt && env ${UNSET.map((v) => `-u ${v}`).join(" ")} ${claude} --model haiku --setting-sources local --settings ${settings} '${prompt}'`;
  await open(page);
  const pane = await page.evaluate(async (command) => {
    const r = await fetch("/api/run", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ command }) });
    return (await r.json()).pane as number;
  }, command);
  await page.goto("/#swarm");
  const host = await page.evaluate(() => window.__arugula.hosts.current);
  const card = page.locator(`.swarm-card[data-kind="ask"][data-panes~="${host}:${pane}"]`);
  await expect(card).toBeVisible({ timeout: 90_000 });
  expect(existsSync(join(dir, "m26-marker.txt"))).toBe(false);
  await expect(card.locator(".agent-perm-cmd")).toHaveText("touch m26-marker.txt");
  await card.getByRole("button", { name: "Allow", exact: true }).click();
  // The tool ran, and the agent carried on to its reply.
  await expect.poll(() => existsSync(join(dir, "m26-marker.txt")), { timeout: 60_000 }).toBe(true);
  await expect
    .poll(async () => page.evaluate(async (p) => (await fetch(`/api/panes/${p}/capture?format=text`)).text(), pane), { timeout: 90_000 })
    .toMatch(/● 42/);
  await page.evaluate(async (p) => fetch(`/api/panes/${p}/close`, { method: "POST" }), pane);
});
