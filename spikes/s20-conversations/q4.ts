// Q4: fork a session the CLI made. SID=<id>. Checks the original jsonl is
// byte-for-byte unchanged, the fork remembers the earlier context, and a fresh
// process can session/resume the fork's id. (session/fork only writes the
// new transcript; the fork has to be resumed before it can be prompted.)
import { claude, init, text, SCRATCH, ts, CLAUDE_META } from "./acp.ts";
import { readFileSync, existsSync } from "node:fs";
import { createHash } from "node:crypto";
import { homedir } from "node:os";

const sid = process.env.SID!;
const dir = `${homedir()}/.claude/projects/${SCRATCH.replace(/[^A-Za-z0-9]/g, "-")}`;
const h = (p: string) => createHash("sha256").update(readFileSync(p)).digest("hex").slice(0, 16);
const orig = `${dir}/${sid}.jsonl`, before = h(orig);
const ask = async (a: any, s: string, q: string) => {
  await a.request("session/set_config_option", { sessionId: s, configId: "model", value: "haiku" });
  const m = a.updates.length;
  await a.request("session/prompt", { sessionId: s, prompt: [{ type: "text", text: q }] });
  return text(a.updates.slice(m));
};
let a = claude("q4-fork.ndjson", { quiet: true, perm: { mode: "allow" } });
await init(a);
let t = Date.now();
const f = await a.request("session/fork", { sessionId: sid, cwd: SCRATCH, mcpServers: [], _meta: CLAUDE_META });
const fid = f.sessionId;
console.log(ts(), "fork", Date.now() - t, "ms ->", fid, "file exists:", existsSync(`${dir}/${fid}.jsonl`));
await a.request("session/resume", { sessionId: fid, cwd: SCRATCH, mcpServers: [], _meta: CLAUDE_META });
console.log("fork turn:", await ask(a, fid, "In the fork: the courier's name? Also remember that the fork's password is 'quince'. One line."));
a.close(); await a.exited;
console.log("original unchanged:", h(orig) === before);
a = claude("q4-resume-fork.ndjson", { quiet: true, perm: { mode: "allow" } });
await init(a);
t = Date.now();
await a.request("session/resume", { sessionId: fid, cwd: SCRATCH, mcpServers: [], _meta: CLAUDE_META });
console.log(ts(), "resume fork", Date.now() - t, "ms");
console.log("resumed fork:", await ask(a, fid, "Courier's name and the fork's password? One line."));
a.close(); await a.exited;
console.log("original still unchanged:", h(orig) === before);
