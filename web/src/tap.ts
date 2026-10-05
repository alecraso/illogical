// Where a tap on a notification goes (sw.ts): the pane (its card shows over
// it), through control (M21) the pane on the daemon it came from; something
// that wants you (M24's reason) at its card on the swarm's rail (M26); a
// mention (M61) with its thread open.

/** A thread worth opening: a pane's or a session's. */
export function tapThread(thread?: string): string | undefined {
  return thread && /^(pane|session)-\d+$/.test(thread) ? thread : undefined;
}

export function tapUrl(n: { pane?: number; daemon?: string; thread?: string; reason?: unknown }): string {
  if (!n.pane) return "/";
  const where = n.reason ? "swarm" : "pane";
  const thread = tapThread(n.thread);
  return (n.daemon ? `/#${where}=${n.daemon}.${n.pane}` : `/#${where}=${n.pane}`) + (thread ? `&thread=${thread}` : "");
}
