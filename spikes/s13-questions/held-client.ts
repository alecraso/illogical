// Client for held-pipes.sh (Q2): attaches to the held FIFOs directly.
import { Acp, init, kinds, text, newClaudeSession, WORK, ts, sleep } from "./acp.ts";
import { readFileSync, writeFileSync } from "node:fs";

const D = `${WORK}/held`;
const step = process.env.STEP;
const a = new Acp({
  cmd: "", fifo: { in: `${D}/in`, out: `${D}/out` },
  log: `${WORK}/q2-client${step}.ndjson`, quiet: true, perm: { mode: "hold" },
  custom: { "elicitation/create": () => new Promise(() => {}) }, // never answered by client #1
});
a.onRequest = (m) => console.log(ts(), `#${step} REQ`, m.method, m.id, JSON.stringify(m.params).slice(0, 300));
a.onNote = (m) => { if (!m.method.startsWith("_auth")) console.log(ts(), `#${step} NOTE`, JSON.stringify(m)); };
a.onUpdate = (u) => { const k = u.update.sessionUpdate; if (k.startsWith("tool_call")) console.log(ts(), `#${step} UPD`, JSON.stringify(u.update).slice(0, 400)); };

if (step === "1") {
  await init(a, { fs: { readTextFile: false, writeTextFile: false }, terminal: false, elicitation: { form: {}, url: {} } });
  const s = await newClaudeSession(a);
  writeFileSync(`${D}/sid`, s.sessionId);
  await a.request("session/prompt", { sessionId: s.sessionId, prompt: [{ type: "text", text: "My favourite bird is the merlin. Reply with just OK." }] });
  a.request("session/prompt", { sessionId: s.sessionId, prompt: [{ type: "text", text: "Before doing anything, use the AskUserQuestion tool to ask me which colour I prefer, red or blue (one question). Then reply in one line with my colour and my favourite bird, and stop." }] });
  while (!a.incoming.length) await sleep(100);
  const el = a.incoming[0];
  // what the daemon would persist in its block log
  writeFileSync(`${D}/pending.json`, JSON.stringify({ elicitationRequestId: el.id, params: el.params, nextId: a.nextId }));
  console.log(ts(), "#1 SIGKILL self with elicitation", el.id, "unanswered; prompt id", a.nextId - 1, "outstanding");
  process.kill(process.pid, "SIGKILL");
} else {
  const sid = readFileSync(`${D}/sid`, "utf8");
  const p = JSON.parse(readFileSync(`${D}/pending.json`, "utf8"));
  a.nextId = 100;
  await sleep(500);
  console.log(ts(), "#2 backlog frames on attach:", a.updates.length + a.incoming.length + a.notes.length);
  const answer = { action: "accept", content: { question_0: p.params.requestedSchema.properties.question_0.oneOf[1].const } };
  console.log(ts(), "#2 answering old id", p.elicitationRequestId, JSON.stringify(answer));
  a.send({ jsonrpc: "2.0", id: p.elicitationRequestId, result: answer });
  const t = Date.now();
  let done = false;
  while (Date.now() - t < 60000 && !done) {
    done = readFileSync(`${WORK}/q2-client2.ndjson`, "utf8").split("\n").some((l) => l.includes(`"id":${p.nextId - 1},"result"`));
    await sleep(200);
  }
  console.log(ts(), "#2 orphaned turn done?", done, JSON.stringify(kinds(a.updates)), JSON.stringify(text(a.updates)));
  const m = a.updates.length;
  const r = await a.request("session/prompt", { sessionId: sid, prompt: [{ type: "text", text: "What colour did I pick, and what is my favourite bird? One line." }] });
  console.log(ts(), "#2 turn3", r.stopReason, JSON.stringify(text(a.updates.slice(m))));
  const costs = a.updates.filter((u) => u.update.sessionUpdate === "usage_update" && u.update.cost).map((u) => u.update.cost.amount);
  console.log("COST", costs.at(-1));
  a.proc.kill();
  process.exit(0);
}
