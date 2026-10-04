// Q5: SIGKILL a session (once between turns' worth of context, mid-turn with a
// permission request pending), then --resume it in a new process.
import { start, log, DIR, sleep } from "./lib.ts";

const BUN = "/home/me/.local/share/mise/installs/bun/1.4.2/bin/bun";
const mcp = (tag: string, mode: string) => JSON.stringify({ mcpServers: { perm: { type: "stdio", command: BUN,
  args: [DIR + "perm-mcp.ts"], env: { PERM_MODE: mode, PERM_LOG: `${DIR}samples/${tag}.perm.ndjson` } } } });
const flags = (tag: string, mode: string) => ["--tools", "Bash", "--mcp-config", mcp(tag, mode), "--permission-prompt-tool", "mcp__perm__approve"];
let total = 0;

// Process 1: turn 1 sets a code word; turn 2 is killed while its permission is held.
const a = start(flags("resume-a", "hold:60"), `${DIR}samples/resume-a.ndjson`);
a.send("Remember the code word HERON. Reply with just OK.");
const r1 = await a.waitFor((e) => e.type === "result");
const sid = r1.session_id;
log("session", sid);
a.send("Run exactly `echo killed > killed.txt` with Bash, then say done.");
await a.waitFor((e) => e.type === "assistant" && e.message.content.some((b: any) => b.type === "tool_use"));
await sleep(1000);
log("SIGKILL mid-turn, permission pending");
a.proc.kill("SIGKILL");
await a.proc.exited;
total += a.cost; // turn 2 never reported; its cost is unaccounted (small)

// Process 2: --resume, same flags.
const b = start(["--resume", sid, ...flags("resume-b", "auto")], `${DIR}samples/resume-b.ndjson`);
b.send("What was the code word? Also: did my echo killed command run? One short line.");
const r2 = await b.waitFor((e) => e.type === "result");
const init = b.events.find((e) => e.subtype === "init");
log("resumed init session", init?.session_id, "same?", init?.session_id === sid, "answer", r2?.result);
b.proc.stdin.end(); await b.proc.exited; total += b.cost;

// Process 3: --resume --fork-session.
const c = start(["--resume", sid, "--fork-session", ...flags("resume-c", "auto")], `${DIR}samples/resume-c.ndjson`);
c.send("Code word again? One word.");
const r3 = await c.waitFor((e) => e.type === "result");
log("forked session", c.events.find((e) => e.subtype === "init")?.session_id, "answer", r3?.result);
c.proc.stdin.end(); await c.proc.exited; total += c.cost;
log("total cost", total);
