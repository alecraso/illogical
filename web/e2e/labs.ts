import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";

/** Give a daemon's state dir the `labs` file, which turns on what a stranger
 * doesn't get (chat, huddles, Fountain, studio, VMs, guest ssh and the
 * swarm's extra views): the suite runs with it on, as every feature's spec
 * expects. `labs-off.spec.ts` starts a daemon without it. Returns the dir, to
 * wrap the `--state-dir` argument. */
export function labs(dir: string): string {
  mkdirSync(dir, { recursive: true });
  writeFileSync(join(dir, "labs"), "");
  return dir;
}
