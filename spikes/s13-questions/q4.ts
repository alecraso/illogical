// Q4: codex-acp with elicitation declared.  CONFIG=collaboration_mode=plan,reasoning_effort=low  NAME=<log name>
import { Acp, init, kinds, text, SCRATCH, WORK, ts, sleep } from "./acp.ts";
const MODE = process.env.NAME;
const bin = `${WORK}/codex/node_modules/.bin/codex-acp`;
const a = new Acp({ cmd: bin, args: [], env: { CODEX_PATH: `${process.env.HOME}/.local/bin/codex` }, log: `${WORK}/q4-${MODE ?? "default"}.ndjson`, quiet: true, perm: { mode: "reject" },
  custom: { "elicitation/create": async (m) => {
    console.log(ts(), "ELICITATION/CREATE", JSON.stringify(m, null, 1));
    await sleep(2000);
    const props = m.params.requestedSchema.properties;
    const k = Object.keys(props).find((k) => props[k].oneOf);
    const r = { action: "accept", content: k ? { [k]: props[k].oneOf[1].const } : {} };
    console.log(ts(), "OUR RESPONSE", JSON.stringify(r));
    return r;
  } } });
a.onRequest = (m) => { if (m.method !== "elicitation/create") console.log(ts(), "REQ", m.method, JSON.stringify(m.params).slice(0, 1500)); };
a.onNote = (m) => console.log(ts(), "NOTE", JSON.stringify(m).slice(0, 500));
a.onUpdate = (u) => { const k = u.update.sessionUpdate; if (!k.endsWith("_chunk") && k !== "usage_update") console.log(ts(), "UPD", JSON.stringify(u.update).slice(0, 1500)); };
const ini = await init(a, { fs: { readTextFile: false, writeTextFile: false }, terminal: false, elicitation: { form: {}, url: {} } });
console.log("INIT", JSON.stringify(ini).slice(0, 600));
const s = await a.request("session/new", { cwd: SCRATCH, mcpServers: [] });
console.log("NEW modes", JSON.stringify(s.modes), "configOptions", JSON.stringify(s.configOptions?.map((o: any) => [o.id, o.currentValue, (o.options ?? []).map((x: any) => x.value ?? x.id)])).slice(0, 1500));
for (const kv of (process.env.CONFIG ?? "").split(",").filter(Boolean)) {
  const [configId, value] = kv.split("=");
  const r = await a.request("session/set_config_option", { sessionId: s.sessionId, configId, value }).catch((e) => e);
  console.log("SET", configId, value, JSON.stringify(r).slice(0, 200));
}
const t = Date.now();
const r = await a.request("session/prompt", { sessionId: s.sessionId, prompt: [{ type: "text", text: process.env.PROMPT ?? "Before doing anything else, ask me a multiple-choice question using your tool for asking the user questions (request_user_input if you have it): which colour do I prefer, red or blue? Wait for my answer, then reply with my choice in one line and stop. Do not run any commands." }] });
console.log(ts(), "TURN", Date.now() - t, "ms", JSON.stringify(r).slice(0, 400), JSON.stringify(kinds(a.updates)));
console.log("REPLY", JSON.stringify(text(a.updates)));
console.log("INCOMING", a.incoming.map((m) => m.method).join(","));
a.close(); await Promise.race([a.exited, sleep(5000)]); a.proc.kill(); process.exit(0);
