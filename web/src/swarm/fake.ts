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

const PROJECTS = { api: 9, web: 6, mobile: 4, infra: 5, docs: 4, dotfiles: 2, "": 8 };
const MACHINES = { workstation: 8, laptop: 5, "build-01": 3, "build-02": 3, "build-03": 3, "build-04": 2, "sandbox-a": 2, "team-box": 3 };
const KINDW = { shell: 5, build: 3, test: 3, agent: 3, server: 2, logs: 2, editor: 2 };
/** How long a command of each kind runs (s), for the city's heights (M41). */
const DUR: Partial<Record<WorkKind, [number, number]>> = { shell: [1, 25], build: [25, 260], test: [15, 160], agent: [60, 1500] };

/** A command: running (no end) or finished `ago` seconds ago. */
function cmdInfo(text: string, kind: WorkKind, running: boolean) {
  const [a, b] = DUR[kind] ?? [5, 60];
  const dur = rnd(a, b) * 1000;
  const now = Date.now();
  const started = running ? now - rnd(0.05, 0.9) * dur : now - dur - rnd(30, 900) * 1000;
  return { text, cwd: null, exit: running ? null : Math.random() < 0.08 ? 1 : 0, started_ms: started, ended_ms: running ? null : started + dur, start: 0, end: null };
}
const CMDS: Record<WorkKind, string[]> = {
  shell: [""],
  build: ["cargo build --release", "cargo clippy --all-targets", "npm run build"],
  test: ["cargo test", "npx playwright test", "pytest -x"],
  agent: ["claude", "claude --continue", "codex"],
  server: ["npm run dev", "uvicorn app:main --reload"],
  logs: ["journalctl -fu illogicald", "tail -f /var/log/caddy.log"],
  editor: ["nvim src/main.rs", "nvim README.md"],
  app: [""],
  pr: [""],
  issue: [""],
  fountain: [""],
};

/** `n` made-up panes, refreshed every second, alongside the real ones. */
export function fakeSwarm(fleet: Fleet, n: number): () => void {
  const panes: FleetPane[] = [];
  for (let i = 0; i < n; i++) {
    const project = wpick(PROJECTS);
    const kind = wpick(KINDW) as WorkKind;
    // The first is always on the teammate's machine, so every grouping by
    // person has all three owners (a random 40 sometimes had none of sam's).
    const host = i === 0 ? "sandbox-a" : project === "infra" && Math.random() < 0.5 ? "team-box" : wpick(MACHINES);
    const cmd = pick(CMDS[kind]);
    const info = {
      id: i + 1,
      cwd: project ? `/home/fake/src/${project}` : pick(["/home/fake/scratch", "/tmp/x", "/home/fake/Downloads"]),
      command: cmd || null,
      running: true,
      // M41: what the city stands on: a command running, or the last one.
      current: DUR[kind] && Math.random() < 0.45 ? cmdInfo(cmd || "ls", kind, true) : null,
      last: DUR[kind] && Math.random() < 0.8 ? cmdInfo(cmd || "ls", kind, false) : null,
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
      // M61: a few with people talking about them.
      unread: Math.random() < 0.06 ? Math.ceil(rnd(1, 4)) : 0,
      mention: Math.random() < 0.02,
    });
  }
  fleet.inject(panes);
  const t = setInterval(() => {
    for (const p of panes) {
      const a = p.info.activity!;
      const kind = p.info.kind as WorkKind;
      const cur = p.info.current;
      // Commands finish and start, so heights grow and fall.
      if (cur && DUR[kind] && Math.random() < 0.01) {
        p.info.last = { ...cur, ended_ms: Date.now(), exit: Math.random() < 0.08 ? 1 : 0 };
        p.info.current = null;
        a.bps = 0;
      } else if (!cur && DUR[kind] && Math.random() < 0.01) {
        p.info.current = cmdInfo(p.info.command || "ls", kind, true);
        p.info.current.started_ms = Date.now();
      }
      if (Math.random() < 0.05) a.bps = a.bps || !p.info.current && DUR[kind] ? 0 : Math.round(rnd(50, 5000));
      if (a.bps) a.last_ms = Date.now();
    }
    fleet.inject(panes);
  }, 1000);
  return () => {
    clearInterval(t);
    fleet.inject([]);
  };
}
