// M16: a command an MCP client runs shows who started it. Claude Code (here,
// its requests by hand over /mcp) runs a command; the pane it made says
// "started by mcp:claude-code", and history records the command as theirs.

import { expect, test, type APIRequestContext } from "@playwright/test";
import { reset, ready, screen } from "./helpers";

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
