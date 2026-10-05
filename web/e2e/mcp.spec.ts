// M16: a command an MCP client runs shows who started it. Claude Code (here,
// its requests by hand over /mcp) runs a command; the pane it made says
// "started by mcp:claude-code", and history records the command as theirs.
// From a phone (#214 section 6): the build an MCP client started is watched
// as it runs, its failure is on the phone's Needs you, and the client's
// fix and rerun show up there too. With ANTHROPIC_API_KEY and `claude` on
// PATH, the real Claude Code does the same through `illogical mcp`.

import { spawn, spawnSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { expect, test, type APIRequestContext, type Page } from "@playwright/test";
import { open, reset, ready, screen, text } from "./helpers";
import { pixel7 } from "./phones";

/** One MCP session over Streamable HTTP, as Codex speaks it (2025-06-18). */
async function mcp(request: APIRequestContext, name: string) {
  const base = { "Content-Type": "application/json", Accept: "application/json, text/event-stream" };
  const messages = (body: string) =>
    body
      .split("\n\n")
      .map((e) => e.split("\n").filter((l) => l.startsWith("data:")).map((l) => l.slice(5).trim()).join(""))
      .filter((d) => d)
      .map((d) => JSON.parse(d));
  const init = await request.post("/mcp", {
    headers: base,
    data: { jsonrpc: "2.0", id: 1, method: "initialize", params: { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name, version: "1" } } },
  });
  expect(init.status()).toBe(200);
  const session = init.headers()["mcp-session-id"];
  const headers = { ...base, "Mcp-Session-Id": session, "MCP-Protocol-Version": "2025-06-18" };
  await request.post("/mcp", { headers, data: { jsonrpc: "2.0", method: "notifications/initialized" } });
  let id = 2;
  return async (tool: string, args: object) => {
    const res = await request.post("/mcp", {
      headers,
      data: { jsonrpc: "2.0", id: ++id, method: "tools/call", params: { name: tool, arguments: args } },
      timeout: 30_000,
    });
    const answer = messages(await res.text()).find((m) => m.id === id);
    expect(answer?.result?.isError, JSON.stringify(answer)).toBeFalsy();
    return answer.result.structuredContent;
  };
}

test("a pane an MCP client started says so, and history has it as theirs", async ({ page, request }) => {
  await reset(page);
  const call = await mcp(request, "claude-code");
  const r = await call("run", { command: "echo built-by-$((40+2))", wait: true, timeout: 20 });
  expect(r.exit).toBe(0);
  const pane: number = r.pane;

  // The browser shows its tab: the pane, its output, and who started it.
  await page.evaluate((p) => {
    const c = window.__illogical.client;
    const tab = c.tabOfPane(p);
    if (tab) c.selectTab(tab.id);
  }, pane);
  await ready(page, pane);
  await expect.poll(() => screen(page, pane)).toContain("built-by-42");
  const badge = page.locator(`[data-pane="${pane}"] .started-by`);
  await expect(badge).toHaveText("started by mcp:claude-code");
  await expect(badge).toHaveAttribute("data-started-by", "mcp:claude-code");

  // History: the command, as theirs.
  const history = await (await request.get(`/api/history?pane=${pane}`)).json();
  expect(history).toEqual([expect.objectContaining({ text: "echo built-by-$((40+2))", exit: 0, by: "mcp:claude-code" })]);
  // A pane you open yourself says nothing of the kind.
  await expect(page.locator(".started-by")).toHaveCount(1);
});

const reasonOf = (page: Page, pane: number) => page.evaluate((p) => window.__illogical.client.info(p)?.reason ?? null, pane);

test.describe("watched from a phone", () => {
  test.use(pixel7);

  test("a build an MCP client runs is watched live from the phone, fails there, and its rerun passes", async ({ page, request }) => {
    test.setTimeout(60_000);
    await reset(page);
    const home = await page.evaluate(() => window.__illogical.client.active()!);
    const dir = mkdtempSync(join(tmpdir(), "ilg-e2e-mcp-phone-"));
    try {
      const call = await mcp(request, "claude-code");
      // A build that takes a few seconds and fails until a file exists.
      const build = `for i in 1 2 3 4 5 6; do echo step-$i; sleep 0.5; done; test -f ${dir}/ready && echo BUILD-OK || { echo 'error: ready is missing'; false; }`;
      const started = await call("run", { command: build });
      const pane: number = started.pane;
      // The phone shows it while it runs.
      await page.evaluate((p) => {
        const c = window.__illogical.client;
        c.selectTab(c.tabOfPane(p)!.id);
        c.setActive(p);
      }, pane);
      await ready(page, pane);
      await expect(page.locator(`[data-pane="${pane}"] .started-by`)).toHaveText("started by mcp:claude-code");
      // Live: a step drawn on the phone before the build is over.
      await expect(page.locator(`[data-pane="${pane}"] .xterm-rows`)).toContainText("step-2", { timeout: 10_000 });
      expect(await text(page, pane)).not.toMatch(/^error: ready is missing/m);

      // The client waits it out; the phone, looking elsewhere, has it as Failed.
      await page.evaluate((p) => window.__illogical.client.setActive(p), home);
      const done = await call("wait", { pane, until: "command_end", timeout: 30 });
      expect(done.exit).toBe(1);
      await expect.poll(() => reasonOf(page, pane), { timeout: 15_000 }).toMatchObject({ kind: "failed" });
      // Needs you on the phone's sheet; a tap there opens it.
      await page.locator(".sheet-button").tap();
      await expect(page.locator(`[data-wants="${pane}"]`)).toContainText("failed (exit 1)");
      await page.locator(`[data-wants="${pane}"] .sheet-item`).tap();
      await expect.poll(() => page.evaluate(() => window.__illogical.client.active())).toBe(pane);

      // It reads why, fixes it and reruns in the same pane; the phone sees it pass.
      const out = await call("read_output", { pane, last_command: true });
      expect(JSON.stringify(out)).toContain("ready is missing");
      writeFileSync(join(dir, "ready"), "");
      await call("send_input", { pane, text: build, enter: true });
      const again = await call("wait", { pane, until: "command_end", timeout: 30 });
      expect(again.exit).toBe(0);
      await expect(page.locator(`[data-pane="${pane}"] .xterm-rows`)).toContainText("BUILD-OK");
      const history = await (await request.get(`/api/history?pane=${pane}`)).json();
      expect(history.map((h: { exit: number; by: string }) => [h.exit, h.by])).toEqual([
        [1, "mcp:claude-code"],
        [0, "mcp:claude-code"],
      ]);
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  // The real Claude Code (haiku) with `illogical mcp`, as agents_real.rs's
  // mcp-cc, watched from the phone. It costs money: only with a key.
  test("the real Claude Code runs a build over MCP, watched from the phone", async ({ page }) => {
    const key = process.env.ANTHROPIC_API_KEY;
    const claude = spawnSync("sh", ["-c", "command -v claude"], { encoding: "utf8" }).stdout.trim();
    test.skip(!key || !claude, "needs ANTHROPIC_API_KEY and claude on PATH");
    test.setTimeout(300_000);
    const state = process.env.ILLOGICAL_E2E_STATE;
    test.skip(!state, "needs the run's own daemon (not E2E_BASE_URL)");
    await open(page);
    const dir = mkdtempSync(join(tmpdir(), "ilg-e2e-mcp-real-"));
    try {
      spawnSync("git", ["init", "-q", dir]);
      const config = join(dir, "mcp.json");
      writeFileSync(config, JSON.stringify({ mcpServers: { illogical: { command: resolve("../target/debug/illogical"), args: ["--socket", join(state!, "sock"), "mcp"] } } }));
      const build = `sleep 15; test -f ${dir}/ready && echo BUILD-OK || { echo 'error: ${dir}/ready is missing (touch it)'; false; }`;
      const prompt =
        `Use the illogical MCP tools. Run this build with the run tool, with wait true: \`${build}\`. ` +
        "If it fails, read why, fix it, and run the build again until it succeeds (wait again if it's still running). Then reply with just the word DONE.";
      const env = Object.fromEntries(Object.entries(process.env).filter(([k]) => !k.startsWith("CLAUDE") || k === "CLAUDE_CONFIG_DIR"));
      const agent = spawn(
        claude,
        ["-p", "--model", "haiku", "--strict-mcp-config", "--setting-sources", "local", "--mcp-config", config, "--allowedTools", "mcp__illogical__*", "--output-format", "text", prompt],
        { cwd: dir, env, stdio: ["ignore", "pipe", "pipe"] },
      );
      let said = "";
      agent.stdout!.on("data", (d) => (said += d));
      agent.stderr!.on("data", (d) => (said += d));
      const exited = new Promise((r) => agent.on("exit", r));
      // The pane it starts shows up on the phone, and the phone watches it.
      const theirs = () => page.evaluate(() => window.__illogical.client.state!.panes.find((p) => p.started_by?.by === "mcp:claude-code")?.id ?? null);
      await expect.poll(theirs, { timeout: 120_000 }).not.toBeNull();
      const pane = (await theirs())!;
      await page.evaluate((p) => {
        const c = window.__illogical.client;
        c.selectTab(c.tabOfPane(p)!.id);
        c.setActive(p);
      }, pane);
      await expect(page.locator(`[data-pane="${pane}"] .started-by`)).toHaveText("started by mcp:claude-code");
      await expect(page.locator(`[data-pane="${pane}"] .xterm-rows`)).toContainText("BUILD-OK", { timeout: 240_000 });
      await exited;
      expect(said).toContain("DONE");
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });
});
