// Where the swarm lives (M26): `/#swarm`, beside the tabs. `#swarm=N` or
// `#swarm=<daemon>.N` (a notification's deep link) opens it at pane N's card.

export interface SwarmRoute {
  /** A pane to show: its daemon (through control), and its id. */
  focus: { daemon?: string; pane: number } | null;
}

const listeners = new Set<() => void>();

export function swarmRoute(): SwarmRoute | null {
  const m = /^#swarm(?:=(?:([0-9a-f]+)\.)?(\d+))?$/.exec(location.hash);
  if (!m) return null;
  return { focus: m[2] ? { daemon: m[1], pane: Number(m[2]) } : null };
}

export function onSwarmRoute(fn: () => void): () => void {
  listeners.add(fn);
  return () => listeners.delete(fn);
}

function changed() {
  for (const fn of listeners) fn();
}
addEventListener("hashchange", changed);

export function openSwarm() {
  if (location.hash !== "#swarm") location.hash = "swarm";
}

export function closeSwarm() {
  history.replaceState(null, "", location.pathname + location.search);
  changed();
}
