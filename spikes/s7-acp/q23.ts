// Q2/Q3: permissions, client terminals and fs.
// env: CAPS=plain|meta|none  PERM=allow|always|reject|cancelled|hold  DELAY=ms  PROMPT=bash|fs|both  SLOW=ms  KILLTERM=ms  LOG=name
import { claude, init, kinds, newClaudeSession, CLIENT_CAPS, SCRATCH, ts } from "./acp.ts";

const caps = process.env.CAPS ?? "plain";
const permMode = (process.env.PERM ?? "allow") as any;
const delay = Number(process.env.DELAY ?? 0);
const perm =
  permMode === "always" ? { mode: "allow", kind: "allow_always", delayMs: delay } :
  permMode === "hold" ? { mode: "hold" } : { mode: permMode, delayMs: delay };
const a = claude(`${process.env.LOG ?? "q23"}.ndjson`, {
  quiet: true, perm: perm as any,
  slowTerminalMs: Number(process.env.SLOW ?? 0) || undefined,
  killTerminalAfterMs: Number(process.env.KILLTERM ?? 0) || undefined,
});
a.onRequest = (m) => console.log(ts(), "REQ", m.method, JSON.stringify(m.params).slice(0, 1500));
a.onUpdate = (u) => {
  const k = u.update.sessionUpdate;
  if (k === "tool_call" || k === "tool_call_update") console.log(ts(), "UPD", JSON.stringify(u.update).slice(0, 1200));
};
const capObj = caps === "none" ? {} : caps === "delta" ? CLIENT_CAPS({ _meta: { terminal_output_delta: true } }) : caps === "meta" ? CLIENT_CAPS({ _meta: { terminal_output: true } }) : CLIENT_CAPS();
await init(a, capObj);
const s = await newClaudeSession(a);
const prompts: Record<string, string> = {
  bash: "Use the Bash tool to run exactly: echo hi > x.txt   Then use Bash to run: ls   Reply with the ls output only.",
  fs: "Use the Write tool to create the file y.txt containing the word yo. Then use the Read tool to read y.txt. Reply with its content only.",
  sleep: "Use the Bash tool to run exactly: sleep 20; echo done > z.txt   Reply with OK when it finishes.",
};
for (const p of (process.env.PROMPT ?? "bash").split(",")) {
  const mark = a.updates.length;
  const t = Date.now();
  const r = await a.request("session/prompt", { sessionId: s.sessionId, prompt: [{ type: "text", text: prompts[p] ?? p }] });
  console.log(ts(), "TURN", p, Date.now() - t, "ms", r.stopReason, JSON.stringify(kinds(a.updates.slice(mark))));
}
console.log("INCOMING", a.incoming.map((m) => m.method).join(","));
console.log("SID", s.sessionId);
a.close();
await a.exited;
