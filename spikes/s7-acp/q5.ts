// Q5: fountain acp as an agent host.
// STEP=basic   : init, session/new, prompt, approval round trip (execute=ask), second prompt shares context
// STEP=kill    : session/new, prompt with a pending permission + a slow command, SIGKILL the adapter mid-turn; prints SID
// STEP=load    : SID=<id> fresh process, session/load, then a prompt
// STEP=hold    : never answer a permission request; measure when the server refuses
// env: AGENT, PERM (fountain --permission value), PROFILE
import { Acp, init, kinds, text, CLIENT_CAPS, WORK, ts, sleep } from "./acp.ts";
import { resolve } from "node:path";

const AGENT = process.env.AGENT ?? "f0577ee9-a870-433e-a866-5f4e96e93826";
const step = process.env.STEP ?? "basic";
const args = ["acp", "--agent", AGENT, "--log-level", "debug"];
if (process.env.PERM) args.push("--permission", process.env.PERM);
if (process.env.PROFILE) args.unshift("--profile", process.env.PROFILE);
const permPolicy: any = step === "hold" ? { mode: "hold" } : step === "kill" ? { mode: "hold" } : { mode: "allow", delayMs: Number(process.env.DELAY ?? 2000) };
const a = new Acp({ cmd: `${process.env.HOME}/.local/bin/fountain`, args, log: resolve(WORK, `q5-${step}.ndjson`), quiet: true, perm: permPolicy, cwd: WORK });
a.onRequest = (m) => console.log(ts(), "REQ", m.method, JSON.stringify(m.params).slice(0, 2000));
a.onUpdate = (u) => {
  const k = u.update.sessionUpdate;
  if (k !== "agent_message_chunk" && k !== "agent_thought_chunk") console.log(ts(), "UPD", JSON.stringify(u.update).slice(0, 700));
};
const ini = await init(a, CLIENT_CAPS());
console.log("INIT", JSON.stringify(ini));

async function prompt(sid: string, p: string, meta?: any) {
  const mark = a.updates.length, t = Date.now();
  try {
    const r = await a.request("session/prompt", { sessionId: sid, prompt: [{ type: "text", text: p }], ...(meta ? { _meta: meta } : {}) });
    console.log(ts(), "TURN", Date.now() - t, "ms", JSON.stringify(r), JSON.stringify(kinds(a.updates.slice(mark))), JSON.stringify(text(a.updates.slice(mark))).slice(0, 400));
  } catch (e) {
    console.log(ts(), "TURN-ERR", Date.now() - t, "ms", JSON.stringify(e));
  }
}

if (step === "basic") {
  const t = Date.now();
  const s = await a.request("session/new", { cwd: WORK, mcpServers: [] });
  console.log(ts(), "NEW", Date.now() - t, "ms", JSON.stringify(s));
  await prompt(s.sessionId, "Remember the code word HERON. Reply with just OK.", { clientRequestId: "s7-basic-1" });
  await prompt(s.sessionId, "Use the Bash tool to run exactly: echo hi > x.txt && ls   Then tell me the code word.");
  console.log("SID", s.sessionId);
} else if (step === "kill") {
  const s = await a.request("session/new", { cwd: WORK, mcpServers: [] });
  console.log("SID", s.sessionId);
  await prompt(s.sessionId, "Remember the code word OSPREY. Reply with just OK.");
  a.request("session/prompt", { sessionId: s.sessionId, prompt: [{ type: "text", text: "Use the Bash tool to run exactly: sleep 20; echo done > z.txt   Then reply with the code word." }] }).catch(() => {});
  // wait for the permission request (if --permission ask) or a tool call, then kill
  while (!a.incoming.length && !a.updates.some((u) => u.update.sessionUpdate === "tool_call")) await sleep(200);
  await sleep(1000);
  console.log(ts(), "KILL adapter pid", a.proc.pid);
  a.proc.kill("SIGKILL");
  await a.exited;
  process.exit(0);
} else if (step === "load") {
  const sid = process.env.SID!;
  const mark = a.updates.length, t = Date.now();
  const r = await a.request("session/load", { sessionId: sid, cwd: WORK, mcpServers: [] });
  const u = a.updates.slice(mark);
  console.log(ts(), "LOAD", Date.now() - t, "ms", JSON.stringify(r).slice(0, 500), JSON.stringify(kinds(u)));
  console.log("REPLAY-TEXT", JSON.stringify(text(u)).slice(0, 1500));
  if (process.env.WAIT) await sleep(Number(process.env.WAIT));
  await prompt(sid, "What was the code word? Did the sleep command run? One line.");
} else if (step === "hold") {
  const s = await a.request("session/new", { cwd: WORK, mcpServers: [] });
  console.log("SID", s.sessionId);
  await prompt(s.sessionId, "Use the Bash tool to run exactly: echo hi > x.txt   Then reply with whether it worked, one line.");
}
a.close();
const ex = await Promise.race([a.exited, sleep(10000).then(() => "still running")]);
console.log("EXIT", JSON.stringify(ex));
if (ex === "still running") a.proc.kill();
