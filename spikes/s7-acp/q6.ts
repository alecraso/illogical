// Q6: one-prompt smoke test of another local ACP agent.  usage: bun q6.ts <name> <cmd> [args...]
import { Acp, init, kinds, text, CLIENT_CAPS, SCRATCH, WORK, ts } from "./acp.ts";
const [name, cmd, ...args] = process.argv.slice(2);
const a = new Acp({ cmd, args, log: `${WORK}/q6-${name}.ndjson`, quiet: true, perm: { mode: "reject" } });
a.onRequest = (m) => console.log(ts(), "REQ", m.method, JSON.stringify(m.params).slice(0, 600));
const ini = await init(a, CLIENT_CAPS());
console.log("INIT", JSON.stringify(ini).slice(0, 800));
const s = await a.request("session/new", { cwd: SCRATCH, mcpServers: [] });
console.log("NEW", JSON.stringify(s).slice(0, 1500));
if (process.env.MODEL) console.log("SETMODEL", JSON.stringify(await a.request("session/set_config_option", { sessionId: s.sessionId, configId: "model", value: process.env.MODEL }).catch((e) => e)).slice(0, 300));
const t = Date.now();
const r = await a.request("session/prompt", { sessionId: s.sessionId, prompt: [{ type: "text", text: process.env.PROMPT ?? "Reply with just OK." }] });
console.log(ts(), "TURN", Date.now() - t, "ms", JSON.stringify(r).slice(0, 600), JSON.stringify(kinds(a.updates)), JSON.stringify(text(a.updates)).slice(0, 300));
console.log("INCOMING", a.incoming.map((m) => m.method).join(","));
a.close();
const ex = await Promise.race([a.exited, new Promise((r) => setTimeout(() => r("still running"), 5000))]);
console.log("EXIT", JSON.stringify(ex));
if (ex === "still running") a.proc.kill();
