// M45b: each machine's Fountain runner, for its line in the host menu, the
// phone's host list and the swarm. A machine with the `fountain-runner`
// unit says so in `GET /api/host` (`fountain_runner`: name, online,
// version, sandboxes, and what wants you), from what its daemon last read
// (never Fountain on the request). Asked of each connected host once a
// minute over its summary connection.

import type { Fleet } from "./fleet";
import type { FountainRunnerInfo, HostInfo } from "./proto";

const EVERY = 60_000;
const known = new Map<string, { info: FountainRunnerInfo | null; at: number }>();
const subs = new Set<() => void>();
let timer: number | null = null;

/** A host's runner, if it has one (and it has answered). */
export function runnerOf(host: string): FountainRunnerInfo | null {
  return known.get(host)?.info ?? null;
}

/** Every host that is a runner. */
export function runners(): [string, FountainRunnerInfo][] {
  return [...known].flatMap(([h, v]) => (v.info ? [[h, v.info] as [string, FountainRunnerInfo]] : []));
}

/** One line: "Fountain runner geek online · v0.21.0 · 2 sandboxes". */
export function runnerLabel(i: FountainRunnerInfo): string {
  const state = i.online === true ? "online" : i.online === false ? "offline" : "…";
  const parts = [`Fountain runner ${i.name} ${state}`];
  if (i.version) parts.push(i.version);
  if (i.sandboxes != null) parts.push(`${i.sandboxes} sandbox${i.sandboxes === 1 ? "" : "es"}`);
  return parts.join(" · ");
}

export function subscribeRunners(fn: () => void): () => void {
  subs.add(fn);
  return () => subs.delete(fn);
}

async function ask(fleet: Fleet) {
  const now = Date.now();
  let changed = false;
  await Promise.all(
    fleet.list
      .filter((h) => h.state === "connected" && now - (known.get(h.name)?.at ?? 0) >= EVERY - 1000)
      .map(async (h) => {
        // Asked once a minute, answered or not.
        const was = known.get(h.name)?.info ?? null;
        known.set(h.name, { info: was, at: now });
        try {
          const res = await fleet.request(h.name, "GET", "/api/host");
          if (!res.ok) return;
          const v = await res.json<Partial<HostInfo>>();
          const info = v.fountain_runner ?? null;
          known.set(h.name, { info, at: now });
          if (JSON.stringify(was) !== JSON.stringify(info)) changed = true;
        } catch {
          // not connected after all: asked again next time
        }
      }),
  );
  if (changed) for (const fn of subs) fn();
}

/** Start asking (once per page). */
export function watchRunners(fleet: Fleet) {
  if (timer !== null) return;
  void ask(fleet);
  fleet.subscribe(() => void ask(fleet));
  timer = window.setInterval(() => void ask(fleet), EVERY);
}
