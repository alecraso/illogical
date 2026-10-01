// Q2: one long-lived process, two turns over stream-json stdin.
import { start, log, DIR } from "./lib.ts";

const c = start(["--tools", "Bash", "--allowedTools", "Bash(echo *)", "--replay-user-messages"], DIR + "samples/multiturn.ndjson");
log("pid", c.proc.pid);
c.send("Remember the code word PELICAN. Then run `echo turn1` with Bash and reply with one word.");
await c.waitFor((e) => e.type === "result");
log("turn 1 done, pid still", c.proc.pid, "exited", c.exited);
c.send("What was the code word? Answer with just the word.");
const r2 = await c.waitFor((e) => e.type === "result" && c.results().length === 2);
log("turn 2 result", r2?.result, "pid", c.proc.pid);
const types = new Map<string, number>();
for (const e of c.events) { const k = [e.type, e.subtype].filter(Boolean).join("/"); types.set(k, (types.get(k) ?? 0) + 1); }
log("event types", JSON.stringify([...types]));
log("total cost", c.cost);
c.proc.stdin.end();
await c.proc.exited;
log("exit after stdin EOF between turns:", c.exited);
