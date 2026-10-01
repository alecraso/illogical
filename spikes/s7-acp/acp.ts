// Minimal hand-rolled ACP client: JSON-RPC 2.0 over newline-delimited stdio.
// Logs every frame (both directions) with a timestamp to an NDJSON file.
// Implements the client side of session/request_permission, fs/* and terminal/*.
import { spawn, type ChildProcess } from "node:child_process";
import { appendFileSync, readFileSync, writeFileSync, mkdirSync, createReadStream, createWriteStream, openSync } from "node:fs";
import { dirname, resolve } from "node:path";

export const HERE = dirname(new URL(import.meta.url).pathname);
export const WORK = resolve(HERE, "work");
export const SCRATCH = process.env.CWD ? resolve(process.env.CWD) : resolve(WORK, "scratch");

export type PermPolicy =
  | { mode: "allow"; kind?: "allow_once" | "allow_always"; delayMs?: number }
  | { mode: "reject"; delayMs?: number }
  | { mode: "hold" } // never answer
  | { mode: "cancelled"; delayMs?: number };

export interface Opts {
  cmd: string;
  args?: string[];
  cwd?: string;
  env?: Record<string, string>;
  log: string; // ndjson path
  perm?: PermPolicy;
  slowTerminalMs?: number; // delay terminal/output + wait_for_exit answers
  killTerminalAfterMs?: number; // client kills the command on its own
  quiet?: boolean;
  detached?: boolean;
  fifo?: { in: string; out: string }; // attach to an already-running agent through held FIFOs instead of spawning
}

type Pending = { resolve: (v: any) => void; reject: (e: any) => void; method: string };

export const t0 = Date.now();
export const ts = () => ((Date.now() - t0) / 1000).toFixed(3);

interface Term { proc: ChildProcess; out: string; exit: null | { exitCode: number | null; signal: string | null }; waiters: ((v: any) => void)[]; limit?: number; truncated: boolean }

export class Acp {
  proc: ChildProcess;
  nextId = 1;
  pending = new Map<number | string, Pending>();
  buf = "";
  updates: any[] = [];
  incoming: any[] = []; // agent->client requests
  terms = new Map<string, Term>();
  termSeq = 0;
  onUpdate?: (u: any) => void;
  onRequest?: (m: any) => void;
  exited: Promise<{ code: number | null; signal: string | null }>;

  constructor(public o: Opts) {
    mkdirSync(dirname(o.log), { recursive: true });
    if (o.fifo) {
      const w = createWriteStream("", { fd: openSync(o.fifo.in, "w") });
      const r = createReadStream("", { fd: openSync(o.fifo.out, "r"), encoding: "utf8" });
      r.on("data", (d) => this.feed(String(d)));
      this.proc = { stdin: w, pid: process.pid, kill: () => { w.destroy(); r.destroy(); } } as any;
      this.exited = new Promise(() => {});
      this.log("attach", o.fifo);
      return;
    }
    this.proc = spawn(o.cmd, o.args ?? [], {
      cwd: o.cwd ?? SCRATCH,
      env: { ...process.env, ...(o.env ?? {}) },
      stdio: ["pipe", "pipe", "pipe"],
      detached: o.detached ?? false,
    });
    this.log("spawn", { pid: this.proc.pid, cmd: o.cmd, args: o.args });
    this.proc.stdout!.setEncoding("utf8");
    this.proc.stdout!.on("data", (d: string) => this.feed(d));
    this.proc.stderr!.setEncoding("utf8");
    this.proc.stderr!.on("data", (d: string) => this.log("stderr", { text: d }));
    this.exited = new Promise((r) => this.proc.on("exit", (code, signal) => { this.log("exit", { code, signal }); r({ code, signal }); }));
  }

  log(dir: string, msg: any) {
    appendFileSync(this.o.log, JSON.stringify({ t: ts(), dir, msg }) + "\n");
    if (!this.o.quiet) {
      const s = JSON.stringify(msg);
      console.log(`${ts()} ${dir} ${s.length > 400 ? s.slice(0, 400) + "…" : s}`);
    }
  }

  send(m: any) {
    this.log("->", m);
    this.proc.stdin!.write(JSON.stringify(m) + "\n");
  }

  request(method: string, params: any): Promise<any> {
    const id = this.nextId++;
    this.send({ jsonrpc: "2.0", id, method, params });
    return new Promise((resolve, reject) => this.pending.set(id, { resolve, reject, method }));
  }

  notify(method: string, params: any) {
    this.send({ jsonrpc: "2.0", method, params });
  }

  feed(d: string) {
    this.buf += d;
    let i;
    while ((i = this.buf.indexOf("\n")) >= 0) {
      const line = this.buf.slice(0, i);
      this.buf = this.buf.slice(i + 1);
      if (!line.trim()) continue;
      let m: any;
      try { m = JSON.parse(line); } catch { this.log("<-bad", { line }); continue; }
      this.log("<-", m);
      this.dispatch(m);
    }
  }

  dispatch(m: any) {
    if (m.id !== undefined && (m.result !== undefined || m.error !== undefined) && !m.method) {
      const p = this.pending.get(m.id);
      if (p) { this.pending.delete(m.id); m.error ? p.reject(m.error) : p.resolve(m.result); }
      return;
    }
    if (m.method === "session/update") {
      this.updates.push(m.params);
      this.onUpdate?.(m.params);
      return;
    }
    if (m.id !== undefined && m.method) {
      this.incoming.push(m);
      this.onRequest?.(m);
      this.handle(m).then(
        (result) => this.send({ jsonrpc: "2.0", id: m.id, result }),
        (e) => this.send({ jsonrpc: "2.0", id: m.id, error: { code: -32603, message: String(e?.message ?? e) } }),
      );
    }
  }

  answerPermission(id: any, outcome: any) {
    this.send({ jsonrpc: "2.0", id, result: { outcome } });
  }

  async handle(m: any): Promise<any> {
    const p = m.params ?? {};
    switch (m.method) {
      case "session/request_permission": {
        const pol = this.o.perm ?? { mode: "allow" };
        if (pol.mode === "hold") return new Promise(() => {});
        if ("delayMs" in pol && pol.delayMs) await sleep(pol.delayMs);
        if (pol.mode === "cancelled") return { outcome: { outcome: "cancelled" } };
        const want = pol.mode === "allow" ? (pol.kind ?? "allow_once") : "reject_once";
        const opt = p.options.find((o: any) => o.kind === want) ?? p.options.find((o: any) => o.kind.startsWith(pol.mode === "allow" ? "allow" : "reject"));
        return { outcome: { outcome: "selected", optionId: opt.optionId } };
      }
      case "fs/read_text_file": {
        let txt = readFileSync(p.path, "utf8");
        if (p.line || p.limit) {
          const lines = txt.split("\n");
          const s = (p.line ?? 1) - 1;
          txt = lines.slice(s, p.limit ? s + p.limit : undefined).join("\n");
        }
        return { content: txt };
      }
      case "fs/write_text_file": {
        mkdirSync(dirname(p.path), { recursive: true });
        writeFileSync(p.path, p.content);
        return {};
      }
      case "terminal/create": {
        const id = `term-${++this.termSeq}`;
        const env = Object.fromEntries((p.env ?? []).map((e: any) => [e.name, e.value]));
        const proc = spawn(p.command, p.args ?? [], { cwd: p.cwd ?? SCRATCH, env: { ...process.env, ...env }, stdio: ["ignore", "pipe", "pipe"], shell: !(p.args?.length) });
        const term: Term = { proc, out: "", exit: null, waiters: [], limit: p.outputByteLimit, truncated: false };
        const add = (d: Buffer) => {
          term.out += d.toString();
          if (term.limit && Buffer.byteLength(term.out) > term.limit) { term.out = term.out.slice(-term.limit); term.truncated = true; }
        };
        proc.stdout!.on("data", add); proc.stderr!.on("data", add);
        proc.on("exit", (code, signal) => { term.exit = { exitCode: code, signal }; term.waiters.forEach((w) => w(term.exit)); });
        this.terms.set(id, term);
        if (this.o.killTerminalAfterMs) setTimeout(() => proc.kill("SIGKILL"), this.o.killTerminalAfterMs);
        return { terminalId: id };
      }
      case "terminal/output": {
        if (this.o.slowTerminalMs) await sleep(this.o.slowTerminalMs);
        const t = this.terms.get(p.terminalId)!;
        return { output: t.out, truncated: t.truncated, ...(t.exit ? { exitStatus: t.exit } : {}) };
      }
      case "terminal/wait_for_exit": {
        if (this.o.slowTerminalMs) await sleep(this.o.slowTerminalMs);
        const t = this.terms.get(p.terminalId)!;
        return t.exit ?? new Promise((r) => t.waiters.push(r));
      }
      case "terminal/kill": {
        this.terms.get(p.terminalId)?.proc.kill("SIGTERM");
        return {};
      }
      case "terminal/release": {
        const t = this.terms.get(p.terminalId);
        if (t && !t.exit) t.proc.kill("SIGKILL");
        this.terms.delete(p.terminalId);
        return {};
      }
      default:
        throw new Error(`unsupported client method ${m.method}`);
    }
  }

  close() { this.proc.stdin!.end(); }
}

export const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

export const CLIENT_CAPS = (extra: any = {}) => ({
  fs: { readTextFile: true, writeTextFile: true },
  terminal: true,
  ...extra,
});

export async function init(a: Acp, caps = CLIENT_CAPS()) {
  return a.request("initialize", {
    protocolVersion: 1,
    clientCapabilities: caps,
    clientInfo: { name: "illogical-s7", version: "0.0.1" },
  });
}

// Claude adapter session/new: haiku, no user/project settings (no hooks), scratch cwd.
export const CLAUDE_META = { claudeCode: { options: { model: "haiku", settingSources: process.env.SOURCES ? process.env.SOURCES.split(",").filter(Boolean) : [] } } };

export function kinds(updates: any[]) {
  const c: Record<string, number> = {};
  for (const u of updates) c[u.update.sessionUpdate] = (c[u.update.sessionUpdate] ?? 0) + 1;
  return c;
}

export function text(updates: any[], kind = "agent_message_chunk") {
  return updates.filter((u) => u.update.sessionUpdate === kind).map((u) => u.update.content?.text ?? "").join("");
}

export const claude = (log: string, extra: Partial<Opts> = {}) =>
  new Acp({ cmd: resolve(HERE, "claude-acp.sh"), log: resolve(WORK, log), ...extra });

// session/new + switch to haiku (the _meta model option is overridden by the adapter's model config option).
export async function newClaudeSession(a: Acp, extraMeta: any = {}) {
  const s = await a.request("session/new", { cwd: SCRATCH, mcpServers: [], _meta: { ...CLAUDE_META, ...extraMeta } });
  const r = await a.request("session/set_config_option", { sessionId: s.sessionId, configId: "model", value: "haiku" });
  const cur = r.configOptions?.find((o: any) => o.id === "model")?.currentValue;
  if (cur !== "haiku") throw new Error("model not haiku: " + cur);
  return s;
}
