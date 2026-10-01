// Q4: restarts with claude-agent-acp.
// STEP=kill  AT=perm|cmd : client SIGKILLs itself mid-turn (adapter is detached, so only the pipes die).
//                          perm: while a permission request is unanswered; cmd: while the allowed command runs.
// STEP=load  SID=...     : fresh client, session/load (replay), then a prompt.
// STEP=resume SID=...    : fresh client, session/resume (no replay), then a prompt.
import { claude, init, kinds, text, newClaudeSession, SCRATCH, ts, sleep, CLAUDE_META } from "./acp.ts";
import { writeFileSync } from "node:fs";

const step = process.env.STEP ?? "kill";
const at = process.env.AT ?? "perm";
const a = claude(`q4-${step}-${at}.ndjson`, {
  quiet: true, detached: true,
  perm: at === "perm" && step === "kill" ? { mode: "hold" } : { mode: "allow" },
});
a.onRequest = (m) => console.log(ts(), "REQ", m.method, JSON.stringify(m.params).slice(0, 300));
await init(a);

if (step === "kill") {
  const s = await newClaudeSession(a);
  console.log("SID", s.sessionId, "ADAPTER", a.proc.pid);
  writeFileSync(`${SCRATCH}/../q4-${at}.sid`, `${s.sessionId} ${a.proc.pid}\n`);
  await a.request("session/prompt", { sessionId: s.sessionId, prompt: [{ type: "text", text: "My favourite bird is the kestrel. Reply with just OK." }] });
  a.request("session/prompt", { sessionId: s.sessionId, prompt: [{ type: "text", text: `Please run this with the Bash tool: sleep 15; echo done > k-${at}.txt   Afterwards remind me of my favourite bird.` }] });
  if (at === "perm") { while (!a.incoming.length) await sleep(100); await sleep(500); }
  else { while (!a.incoming.length) await sleep(100); await sleep(3000); }
  console.log(ts(), "CLIENT SIGKILL self");
  process.kill(process.pid, "SIGKILL");
} else {
  const sid = process.env.SID!;
  const mark = a.updates.length, t = Date.now();
  const method = step === "load" ? "session/load" : "session/resume";
  const r = await a.request(method, { sessionId: sid, cwd: SCRATCH, mcpServers: [], _meta: CLAUDE_META });
  const u = a.updates.slice(mark);
  console.log(ts(), method, Date.now() - t, "ms", JSON.stringify(r).slice(0, 200), JSON.stringify(kinds(u)));
  console.log("USER", JSON.stringify(text(u, "user_message_chunk")).slice(0, 600));
  console.log("AGENT", JSON.stringify(text(u)).slice(0, 600));
  for (const x of u.filter((x) => x.update.sessionUpdate.startsWith("tool_call"))) console.log("TOOL", JSON.stringify(x.update).slice(0, 400));
  await a.request("session/set_config_option", { sessionId: sid, configId: "model", value: "haiku" }).catch((e) => console.log("setmodel", e));
  const m2 = a.updates.length;
  const r2 = await a.request("session/prompt", { sessionId: sid, prompt: [{ type: "text", text: "What is my favourite bird, and did your last command finish? One line." }] });
  console.log(ts(), "TURN", JSON.stringify(r2.stopReason), JSON.stringify(text(a.updates.slice(m2))));
  a.close();
  await a.exited;
}
