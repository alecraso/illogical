// Q2/Q3: resume a session the CLI made, through the adapter the daemon uses.
// SID=<id> METHOD=session/resume|session/load SOURCES=project,local LOG=name
// Asks for the courier's name (only in the transcript) and the canary word as
// CLAUDE.md says it *now* (changed after the CLI session), and reports whether
// the project's UserPromptSubmit hook fired.
import { claude, init, kinds, text, SCRATCH, ts, CLAUDE_META } from "./acp.ts";
import { existsSync, readFileSync } from "node:fs";

const sid = process.env.SID!;
if (process.env.SETTINGS) (CLAUDE_META.claudeCode.options as any).settings = JSON.parse(process.env.SETTINGS);
const method = process.env.METHOD ?? "session/resume";
const hookFile = `${SCRATCH}/project-hook.fired`;
const firedBefore = existsSync(hookFile) ? readFileSync(hookFile, "utf8").split("\n").length : 0;
const a = claude(`${process.env.LOG ?? "q2"}.ndjson`, { quiet: true, perm: { mode: "allow" } });
const caps = await init(a);
console.log(ts(), "adapter", JSON.stringify(caps?.agentInfo ?? {}));
let mark = a.updates.length, t = Date.now();
const r = await a.request(method, { sessionId: sid, cwd: SCRATCH, mcpServers: [], _meta: CLAUDE_META });
console.log(ts(), method, Date.now() - t, "ms", "sources", JSON.stringify(CLAUDE_META.claudeCode.options.settingSources), JSON.stringify(kinds(a.updates.slice(mark))), JSON.stringify(r).slice(0, 120));
await a.request("session/set_config_option", { sessionId: sid, configId: "model", value: "haiku" });
mark = a.updates.length;
const r2 = await a.request("session/prompt", { sessionId: sid, prompt: [{ type: "text", text:
  "Answer from your context only, without reading any file or using any tool, in one line: (1) the courier's name, (2) the canary word as CLAUDE.md states it in your current system context (it may have changed since earlier in this conversation; if you have no CLAUDE.md in context say NONE), (3) the number I asked you to remember." }] });
console.log(ts(), "TURN", r2.stopReason, JSON.stringify(text(a.updates.slice(mark))));
const firedAfter = existsSync(hookFile) ? readFileSync(hookFile, "utf8").split("\n").length : 0;
console.log("project hook fired:", firedAfter > firedBefore);
a.close(); await a.exited;
