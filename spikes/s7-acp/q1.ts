// Q1: initialize, session/new, two prompts sharing context, streaming, cancel.
import { claude, init, kinds, text, sleep, newClaudeSession } from "./acp.ts";

const a = claude("q1.ndjson", { quiet: !!process.env.QUIET });
const ini = await init(a);
console.log("INIT", JSON.stringify(ini).slice(0, 300));
const s = await newClaudeSession(a);
console.log("NEW", JSON.stringify(s).slice(0, 3000));
const sid = s.sessionId;

for (const [i, p] of ["Remember the code word PELICAN. Reply with just OK.", "What was the code word? One word."].entries()) {
  const mark = a.updates.length;
  const t = Date.now();
  const r = await a.request("session/prompt", { sessionId: sid, prompt: [{ type: "text", text: p }] });
  const u = a.updates.slice(mark);
  console.log(`TURN${i + 1}`, Date.now() - t, "ms", JSON.stringify(r), JSON.stringify(kinds(u)), JSON.stringify(text(u)));
}

// cancel: a turn that takes a while
const mark = a.updates.length;
const pr = a.request("session/prompt", { sessionId: sid, prompt: [{ type: "text", text: "Write a 400 word story about a lighthouse." }] });
await sleep(4000);
const tc = Date.now();
a.notify("session/cancel", { sessionId: sid });
const r3 = await pr;
console.log("CANCEL", Date.now() - tc, "ms after cancel", JSON.stringify(r3), JSON.stringify(kinds(a.updates.slice(mark))));

// one more prompt after cancel works?
const r4 = await a.request("session/prompt", { sessionId: sid, prompt: [{ type: "text", text: "Code word again? One word." }] });
console.log("AFTER-CANCEL", JSON.stringify(r4), JSON.stringify(text(a.updates.slice(mark))).slice(-200));
a.close();
console.log("EXIT", await a.exited);
