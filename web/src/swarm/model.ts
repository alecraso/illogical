// The swarm's model (M26): what group each pane is in, what colour its kind
// is, how busy it is, and which card on the "needs you" rail its reason
// lands on. Pure functions over the fleet's merged panes (M25), so the field
// and the rail agree and the tests can check them directly.

import type { FleetPane } from "../fleet";
import type { Reason, WorkKind } from "../proto";

export type GroupBy = "project" | "machine" | "kind" | "session" | "person";
export const GROUPINGS: GroupBy[] = ["project", "machine", "kind", "session", "person"];

/** The prototype's palette, one colour per kind. */
export const KINDS: Record<WorkKind, [number, number, number]> = {
  shell: [138, 160, 190],
  build: [242, 176, 70],
  test: [110, 210, 140],
  agent: [185, 140, 255],
  server: [80, 205, 200],
  logs: [100, 125, 165],
  editor: [240, 130, 165],
  // M35: a studio box.
  app: [130, 200, 90],
  // M36: a pull request.
  pr: [235, 150, 110],
  // M37: an issue.
  issue: [215, 190, 95],
};

/** What a reason looks like on the rail. */
export const REASON_COL: Record<Reason["kind"], [number, number, number]> = {
  ask: [185, 140, 255],
  failed: [255, 84, 104],
  exited: [255, 84, 104],
  done: [99, 224, 160],
  input: [255, 192, 77],
  paused: [110, 170, 255],
  errors: [255, 84, 104],
  conflict: [255, 140, 60],
  diff: [185, 140, 255],
  gate: [255, 192, 77],
};

/** An editor that joined the swarm (M28): no tab, no PTY. Following it
 * opens a view of its cursor instead of a tab. */
export function isPresence(p: FleetPane): boolean {
  return p.info.type === "editor" && !!p.info.editor && p.session === null;
}

/** An editor whose cursor can be followed: one that joined, or an editor
 * block whose window is connected. */
export function followable(p: FleetPane): boolean {
  return !!p.info.editor && !p.stale;
}

export function kindOf(p: FleetPane): WorkKind {
  if (p.info.kind) return p.info.kind;
  if (p.info.type === "agent") return "agent";
  if (p.info.type === "app") return "app";
  if (p.info.type === "forge") return "pr";
  if (p.info.type === "browser") return "server";
  return "shell";
}

/** Where a pane with no git project groups by project: its working
 * directory's top directory under a home directory (`~/dev/x` is "dev"),
 * else its first path component ("/tmp"). S16 found most panes are outside
 * any repository, so this is most of the field, never one "none" pile. */
export function dirGroup(cwd: string | null | undefined, host: string): string {
  if (!cwd) return `~ on ${host}`;
  const home = /\/(?:home|Users)\/[^/]+(?:\/([^/]+))?/.exec(cwd) ?? (/^\/root(?:\/([^/]+))?/.exec(cwd) as RegExpExecArray | null);
  if (home) return home[1] ? `~/${home[1]}` : "~";
  const top = cwd.split("/").filter(Boolean)[0];
  return top ? `/${top}` : "/";
}

/** Whose a pane is, for clustering by person (M30): you, a teammate, or a
 * team. */
export function personOf(p: FleetPane): string {
  if (!p.person || p.person.kind === "me") return "you";
  return p.person.kind === "team" ? `team ${p.person.name}` : p.person.name;
}

export function groupOf(p: FleetPane, by: GroupBy): string {
  switch (by) {
    case "project":
      return p.info.project?.name ?? dirGroup(p.info.cwd, p.host);
    case "machine":
      return p.info.host != null ? `${p.host} vm${p.info.host}` : p.host;
    case "kind":
      return kindOf(p);
    case "session":
      return p.session ? `${p.session.name} · ${p.host}` : p.host;
    case "person":
      return personOf(p);
  }
}

/** How busy a pane looks, 0..1: brightness, and how hard it's pulled to the
 * middle of its cluster. */
export function activityOf(p: FleetPane): number {
  const bps = p.info.activity?.bps ?? 0;
  if (bps > 0) return Math.min(1, 0.4 + Math.log10(bps + 1) / 5);
  if (p.info.attention === "working" || p.info.current) return 0.35;
  return 0.04;
}

/** A pane's reason, while it still wants someone. */
export function reasonOf(p: FleetPane): Reason | null {
  const r = p.info.reason;
  if (!r || (p.info.attention !== "needs_input" && p.info.attention !== "done")) return null;
  return r;
}

/** The rail card a reason lands on: its bundle key, with "here" (the daemon
 * the pane is on) named, so failures bundle by machine across the fleet;
 * asks bundle by project and agent across machines; the rest one each. */
export function bundleOf(p: FleetPane, r: Reason): string {
  if (!r.bundle) return `one:${p.key}`;
  if (r.kind === "failed" || r.kind === "exited") {
    const [kind, ...rest] = r.bundle.split(":");
    const machine = rest.join(":");
    return `${kind}:${machine === "here" ? p.host : `${p.host}/${machine}`}`;
  }
  return r.bundle;
}

/** What a card is called, for one pane or a bundle of `n`. */
export function cardTitle(r: Reason, n: number, machines: string[], agent?: string): string {
  const where = machines.length > 1 ? "several machines" : machines[0];
  switch (r.kind) {
    case "ask":
      if (n > 1) return `${n} agents ask`;
      return `${titleCase(agent ?? r.ask?.agent ?? "An agent")} asks`;
    case "failed":
      return n > 1 ? `${n} failed on ${where}` : "Failed";
    case "exited":
      return n > 1 ? `${n} exited on ${where}` : "Exited";
    case "done":
      return n > 1 ? `${n} finished` : "Finished";
    case "input":
      return "Waiting for you";
    case "paused":
      return n > 1 ? `${n} debuggers paused` : "Debugger paused";
    case "errors":
      return n > 1 ? `${n} editors have errors` : "Errors after a save";
    case "conflict":
      return n > 1 ? `${n} merge conflicts` : "Merge conflict";
    case "diff":
      return n > 1 ? `${n} edits wait` : "Claude Code wants to edit";
    case "gate":
      // M36: a review asked of you is a gate too.
      if (r.gate?.source.kind === "forge") return n > 1 ? `${n} reviews asked of you` : "Review requested";
      return n > 1 ? `${n} workspaces wait at gates` : "Waits at a gate";
  }
}

function titleCase(s: string): string {
  // hud writes its own name in lower case.
  if (s === "hud") return s;
  return s === "claude" ? "Claude Code" : s.charAt(0).toUpperCase() + s.slice(1);
}
