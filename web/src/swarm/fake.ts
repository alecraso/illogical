// A synthetic fleet for the swarm (M26): the prototype's world (projects,
// machines, kinds and their commands) as fleet panes, with output that comes
// and goes, so the frame-rate check and screenshots can draw a few hundred
// panes without running them. `__illogical.swarmFake(n)` in a page; real
// panes come from e2e/fake-fleet.ts.

import type { Fleet, FleetPane } from "../fleet";
import type { PaneInfo, WorkKind } from "../proto";

const rnd = (a: number, b: number) => a + Math.random() * (b - a);
const pick = <T,>(a: T[]): T => a[Math.floor(Math.random() * a.length)];
function wpick(o: Record<string, number>): string {
  let s = 0;
  for (const k in o) s += o[k];
  let r = Math.random() * s;
  for (const k in o) if ((r -= o[k]) < 0) return k;
  return Object.keys(o)[0];
}

const PROJECTS = { illogical: 9, hal0: 6, "skipto.tv": 4, ravix: 5, "home-cloud": 4, dotfiles: 2, "": 8 };
const MACHINES = { geek: 8, "jake-mini": 5, "build-01": 3, "build-02": 3, "build-03": 3, "build-04": 2, "sandbox-a": 2, "hal0-box": 3 };
const KINDW = { shell: 5, build: 3, test: 3, agent: 3, server: 2, logs: 2, editor: 2 };
const CMDS: Record<WorkKind, string[]> = {
  shell: [""],
  build: ["cargo build --release", "cargo clippy --all-targets", "npm run build"],
  test: ["cargo test", "npx playwright test", "pytest -x"],
  agent: ["claude", "claude --continue", "codex"],
  server: ["npm run dev", "uvicorn app:main --reload"],
  logs: ["journalctl -fu illogicald", "tail -f /var/log/caddy.log"],
  editor: ["nvim src/main.rs", "nvim README.md"],
};

/** `n` made-up panes, refreshed every second, alongside the real ones. */
export function fakeSwarm(fleet: Fleet, n: number): () => void {
  const panes: FleetPane[] = [];
  for (let i = 0; i < n; i++) {
    const project = wpick(PROJECTS);
    const kind = wpick(KINDW) as WorkKind;
    const host = project === "hal0" && Math.random() < 0.5 ? "hal0-box" : wpick(MACHINES);
    const cmd = pick(CMDS[kind]);
    const info = {
      id: i + 1,
      cwd: project ? `/home/fake/src/${project}` : pick(["/home/fake/scratch", "/tmp/x", "/home/fake/Downloads"]),
      command: cmd || null,
      running: true,
      current: null,
      last: null,
      attention: "idle",
      type: "terminal",
      host: null,
      kind,
      project: project ? { root: `/home/fake/src/${project}`, name: project } : null,
      activity: { bps: Math.random() < 0.45 ? Math.round(rnd(50, 5000)) : 0, last_ms: Date.now() },
    } as unknown as PaneInfo;
    // Whose (M30): build machines are the team's, the sandbox a teammate's.
    const person = host.startsWith("build-")
      ? { id: "team:infra", name: "infra", kind: "team" as const }
      : host === "sandbox-a"
        ? { id: "account:sam", name: "sam", kind: "person" as const }
        : { id: "me", name: "me", kind: "me" as const };
    panes.push({
      key: `fake-${host}:${i + 1}`,
      host,
      id: i + 1,
      info,
      session: { id: 1, name: "main" },
      stale: false,
      person,
      driver: null,
      watchers: [],
    });
  }
  fleet.inject(panes);
  const t = setInterval(() => {
    for (const p of panes) {
      const a = p.info.activity!;
      if (Math.random() < 0.05) a.bps = a.bps ? 0 : Math.round(rnd(50, 5000));
      if (a.bps) a.last_ms = Date.now();
    }
    fleet.inject(panes);
  }, 1000);
  return () => {
    clearInterval(t);
    fleet.inject([]);
  };
}
