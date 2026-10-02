// Q5: two writers. SID=<id> already open in an interactive CLI (tmux -L s20 -t v).
// STEP=write: resume over ACP and tell it the vault code (the CLI doesn't see it).
// STEP=check: a fresh resume afterwards, to see which writer's turns it has.
import { claude, init, text, SCRATCH, ts, CLAUDE_META } from "./acp.ts";
const sid = process.env.SID!, step = process.env.STEP ?? "write";
const a = claude(`q5-${step}.ndjson`, { quiet: true, perm: { mode: "allow" } });
await init(a);
await a.request("session/resume", { sessionId: sid, cwd: SCRATCH, mcpServers: [], _meta: CLAUDE_META });
await a.request("session/set_config_option", { sessionId: sid, configId: "model", value: "haiku" });
const m = a.updates.length;
const q = step === "write" ? "The vault code is 2468. Reply with just OK."
  : "One line: the vault code (or UNKNOWN), the lighthouse colour (or UNKNOWN), and the last two things I said before this message.";
await a.request("session/prompt", { sessionId: sid, prompt: [{ type: "text", text: q }] });
console.log(ts(), step, JSON.stringify(text(a.updates.slice(m))));
a.close(); await a.exited;
