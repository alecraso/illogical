import { paneIds, type Client } from "../client";
import type { Attention, TabView } from "../proto";

/** The most urgent attention among a tab's panes. */
export function tabAttention(client: Client, tab: TabView): Attention {
  const states = paneIds(tab).map((p) => client.info(p)?.attention ?? "idle");
  return states.includes("needs_input") ? "needs_input" : states.includes("done") ? "done" : "idle";
}

export function AttentionBadge({ state }: { state: Attention }) {
  if (state !== "needs_input" && state !== "done") return null;
  return (
    <span class={`att ${state}`} title={state === "done" ? "Finished" : "Needs you"} aria-label={state === "done" ? "finished" : "needs you"}>
      {state === "done" ? "✓" : "●"}
    </span>
  );
}

