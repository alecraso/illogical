// Client for held-pipes.sh: attaches to the held FIFOs directly.
import { Acp, init, kinds, text, newClaudeSession, WORK, ts, sleep } from "./acp.ts";
import { readFileSync, writeFileSync } from "node:fs";

const D = `${WORK}/held`;
const step = process.env.STEP;
const a = new Acp({
  cmd: "", fifo: { in: `${D}/in`, out: `${D}/out` },
  log: `${WORK}/q4c-client${step}.ndjson`, quiet: true, perm: { mode: "hold" },
});
a.onRequest = (m) => console.log(ts(), `#${step} REQ`, m.method, m.id, JSON.stringify(m.params.toolCall?.rawInput ?? {}));

if (step === "1") {
  await init(a);
  const s = await newClaudeSession(a);
  writeFileSync(`${D}/sid`, s.sessionId);
  await a.request("session/prompt", { sessionId: s.sessionId, prompt: [{ type: "text", text: "My favourite bird is the merlin. Reply with just OK." }] });
  a.request("session/prompt", { sessionId: s.sessionId, prompt: [{ type: "text", text: "Please run this with the Bash tool: echo held > held.txt   Afterwards remind me of my favourite bird." }] });
  while (!a.incoming.length) await sleep(100);
  // the daemon would persist this in its block log
  writeFileSync(`${D}/pending.json`, JSON.stringify({ permissionRequestId: a.incoming[0].id, options: a.incoming[0].params.options, nextId: a.nextId }));
  console.log(ts(), "#1 SIGKILL self with permission", a.incoming[0].id, "unanswered; prompt id", a.nextId - 1, "outstanding");
  process.kill(process.pid, "SIGKILL");
} else {
  const sid = readFileSync(`${D}/sid`, "utf8");
  const p = JSON.parse(readFileSync(`${D}/pending.json`, "utf8"));
  a.nextId = 100; // never reuse client #1's ids
  await sleep(500);
  console.log(ts(), "#2 backlog frames on attach:", a.updates.length + a.incoming.length);
  a.answerPermission(p.permissionRequestId, { outcome: "selected", optionId: p.options.find((o: any) => o.kind === "allow_once").optionId });
  // wait for the orphaned prompt's response (id from client #1)
  const t = Date.now();
  while (Date.now() - t < 60000) {
    const done = readFileSync(`${WORK}/q4c-client2.ndjson`, "utf8").split("\n").some((l) => l.includes(`"id":${p.nextId - 1},"result"`) || l.includes(`"id":${p.nextId - 1},"jsonrpc"`) );
    if (done) break;
    await sleep(200);
  }
  console.log(ts(), "#2 orphaned turn", kinds(a.updates), JSON.stringify(text(a.updates)));
  const m = a.updates.length;
  const r = await a.request("session/prompt", { sessionId: sid, prompt: [{ type: "text", text: "What is my favourite bird, and did the last command run? One line." }] });
  console.log(ts(), "#2 turn3", r.stopReason, JSON.stringify(text(a.updates.slice(m))));
  a.proc.kill();
  process.exit(0);
}
