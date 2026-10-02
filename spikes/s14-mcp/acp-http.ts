// S14: does claude-agent-acp pass mcpServers of type "http" (with headers) as
// well as stdio? Three servers in one session/new:
//   s14http   {type:"http", url, headers:[Authorization]}  -> the HTTP spike server on :7950
//   s14stdio  {name, command, args, env}  (no type field)   -> s14-mcp stdio
//   s14typed  {type:"stdio", name, command, ...}            -> s14-mcp stdio (explicit type)
// The prompt asks the model to list its mcp__ tools; the spike servers' logs
// (work/acp-http.log, work/acp-stdio.log, work/acp-typed.log) show who connected.
import { claude, init, text, SCRATCH, ts } from "./acp.ts";
import { resolve } from "node:path";

const bin = resolve(import.meta.dir, "target/debug/s14-mcp");
const log = (n: string) => [{ name: "S14_LOG", value: resolve(import.meta.dir, `work/acp-${n}.log`) }];
const a = claude("acp-http.ndjson", { quiet: true, perm: { mode: "allow" } });
const ini = await init(a, { fs: { readTextFile: false, writeTextFile: false }, terminal: false });
console.log(ts(), "agent mcpCapabilities", JSON.stringify(ini.agentCapabilities?.mcpCapabilities));
const mcpServers = [
  { type: "http", name: "s14http", url: "http://127.0.0.1:7950/mcp", headers: [{ name: "Authorization", value: "Bearer s14-block-token" }] },
  { name: "s14stdio", command: bin, args: ["stdio"], env: log("stdio") },
  { type: "stdio", name: "s14typed", command: bin, args: ["stdio"], env: log("typed") },
];
const s = await a.request("session/new", { cwd: SCRATCH, mcpServers, _meta: { claudeCode: { options: { settingSources: [] } } } });
await a.request("session/set_config_option", { sessionId: s.sessionId, configId: "model", value: "haiku" });
const r = await a.request("session/prompt", {
  sessionId: s.sessionId,
  prompt: [{ type: "text", text: "Call mcp__s14http__summary_and_structured once. Then list the names of every tool you have whose name starts with mcp__ (use ToolSearch if they are deferred), one per line, and stop." }],
});
console.log(ts(), "TURN", JSON.stringify(r).slice(0, 200));
console.log("REPLY", text(a.updates));
a.close();
process.exit(0);
