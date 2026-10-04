// Q3: --permission-prompt-tool backed by perm-mcp.ts.
// usage: bun perm.ts <mode> <tag> [prompt] [-- extra claude args]
//   mode: auto | hold:<sec> | socket   (see perm-mcp.ts)
import { start, log, DIR } from "./lib.ts";

const argv = process.argv.slice(2);
const dd = argv.indexOf("--");
const extra = dd >= 0 ? argv.slice(dd + 1) : [];
const [mode = "auto", tag = "perm", prompt = "Run `echo hello` with the Bash tool, then say done."] = dd >= 0 ? argv.slice(0, dd) : argv;

const BUN = "/home/me/.local/share/mise/installs/bun/1.4.2/bin/bun";
const permLog = `${DIR}samples/${tag}.perm.ndjson`;
const mcp = JSON.stringify({ mcpServers: { perm: { type: "stdio", command: BUN, args: [DIR + "perm-mcp.ts"],
  env: { PERM_MODE: mode, PERM_LOG: permLog, PERM_SOCK: process.env.PERM_SOCK ?? "" } } } });

const c = start(["--tools", "Bash", "--mcp-config", mcp, "--permission-prompt-tool", "mcp__perm__approve", ...extra],
  `${DIR}samples/${tag}.ndjson`);
log("claude pid", c.proc.pid, "mode", mode);
c.send(prompt);
const r = await c.waitFor((e) => e.type === "result", 15 * 60_000);
log("result", r ? r.subtype : "none", "cost", c.cost);
c.proc.stdin.end();
await c.proc.exited;
