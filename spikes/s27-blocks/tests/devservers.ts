// Real dev servers for blocks: Vite, Next and code-server, each in a copy
// of its fixture under .run/ (so a test's edits never touch the tree).

import { spawn, type ChildProcess } from "node:child_process";
import { cpSync, existsSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { freePort } from "./servers.ts";

export interface Server {
  port: number;
  dir: string;
  proc: ChildProcess;
  log: string[];
  stop(): void;
}

async function answers(port: number, path: string, timeoutMs: number, proc: ChildProcess, log: string[]) {
  const end = Date.now() + timeoutMs;
  while (Date.now() < end) {
    if (proc.exitCode !== null) throw new Error(`exited ${proc.exitCode}:\n${log.join("")}`);
    try {
      const r = await fetch(`http://127.0.0.1:${port}${path}`);
      if (r.status < 500) return;
    } catch {}
    await new Promise((r) => setTimeout(r, 250));
  }
  throw new Error(`nothing answered on ${port} in ${timeoutMs} ms:\n${log.join("")}`);
}

function start(cmd: string, args: string[], dir: string, port: number, env: Record<string, string> = {}): Server {
  const log: string[] = [];
  const proc = spawn(cmd, args, { cwd: dir, env: { ...process.env, ...env }, stdio: ["ignore", "pipe", "pipe"], detached: true });
  proc.stdout!.on("data", (d) => log.push(String(d)));
  proc.stderr!.on("data", (d) => log.push(String(d)));
  return {
    port,
    dir,
    proc,
    log,
    stop() {
      try {
        process.kill(-proc.pid!, "SIGTERM");
      } catch {}
    },
  };
}

function copy(fixture: string): string {
  const dir = resolve(`.run/${fixture}-${process.pid}-${Date.now()}`);
  rmSync(dir, { recursive: true, force: true });
  cpSync(`fixtures/${fixture}`, dir, { recursive: true });
  return dir;
}

export async function vite(): Promise<Server> {
  const dir = copy("vite-app");
  const port = await freePort();
  const bin = resolve("node_modules/vite/bin/vite.js");
  const s = start(process.execPath, [bin, "--host", "127.0.0.1", "--port", String(port), "--strictPort"], dir, port);
  await answers(port, "/", 30_000, s.proc, s.log);
  return s;
}

export async function next(): Promise<Server> {
  const dir = copy("next-app");
  writeFileSync(`${dir}/next.config.mjs`, `export default { agentRules: false, turbopack: { root: ${JSON.stringify(resolve("."))} } };\n`);
  const port = await freePort();
  const bin = resolve("node_modules/next/dist/bin/next");
  const s = start(process.execPath, [bin, "dev", "-H", "127.0.0.1", "-p", String(port)], dir, port, { NEXT_TELEMETRY_DISABLED: "1" });
  await answers(port, "/", 120_000, s.proc, s.log);
  return s;
}

/** The code-server release editor blocks run (crates/daemon/src/editor/
 * server.rs), if it's been fetched to .run/cache (see README). */
const PLATFORM = `${process.platform === "darwin" ? "macos" : "linux"}-${process.arch === "arm64" ? "arm64" : "amd64"}`;
export const CODE_SERVER = resolve(`.run/cache/code-server-4.140.0-${PLATFORM}/bin/code-server`);

export async function codeServer(): Promise<Server> {
  const dir = resolve(`.run/cs-${process.pid}-${Date.now()}`);
  mkdirSync(`${dir}/project`, { recursive: true });
  writeFileSync(`${dir}/project/hello.txt`, "hello from the block\n");
  writeFileSync(`${dir}/project/notes.md`, "# Notes heading\n\nsome text\n");
  const port = await freePort();
  const s = start(
    CODE_SERVER,
    [
      "--auth", "none",
      "--bind-addr", `127.0.0.1:${port}`,
      "--disable-telemetry",
      "--disable-update-check",
      "--disable-workspace-trust",
      "--user-data-dir", `${dir}/data`,
      "--extensions-dir", `${dir}/ext`,
      `${dir}/project`,
    ],
    dir,
    port,
    { XDG_CONFIG_HOME: `${dir}/config`, XDG_DATA_HOME: `${dir}/share` },
  );
  await answers(port, "/healthz", 60_000, s.proc, s.log);
  return s;
}

export const haveCodeServer = () => existsSync(CODE_SERVER);
