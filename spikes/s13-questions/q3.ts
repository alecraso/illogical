// Q3: fountain acp with elicitation declared.  STEP=basic (answer after 3s) | hold (never answer, cap 6 min)
import { Acp, init, kinds, text, WORK, ts, sleep } from "./acp.ts";
import { resolve } from "node:path";
const AGENT = process.env.AGENT ?? "f0577ee9-a870-433e-a866-5f4e96e93826";
const step = process.env.STEP ?? "basic";
const a = new Acp({ cmd: `${process.env.HOME}/.local/bin/fountain`, args: ["acp", "--agent", AGENT, "--log-level", "debug"], log: resolve(WORK, `q3-${step}.ndjson`), quiet: true, perm: step === "hold" ? { mode: "hold" } : { mode: "allow", delayMs: 3000 }, cwd: WORK,
  custom: { "elicitation/create": async (m) => {
    console.log(ts(), "ELICITATION/CREATE", JSON.stringify(m));
    if (step === "hold") return new Promise(() => {});
    await sleep(3000);
    const r = { action: "accept", content: { question_0: m.params.requestedSchema.properties.question_0.oneOf[1].const } };
    console.log(ts(), "OUR RESPONSE", JSON.stringify(r));
    return r;
  } } });
a.onRequest = (m) => { if (m.method !== "elicitation/create") console.log(ts(), "REQ", m.method, JSON.stringify(m)); };
a.onNote = (m) => console.log(ts(), "NOTE", JSON.stringify(m).slice(0, 500));
a.onUpdate = (u) => { const k = u.update.sessionUpdate; if (k.startsWith("tool_call")) console.log(ts(), "UPD", JSON.stringify(u.update).slice(0, 1500)); };
const ini = await init(a, { fs: { readTextFile: false, writeTextFile: false }, terminal: false, elicitation: { form: {}, url: {} } });
console.log("INIT", JSON.stringify(ini));
const s = await a.request("session/new", { cwd: WORK, mcpServers: [] });
console.log("SID", s.sessionId);
const t = Date.now();
const pr = a.request("session/prompt", { sessionId: s.sessionId, prompt: [{ type: "text", text: "Before doing anything, use the AskUserQuestion tool to ask me which colour I prefer, red or blue (one question, single choice). Then reply with my choice in one line and stop." }] });
const r = await Promise.race([pr, sleep(370000).then(() => "TIMEOUT-6min")]);
console.log(ts(), "TURN", Date.now() - t, "ms", JSON.stringify(r).slice(0, 300), JSON.stringify(kinds(a.updates)));
console.log("REPLY", JSON.stringify(text(a.updates)));
const costs = a.updates.filter((u) => u.update.sessionUpdate === "usage_update" && u.update.cost).map((u) => u.update.cost.amount);
console.log("COST", costs.at(-1), "INCOMING", a.incoming.map((m) => m.method).join(","));
a.close(); await Promise.race([a.exited, sleep(5000)]); a.proc.kill(); process.exit(0);
