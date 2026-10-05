// Ports from the OS (#67): daemons and control get `--listen 127.0.0.1:0`
// and say which port they took (in `listen` in the state directory, or
// beside control's database); fake servers listen on 0. So a run reserves
// nothing but E2E_PORT, and two runs side by side don't collide.

import type { ChildProcess } from "node:child_process";
import { readFileSync } from "node:fs";
import type { AddressInfo, Server } from "node:net";
import { dirname, join } from "node:path";

/** What to pass to `--listen` (and `--block-listen`): any free port. */
export const ANY = "127.0.0.1:0";

/** The port recorded in `file` (`HOST:PORT`), waiting for it. */
export async function recorded(file: string, proc?: ChildProcess, timeout = 20_000): Promise<number> {
  const deadline = Date.now() + timeout;
  for (;;) {
    try {
      const port = Number(readFileSync(file, "utf8").trim().split(":").pop());
      if (port > 0) return port;
    } catch {
      // not yet
    }
    if (proc && proc.exitCode !== null) throw new Error(`exited (${proc.exitCode}) before writing ${file}`);
    if (Date.now() > deadline) throw new Error(`no port in ${file}`);
    await new Promise((r) => setTimeout(r, 25));
  }
}

/** A daemon's port, from its state directory. */
export const daemonPort = (state: string, proc?: ChildProcess) => recorded(join(state, "listen"), proc);
/** Its block sites' port (`--block-listen 127.0.0.1:0`). */
export const blockPort = (state: string, proc?: ChildProcess) => recorded(join(state, "block-listen"), proc);
/** illogical-control's port, from beside its database. */
export const controlPort = (db: string, proc?: ChildProcess) => recorded(join(dirname(db), "listen"), proc);

/** Listen on a free loopback port; resolves to it. */
export function listen(server: Server, host = "127.0.0.1"): Promise<number> {
  return new Promise((ok, fail) => {
    server.once("error", fail);
    server.listen(0, host, () => ok((server.address() as AddressInfo).port));
  });
}
