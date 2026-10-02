import { paneIds, type Client } from "../client";
import type { Attention, Reason, TabView } from "../proto";

/** The most urgent attention among a tab's panes, and why. */
export function tabAttention(client: Client, tab: TabView): { state: Attention; reason?: Reason | null } {
  const infos = paneIds(tab).map((p) => client.info(p));
  const pick = (s: Attention) => infos.find((i) => i?.attention === s);
  const top = pick("needs_input") ?? pick("done");
  return top ? { state: top.attention, reason: top.reason } : { state: "idle" };
}

/** A dot for "needs you", a tick for "done", a cross for a failure (M24);
 * the reason's headline is its title. */
export function AttentionBadge({ state, reason }: { state: Attention; reason?: Reason | null }) {
  if (state !== "needs_input" && state !== "done") return null;
  const failed = reason?.kind === "failed" || reason?.kind === "exited";
  const label = reason?.headline ?? (state === "done" ? "Finished" : "Needs you");
  return (
    <span class={`att ${state}${failed ? " failed" : ""}`} title={label} aria-label={label}>
      {failed ? "✗" : state === "done" ? "✓" : "●"}
    </span>
  );
}
