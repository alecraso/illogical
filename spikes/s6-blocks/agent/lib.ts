// Shared helpers: spawn a nested claude in stream-json mode, log its events.
import { appendFileSync, writeFileSync } from "node:fs";

export const DIR = new URL(".", import.meta.url).pathname;
export const CWD = DIR + "../work/claude";
const t0 = performance.now();
export const ms = () => Math.round(performance.now() - t0);
export const sleep = (n: number) => new Promise((r) => setTimeout(r, n));
export const log = (...a: unknown[]) => console.log(`[${ms()}ms]`, ...a);

export const BASE = [
  "-p", "--verbose", "--input-format", "stream-json", "--output-format", "stream-json",
  "--model", "haiku", "--strict-mcp-config", "--setting-sources", "",
];

export type Claude = ReturnType<typeof start>;

export function start(extra: string[], sample?: string) {
  const proc = Bun.spawn([DIR + "claude.sh", ...BASE, ...extra], {
    cwd: CWD, stdin: "pipe", stdout: "pipe", stderr: "pipe",
  });
  if (sample) writeFileSync(sample, "");
  const c = {
    proc, events: [] as any[], cost: 0, exited: null as number | null,
    send(text: string) {
      const msg = { type: "user", message: { role: "user", content: text } };
      proc.stdin.write(JSON.stringify(msg) + "\n"); proc.stdin.flush();
      log("> sent", JSON.stringify(text));
    },
    async waitFor(f: (e: any) => boolean, max = 120000) {
      const end = Date.now() + max;
      let i = 0;
      while (Date.now() < end) {
        for (; i < c.events.length; i++) if (f(c.events[i])) return c.events[i];
        if (c.exited !== null) return null;
        await sleep(50);
      }
      return null;
    },
    results: () => c.events.filter((e) => e.type === "result"),
  };
  (async () => {
    const dec = new TextDecoder(); let buf = "";
    for await (const chunk of proc.stdout) {
      buf += dec.decode(chunk);
      let nl;
      while ((nl = buf.indexOf("\n")) >= 0) {
        const line = buf.slice(0, nl); buf = buf.slice(nl + 1);
        if (!line.trim()) continue;
        if (sample) appendFileSync(sample, line + "\n");
        let e: any; try { e = JSON.parse(line); } catch { log("non-json", line); continue; }
        e._t = ms(); c.events.push(e);
        if (e.type === "result") { c.cost = e.total_cost_usd ?? c.cost; } // cumulative per process
        log("<", summary(e));
      }
    }
  })();
  (async () => { const t = await new Response(proc.stderr).text(); if (t.trim()) log("stderr:", t.trim().slice(0, 500)); })();
  proc.exited.then((code) => { c.exited = code; log("exited", code, "signal", proc.signalCode); });
  return c;
}

export function summary(e: any): string {
  const s = [e.type, e.subtype].filter(Boolean).join("/");
  if (e.type === "system" && e.subtype === "init")
    return `${s} session=${e.session_id} model=${e.model} perm=${e.permissionMode} mcp=${JSON.stringify(e.mcp_servers)} tools=${e.tools?.length}`;
  if (e.type === "assistant")
    return `${s} ` + JSON.stringify(e.message?.content?.map((b: any) => b.type === "text" ? { text: b.text.slice(0, 120) } : b.type === "tool_use" ? { tool_use: b.name, input: b.input, id: b.id } : { [b.type]: 1 }));
  if (e.type === "user")
    return `${s} ` + JSON.stringify(e.message?.content?.map?.((b: any) => b.type === "tool_result" ? { tool_result: String(typeof b.content === "string" ? b.content : JSON.stringify(b.content)).slice(0, 160), is_error: b.is_error } : b) ?? e.message?.content);
  if (e.type === "result")
    return `${s} is_error=${e.is_error} turns=${e.num_turns} dur=${e.duration_ms} cost=${e.total_cost_usd} denials=${JSON.stringify(e.permission_denials)} result=${JSON.stringify(String(e.result ?? "").slice(0, 120))}`;
  return `${s} ${JSON.stringify(e).slice(0, 200)}`;
}

export function record(file: string, line: string) { appendFileSync(file, line + "\n"); }
