// Q4b: a permission request is pending while the "daemon" dies and comes back.
// claude -> perm-mcp.ts (stdio) -> Unix socket -> fake-daemon.ts
// daemon #1 receives the request and is SIGKILLed without answering; after
// DOWN seconds daemon #2 starts, the MCP server reconnects and resends, and
// daemon #2 allows it. Does claude still accept the answer?
import { start, log, DIR, sleep } from "./lib.ts";

const DOWN = Number(process.argv[2] ?? 20);
const SOCK = `${DIR}../work/perm.sock`;
const BUN = "/home/me/.local/share/mise/installs/bun/1.4.2/bin/bun";
const daemon = (mode: string) => {
  const p = Bun.spawn([BUN, DIR + "fake-daemon.ts", SOCK, mode], { stdout: "pipe", stderr: "inherit" });
  const lines: string[] = [];
  (async () => { for await (const l of p.stdout.pipeThrough(new TextDecoderStream())) for (const x of l.split("\n").filter(Boolean)) { lines.push(x); log("daemon:", x); } })();
  return { p, lines };
};

const d1 = daemon("never");
await sleep(300);
const permLog = `${DIR}samples/restart.perm.ndjson`;
const mcp = JSON.stringify({ mcpServers: { perm: { type: "stdio", command: BUN, args: [DIR + "perm-mcp.ts"],
  env: { PERM_MODE: "socket", PERM_LOG: permLog, PERM_SOCK: SOCK } } } });
const c = start(["--tools", "Bash", "--mcp-config", mcp, "--permission-prompt-tool", "mcp__perm__approve"], `${DIR}samples/restart.ndjson`);
c.send("Run exactly `echo survived > restart.txt` with the Bash tool, then say done.");

const until = async (f: () => boolean, max = 120000) => { const end = Date.now() + max; while (!f() && Date.now() < end) await sleep(50); return f(); };
await until(() => d1.lines.some((l) => l.includes('"request"')));
log("daemon #1 has the request; SIGKILL it");
d1.p.kill("SIGKILL");
await sleep(DOWN * 1000);
log(`after ${DOWN}s down, starting daemon #2; claude exited=${c.exited}, results so far=${c.results().length}`);
const d2 = daemon("allow");
const r = await c.waitFor((e) => e.type === "result", 120000);
log("result", r?.subtype, JSON.stringify(r?.result), "cost", c.cost);
c.proc.stdin.end();
await c.proc.exited;
d2.p.kill();
