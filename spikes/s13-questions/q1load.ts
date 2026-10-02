// Q1 extra: how a session/load replay shows an answered AskUserQuestion.  SID=<session id>
import { claude, init, kinds, SCRATCH, ts, sleep } from "./acp.ts";
const a = claude(`q1-load.ndjson`, { quiet: true });
await init(a, { fs: { readTextFile: false, writeTextFile: false }, terminal: false, elicitation: { form: {}, url: {} } });
await a.request("session/load", { sessionId: process.env.SID, cwd: SCRATCH, mcpServers: [], _meta: { claudeCode: { options: { settingSources: [] } } } });
console.log("KINDS", JSON.stringify(kinds(a.updates)));
for (const u of a.updates) { const k = u.update.sessionUpdate; if (k.startsWith("tool_call") || k === "user_message_chunk" || k === "agent_message_chunk") console.log(k, JSON.stringify(u.update).slice(0, 900)); }
a.close(); await Promise.race([a.exited, sleep(5000)]); a.proc.kill(); process.exit(0);
