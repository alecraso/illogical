// Q6: a tiny stdio MCP server whose tools ask the user through MCP elicitation (form and url mode).
// Run by claude-agent-acp from session/new mcpServers. Needs work/mcp (npm i @modelcontextprotocol/sdk).
import { createRequire } from "node:module";
const require = createRequire(new URL("./work/mcp/node_modules/", import.meta.url));
const { McpServer } = require("@modelcontextprotocol/sdk/server/mcp.js");
const { StdioServerTransport } = require("@modelcontextprotocol/sdk/server/stdio.js");
const { appendFileSync } = require("node:fs");
const log = (m) => appendFileSync(new URL("./work/mcp-elicit.log", import.meta.url), `${new Date().toISOString()} ${JSON.stringify(m)}\n`);

const server = new McpServer({ name: "s12", version: "0.0.1" }, { capabilities: {} });
server.tool("pick_size", "Ask the user for a t-shirt size and a quantity, through a form.", {}, async () => {
  const r = await server.server.elicitInput({
    mode: "form",
    message: "Order details",
    requestedSchema: {
      type: "object",
      properties: {
        size: { type: "string", title: "Size", enum: ["S", "M", "L"], enumNames: ["Small", "Medium", "Large"] },
        qty: { type: "integer", title: "Quantity", minimum: 1, maximum: 9 },
        gift: { type: "boolean", title: "Gift wrap" },
      },
      required: ["size"],
    },
  });
  log({ tool: "pick_size", result: r });
  return { content: [{ type: "text", text: `elicitation result: ${JSON.stringify(r)}` }] };
});
server.tool("sign_in", "Ask the user to open a sign-in link.", {}, async () => {
  const elicitationId = "s12-signin-1";
  const r = await server.server.elicitInput({ mode: "url", message: "Sign in to S12", url: "https://example.com/s12-signin", elicitationId });
  log({ tool: "sign_in", result: r });
  if (r.action === "accept") await server.server.createElicitationCompletionNotifier(elicitationId)();
  return { content: [{ type: "text", text: `url elicitation result: ${JSON.stringify(r)}` }] };
});
await server.connect(new StdioServerTransport());
log({ started: true, clientCaps: server.server.getClientCapabilities?.() });
