// Q4a: what claude does when the process owning its pipes goes away.
// usage: bun eof.ts midturn   close stdin right after sending (turn needs a 10s permission hold)
//        bun eof.ts orphan    SIGKILL this harness (stdin EOF + stdout reader gone) while a
//                             permission request is held; watch claude from outside
import { start, log, DIR, sleep } from "./lib.ts";

const kind = process.argv[2] ?? "midturn";
const BUN = "/home/me/.local/share/mise/installs/bun/1.4.2/bin/bun";
const permLog = `${DIR}samples/eof-${kind}.perm.ndjson`;
const mcp = JSON.stringify({ mcpServers: { perm: { type: "stdio", command: BUN, args: [DIR + "perm-mcp.ts"],
  env: { PERM_MODE: "hold:10", PERM_LOG: permLog } } } });
const c = start(["--tools", "Bash", "--mcp-config", mcp, "--permission-prompt-tool", "mcp__perm__approve"],
  `${DIR}samples/eof-${kind}.ndjson`);
console.log(JSON.stringify({ claude_pid: c.proc.pid }));
c.send(`Run exactly \`echo ${kind} > eof-${kind}.txt\` with the Bash tool, then say done.`);

if (kind === "midturn") {
  await sleep(200);
  c.proc.stdin.end();
  log("stdin closed mid-turn");
  await c.proc.exited;
  log("results", c.results().length, "cost", c.cost);
} else {
  await c.waitFor((e) => e.type === "assistant" && e.message.content.some((b: any) => b.type === "tool_use"));
  log("tool_use seen, permission held; harness SIGKILLs itself");
  process.kill(process.pid, "SIGKILL");
}
