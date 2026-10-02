import { paneIds, type Client } from "../client";
import type { Attention, PaneId, Reason, TabView } from "../proto";
import { openMenu, type MenuItem } from "./menu";

/** The most urgent attention among a tab's panes, and why. */
export function tabAttention(client: Client, tab: TabView): { state: Attention; reason?: Reason | null; pane?: PaneId } {
  const infos = paneIds(tab).map((p) => client.info(p));
  const pick = (s: Attention) => infos.find((i) => i?.attention === s);
  const top = pick("needs_input") ?? pick("done");
  return top ? { state: top.attention, reason: top.reason, pane: top.id } : { state: "idle" };
}

/** What can be done about a pane's reason from a menu (M11: Rerun, and
 * Dismiss), for someone who may. */
export function reasonItems(client: Client, pane: PaneId, reason: Reason | null | undefined): MenuItem[] {
  const tab = client.tabOfPane(pane);
  const session = tab ? client.sessionOfTab(tab.id) : undefined;
  if (!reason || client.role(session ?? null) === "viewer") return [];
  const items: MenuItem[] = [];
  if (reason.actions.includes("rerun")) items.push({ label: `Rerun ${reason.command ?? ""}`.trim(), run: () => void client.act({ action: "rerun", pane }) });
  if (reason.actions.includes("dismiss")) items.push({ label: "Dismiss", run: () => void client.act({ action: "dismiss", pane }) });
  return items;
}

/** A dot for "needs you", a tick for "done", a cross for a failure (M24);
 * the reason's headline is its title. With a client and pane, a failure
 * that can be run again opens a menu with Rerun (M11). */
export function AttentionBadge({ state, reason, pane, client }: { state: Attention; reason?: Reason | null; pane?: PaneId; client?: Client }) {
  if (state !== "needs_input" && state !== "done") return null;
  const failed = reason?.kind === "failed" || reason?.kind === "exited";
  const label = reason?.headline ?? (state === "done" ? "Finished" : "Needs you");
  const items = client && pane !== undefined && reason?.actions.includes("rerun") ? reasonItems(client, pane, reason) : [];
  return (
    <span
      class={`att ${state}${failed ? " failed" : ""}${items.length ? " acts" : ""}`}
      title={label}
      aria-label={label}
      data-rerun-badge={items.length ? pane : undefined}
      onPointerDown={items.length ? (e) => e.stopPropagation() : undefined}
      onClick={
        items.length
          ? (e) => {
              e.stopPropagation();
              openMenu(e, [{ header: label }, ...items]);
            }
          : undefined
      }
    >
      {failed ? "✗" : state === "done" ? "✓" : "●"}
    </span>
  );
}
