// Q1: AskUserQuestion as an ACP form elicitation through claude-agent-acp.
// env: V=85|81 (adapter version), CAPS=objects|bools|none, CASE=single|multi|preview|other|decline|cancelaction|stop|nocaps
import { Acp, claude, init, kinds, text, newClaudeSession, SCRATCH, ts, sleep } from "./acp.ts";
import { readFileSync, existsSync } from "node:fs";
import { homedir } from "node:os";

const V = process.env.V ?? "85";
const CASE = process.env.CASE ?? "single";
const CAPS = process.env.CASE === "nocaps" ? "none" : (process.env.CAPS ?? "objects");
const elic = CAPS === "objects" ? { form: {}, url: {} } : CAPS === "bools" ? { form: true, url: true } : undefined;

const PROMPTS: Record<string, string> = {
  single: "Before doing anything, use the AskUserQuestion tool to ask me which colour I prefer, red or blue (one question, single choice). Then reply with my choice in one line and stop.",
  other: "Before doing anything, use the AskUserQuestion tool to ask me which colour I prefer, red or blue (one question, single choice). Then reply with my answer in one line and stop.",
  decline: "Before doing anything, use the AskUserQuestion tool to ask me which colour I prefer, red or blue (one question, single choice). Then reply in one line saying what I chose (or that I didn't), and stop.",
  cancelaction: "Before doing anything, use the AskUserQuestion tool to ask me which colour I prefer, red or blue (one question, single choice). Then reply in one line saying what I chose (or that I didn't), and stop.",
  stop: "Before doing anything, use the AskUserQuestion tool to ask me which colour I prefer, red or blue (one question, single choice). Then reply with my choice in one line and stop.",
  nocaps: "Before doing anything, use the AskUserQuestion tool to ask me which colour I prefer, red or blue (one question, single choice). If you can't use that tool, say so in one line and stop. Then reply with my choice in one line and stop.",
  multi: "Before doing anything, call the AskUserQuestion tool ONCE with three questions: (1) header 'Colour': which colour I prefer, red or blue (single choice); (2) header 'Fruit': which fruits I like, apple, pear or plum (multiSelect: true); (3) header 'Pet': which pet I prefer, cat or dog (single choice). Give each option a short description. Then reply with one line summarising exactly what I answered (including any notes), and stop.",
  preview: "Before doing anything, call the AskUserQuestion tool with one question 'Which layout do you prefer?' (header 'Layout') and two options, 'Sidebar' and 'Top bar'. Give EACH option a `preview` field containing a 3-line ASCII mockup of that layout. Then reply with my choice in one line and stop.",
};

let elicitationAt = 0;
function answer(m: any): any {
  const props = m.params.requestedSchema?.properties ?? {};
  const first = (k: string) => props[k]?.oneOf?.[0]?.const;
  switch (CASE) {
    case "single": case "preview": return { action: "accept", content: { question_0: first("question_0") } };
    case "other": return { action: "accept", content: { question_0_custom: "green, actually" } };
    case "decline": return { action: "decline" };
    case "cancelaction": return { action: "cancel" };
    case "multi": {
      const fruits = (props.question_1?.items?.anyOf ?? []).map((o: any) => o.const);
      return { action: "accept", content: {
        question_0: first("question_0"), question_0_custom: "dark red please",
        question_1: [fruits[0], fruits[2]], question_1_custom: "kiwi",
        question_2_custom: "a parrot",
      } };
    }
  }
  return null;
}

const a: Acp = claude(`q1-v${V}-${CASE}-${CAPS}.ndjson`, { quiet: true, perm: { mode: "reject" }, custom: {
  "elicitation/create": async (m) => {
    elicitationAt = Date.now();
    console.log(ts(), "ELICITATION/CREATE", JSON.stringify(m, null, 1));
    if (CASE === "stop") return new Promise(() => {}); // answered by hand below
    const r = answer(m);
    await sleep(1500);
    console.log(ts(), "OUR RESPONSE", JSON.stringify({ jsonrpc: "2.0", id: m.id, result: r }));
    return r;
  },
} });
a.onRequest = (m) => { if (m.method !== "elicitation/create") console.log(ts(), "REQ", m.method, JSON.stringify(m.params).slice(0, 1500)); };
a.onNote = (m) => console.log(ts(), "NOTE", JSON.stringify(m));
a.onUpdate = (u) => {
  const k = u.update.sessionUpdate;
  if (k === "tool_call" || k === "tool_call_update") console.log(ts(), "UPD", JSON.stringify(u.update).slice(0, 3000));
};

const caps: any = { fs: { readTextFile: false, writeTextFile: false }, terminal: false, _meta: { terminal_output: true } };
if (elic) caps.elicitation = elic;
let ini: any;
try { ini = await init(a, caps); } catch (e) { console.log("INIT-ERR", JSON.stringify(e)); process.exit(1); }
console.log("INIT agentInfo", JSON.stringify(ini.agentInfo), "caps sent", JSON.stringify(caps.elicitation ?? null));
const s = await newClaudeSession(a);
const sid = s.sessionId;
console.log("SID", sid);
const t = Date.now();
const pr = a.request("session/prompt", { sessionId: sid, prompt: [{ type: "text", text: PROMPTS[CASE] }] });
if (CASE === "stop") {
  while (!a.incoming.some((m) => m.method === "elicitation/create")) await sleep(100);
  const el = a.incoming.find((m) => m.method === "elicitation/create");
  await sleep(2000);
  console.log(ts(), "SEND session/cancel");
  a.notify("session/cancel", { sessionId: sid });
  await sleep(3000);
  console.log(ts(), "notes so far", JSON.stringify(a.notes));
  const late = { action: "cancel" };
  console.log(ts(), "LATE ANSWER to", el.id, JSON.stringify(late));
  a.send({ jsonrpc: "2.0", id: el.id, result: late });
}
const r = await pr;
console.log(ts(), "TURN", Date.now() - t, "ms", JSON.stringify(r).slice(0, 300), JSON.stringify(kinds(a.updates)));
console.log("REPLY", JSON.stringify(text(a.updates)));
if (CASE === "stop") {
  const m = a.updates.length;
  const r2 = await a.request("session/prompt", { sessionId: sid, prompt: [{ type: "text", text: "Did I answer your question? One line." }] });
  console.log(ts(), "TURN2", r2.stopReason, JSON.stringify(text(a.updates.slice(m))));
}
const costs = a.updates.filter((u) => u.update.sessionUpdate === "usage_update" && u.update.cost).map((u) => u.update.cost.amount);
console.log("COST", costs.at(-1));
console.log("INCOMING", a.incoming.map((m) => m.method).join(","), "NOTES", a.notes.map((m) => m.method).join(","));
// transcript: the AskUserQuestion tool_use and tool_result as Claude Code stored them
const dir = `${homedir()}/.claude/projects/${SCRATCH.replace(/[^A-Za-z0-9]/g, "-")}`;
const f = `${dir}/${sid}.jsonl`;
if (existsSync(f)) {
  for (const line of readFileSync(f, "utf8").split("\n").filter(Boolean)) {
    const e = JSON.parse(line);
    const c = e.message?.content;
    if (!Array.isArray(c)) continue;
    for (const b of c) {
      if ((b.type === "tool_use" && b.name === "AskUserQuestion") || (b.type === "tool_result")) console.log("TRANSCRIPT", e.type, JSON.stringify(b).slice(0, 2000), e.toolUseResult ? "toolUseResult=" + JSON.stringify(e.toolUseResult).slice(0, 2000) : "");
    }
  }
} else console.log("no transcript at", f);
a.close();
await Promise.race([a.exited, sleep(5000)]);
a.proc.kill();
process.exit(0);
