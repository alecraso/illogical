// Q6: MCP elicitations (form and url) from a local stdio MCP server through claude-agent-acp.
import { Acp, claude, init, kinds, text, newClaudeSession, SCRATCH, ts, sleep } from "./acp.ts";
import { resolve } from "node:path";
const a: Acp = claude(`q6-v${process.env.V ?? "85"}.ndjson`, { quiet: true, perm: { mode: "allow" }, custom: {
  "elicitation/create": async (m) => {
    console.log(ts(), "ELICITATION/CREATE", JSON.stringify(m));
    await sleep(1000);
    const r = m.params.mode === "url" ? { action: "accept" } : { action: "accept", content: { size: "M", qty: 2, gift: true } };
    console.log(ts(), "OUR RESPONSE", JSON.stringify(r));
    return r;
  } } });
a.onRequest = (m) => { if (m.method !== "elicitation/create") console.log(ts(), "REQ", m.method, JSON.stringify(m.params).slice(0, 600)); };
a.onNote = (m) => { if (!m.method.startsWith("_auth")) console.log(ts(), "NOTE", JSON.stringify(m)); };
a.onUpdate = (u) => { const k = u.update.sessionUpdate; if (k.startsWith("tool_call")) console.log(ts(), "UPD", JSON.stringify(u.update).slice(0, 600)); };
await init(a, { fs: { readTextFile: false, writeTextFile: false }, terminal: false, elicitation: { form: {}, url: {} } });
const mcpServers = [{ name: "s12", command: "node", args: [resolve(import.meta.dir, "mcp-elicit.mjs")], env: [] }];
const s = await a.request("session/new", { cwd: SCRATCH, mcpServers, _meta: { claudeCode: { options: { settingSources: [] } } } });
await a.request("session/set_config_option", { sessionId: s.sessionId, configId: "model", value: "haiku" });
const r = await a.request("session/prompt", { sessionId: s.sessionId, prompt: [{ type: "text", text: "Call the mcp__s12__pick_size tool, then the mcp__s12__sign_in tool, then report both results in one line each and stop." }] });
console.log(ts(), "TURN", JSON.stringify(r).slice(0, 200), JSON.stringify(kinds(a.updates)));
console.log("REPLY", JSON.stringify(text(a.updates)));
const costs = a.updates.filter((u) => u.update.sessionUpdate === "usage_update" && u.update.cost).map((u) => u.update.cost.amount);
console.log("COST", costs.at(-1), "INCOMING", a.incoming.map((m) => m.method).join(","), "NOTES", a.notes.map((m) => m.method).join(","));
a.close(); await Promise.race([a.exited, sleep(5000)]); a.proc.kill(); process.exit(0);
