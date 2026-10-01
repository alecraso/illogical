// A stdio MCP server with one tool, `approve`, for --permission-prompt-tool.
// Hand-rolled JSON-RPC (newline-delimited), no deps.
//
// PERM_MODE:
//   auto        allow Bash `echo|ls|pwd ...`, deny everything else (with a reason)
//   hold:<sec>  wait <sec> seconds, then answer like auto
//   socket      relay to a "daemon" on the Unix socket PERM_SOCK; reconnect
//               and resend pending requests if the daemon goes away
// PERM_LOG: file that gets every request and answer as NDJSON.
import { appendFileSync } from "node:fs";

const MODE = process.env.PERM_MODE ?? "auto";
const LOG = process.env.PERM_LOG ?? "/dev/stderr";
const t0 = Date.now();
const note = (o: object) => appendFileSync(LOG, JSON.stringify({ t: Date.now() - t0, pid: process.pid, ...o }) + "\n");
const out = (o: object) => process.stdout.write(JSON.stringify(o) + "\n");
const sleep = (n: number) => new Promise((r) => setTimeout(r, n));

function policy(args: any) {
  const cmd = String(args?.input?.command ?? "");
  // echo/ls/pwd only; the one redirect allowed is `> name.txt` inside the scratch cwd.
  const bare = cmd.replace(/\s*>\s*[\w.-]+\.txt\s*$/, "");
  if (args?.tool_name === "Bash" && /^(echo|ls|pwd)(\s|$)/.test(bare) && !/[;&|`$<>]/.test(bare))
    return { behavior: "allow", updatedInput: args.input };
  return { behavior: "deny", message: `spike policy: only echo/ls/pwd are allowed, not ${args?.tool_name} ${cmd}` };
}

// --- socket relay -------------------------------------------------------
const pending = new Map<string, { args: any; resolve: (d: any) => void }>();
let sock: any = null;
let rid = 0;
async function connectLoop() {
  const path = process.env.PERM_SOCK!;
  for (;;) {
    try {
      let buf = "";
      await new Promise<void>((resolveClosed, reject) => {
        Bun.connect({
          unix: path,
          socket: {
            open(s) {
              sock = s; note({ ev: "daemon-connected", resend: [...pending.keys()] });
              for (const [id, p] of pending) s.write(JSON.stringify({ id, args: p.args }) + "\n");
            },
            data(_s, d) {
              buf += new TextDecoder().decode(d);
              let nl;
              while ((nl = buf.indexOf("\n")) >= 0) {
                const m = JSON.parse(buf.slice(0, nl)); buf = buf.slice(nl + 1);
                const p = pending.get(m.id);
                if (p) { pending.delete(m.id); p.resolve(m.decision); }
              }
            },
            close() { sock = null; note({ ev: "daemon-gone" }); resolveClosed(); },
            error(_s, e) { reject(e); },
          },
        }).catch(reject);
      });
    } catch (e) { /* not listening yet */ }
    sock = null;
    await sleep(500);
  }
}
if (MODE === "socket") connectLoop();

function relay(args: any): Promise<any> {
  const id = `r${++rid}`;
  return new Promise((resolve) => {
    pending.set(id, { args, resolve });
    sock?.write(JSON.stringify({ id, args }) + "\n");
  });
}

async function decide(args: any) {
  if (MODE.startsWith("hold:")) { await sleep(Number(MODE.slice(5)) * 1000); return policy(args); }
  if (MODE === "socket") return relay(args);
  return policy(args);
}

// --- JSON-RPC over stdio ------------------------------------------------
async function handle(msg: any) {
  const { id, method, params } = msg;
  if (method === "initialize")
    return out({ jsonrpc: "2.0", id, result: {
      protocolVersion: params?.protocolVersion ?? "2025-06-18",
      capabilities: { tools: {} }, serverInfo: { name: "perm", version: "0.1" } } });
  if (method === "tools/list")
    return out({ jsonrpc: "2.0", id, result: { tools: [{
      name: "approve",
      description: "Decide whether Claude may use a tool.",
      inputSchema: { type: "object", properties: {
        tool_name: { type: "string" }, input: { type: "object" }, tool_use_id: { type: "string" } },
        required: ["tool_name", "input"] } }] } });
  if (method === "tools/call") {
    note({ ev: "request", params });
    const decision = await decide(params.arguments);
    note({ ev: "answer", decision });
    return out({ jsonrpc: "2.0", id, result: { content: [{ type: "text", text: JSON.stringify(decision) }] } });
  }
  if (id !== undefined) out({ jsonrpc: "2.0", id, error: { code: -32601, message: `no ${method}` } });
  else note({ ev: "notification", method });
}

note({ ev: "start", mode: MODE });
let buf = "";
process.stdin.on("data", (d) => {
  buf += d.toString();
  let nl;
  while ((nl = buf.indexOf("\n")) >= 0) {
    const line = buf.slice(0, nl); buf = buf.slice(nl + 1);
    if (line.trim()) handle(JSON.parse(line));
  }
});
process.stdin.on("end", () => { note({ ev: "stdin-eof" }); process.exit(0); });
process.on("SIGTERM", () => { note({ ev: "sigterm" }); process.exit(0); });
