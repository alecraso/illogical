// S24 q1 through claude-agent-acp: does a session take a Fountain agent's
// system prompt, skills (as a local plugin) and MCP servers from session/new?
//
//   node q1-acp.mjs <bundle dir from wear.py> [cwd]
//
// Prints the agent's answer and exits.
import { spawn } from "node:child_process";
import { readFileSync } from "node:fs";
import { join } from "node:path";

const bundle = process.argv[2];
const cwd = process.argv[3] ?? process.cwd();
const acp = process.env.ACP ?? `${process.env.HOME}/.local/share/illogical/agents/claude/node_modules/.bin/claude-agent-acp`;
const system = readFileSync(join(bundle, "system.md"), "utf8");
const mcp = JSON.parse(readFileSync(join(bundle, "mcp.json"), "utf8")).mcpServers;
// ACP's McpServer shape: http {type, name, url, headers: [{name, value}]}, stdio {name, command, args, env: [{name, value}]}.
const mcpServers = Object.entries(mcp).map(([name, s]) =>
  s.url
    ? { type: s.type ?? "http", name, url: s.url, headers: Object.entries(s.headers ?? {}).map(([k, v]) => ({ name: k, value: v })) }
    : { name, command: s.command, args: s.args ?? [], env: Object.entries(s.env ?? {}).map(([k, v]) => ({ name: k, value: v })) },
);

const child = spawn(acp, [], { stdio: ["pipe", "pipe", "inherit"] });
let id = 0, buf = "", text = "";
const waiting = new Map();
const send = (method, params) => new Promise((ok, fail) => {
  const n = ++id;
  waiting.set(n, { ok, fail });
  child.stdin.write(JSON.stringify({ jsonrpc: "2.0", id: n, method, params }) + "\n");
});
child.stdout.on("data", (d) => {
  buf += d;
  let i;
  while ((i = buf.indexOf("\n")) >= 0) {
    const line = buf.slice(0, i); buf = buf.slice(i + 1);
    if (!line.trim()) continue;
    const m = JSON.parse(line);
    if (m.id && waiting.has(m.id) && !m.method) {
      const w = waiting.get(m.id); waiting.delete(m.id);
      m.error ? w.fail(new Error(JSON.stringify(m.error))) : w.ok(m.result);
    } else if (m.method === "session/update") {
      const u = m.params.update;
      if (u.sessionUpdate === "agent_message_chunk" && u.content?.type === "text") text += u.content.text;
    } else if (m.method === "session/request_permission") {
      child.stdin.write(JSON.stringify({ jsonrpc: "2.0", id: m.id, result: { outcome: { outcome: "cancelled" } } }) + "\n");
    }
  }
});

const t0 = Date.now();
await send("initialize", { protocolVersion: 1, clientCapabilities: { fs: { readTextFile: false, writeTextFile: false }, terminal: false } });
const s = await send("session/new", {
  cwd,
  mcpServers,
  _meta: {
    systemPrompt: { append: system },
    // settingSources: [] as illogical's Claude blocks send (defs.rs); without it the
    // user's SessionStart hook (`illogical inbox`, 24 h timeout) holds the session.
    claudeCode: { options: { settingSources: [], plugins: [{ type: "local", path: join(bundle, "plugin") }] } },
  },
});
await send("session/prompt", {
  sessionId: s.sessionId,
  prompt: [{ type: "text", text: "Without calling any tools: in one line say which agent you are and what you are for. Then list the names of the skills you can use whose names contain 'review' or 'pr', and the names of the MCP servers you have tools from." }],
});
console.log(text.trim());
console.error(`(${((Date.now() - t0) / 1000).toFixed(1)} s)`);
child.kill();
process.exit(0);
