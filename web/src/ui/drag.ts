// Dragging panes and tabs. A drag starts on a pane's grip or a tab and
// becomes a real drag after a few pixels; until then it's a click. While
// dragging, the element under the pointer decides the drop target:
//
// - a pane (`data-pane`): the edge nearest the pointer, or the middle;
// - a tab in the tab bar (`data-tab-index`): before or after it;
// - the rest of the tab bar: the end.
//
// The UI draws the target from `drag.current` and turns the drop into an
// intent.

import type { Edge, PaneId, TabId } from "../proto";

export type Dragged = { kind: "pane"; pane: PaneId } | { kind: "tab"; tab: TabId };

export type Target = { kind: "pane"; pane: PaneId; edge: Edge } | { kind: "tabbar"; index: number } | null;

export interface DragState {
  what: Dragged;
  label: string;
  x: number;
  y: number;
  target: Target;
}

const THRESHOLD = 5;
/** Within this fraction of a pane's edge, the drop goes to that edge. */
const EDGE_ZONE = 0.3;

let current: DragState | null = null;
const listeners = new Set<() => void>();
const changed = () => listeners.forEach((fn) => fn());

export const drag = {
  get current() {
    return current;
  },
  subscribe(fn: () => void) {
    listeners.add(fn);
    return () => listeners.delete(fn);
  },
};

export function targetAt(x: number, y: number): Target {
  const el = document.elementFromPoint(x, y);
  const pane = el?.closest<HTMLElement>("[data-pane]");
  if (pane) {
    const r = pane.getBoundingClientRect();
    const fx = (x - r.left) / r.width;
    const fy = (y - r.top) / r.height;
    const edges: [Edge, number][] = [
      ["left", fx],
      ["right", 1 - fx],
      ["top", fy],
      ["bottom", 1 - fy],
    ];
    const [edge, dist] = edges.reduce((a, b) => (b[1] < a[1] ? b : a));
    return { kind: "pane", pane: Number(pane.dataset.pane), edge: dist < EDGE_ZONE ? edge : "center" };
  }
  const tab = el?.closest<HTMLElement>("[data-tab-index]");
  if (tab) {
    const r = tab.getBoundingClientRect();
    const i = Number(tab.dataset.tabIndex);
    return { kind: "tabbar", index: x < r.left + r.width / 2 ? i : i + 1 };
  }
  if (el?.closest(".tabbar")) {
    return { kind: "tabbar", index: Number.MAX_SAFE_INTEGER };
  }
  return null;
}

/** Start tracking a possible drag. `onClick` runs if the pointer is
 * released without moving; `onDrop` runs at the end of a real drag. */
export function startDrag(
  e: PointerEvent,
  what: Dragged,
  label: string,
  handlers: { onDrop: (what: Dragged, target: Target) => void; onClick?: () => void; onHover?: (t: Target) => void },
) {
  if (e.button !== 0) return;
  const sx = e.clientX;
  const sy = e.clientY;
  let dragging = false;
  // Listen on the window: the grabbed element can disappear mid-drag (a
  // pane's tab is switched away from while dragging it).
  const move = (ev: PointerEvent) => {
    if (!dragging && Math.hypot(ev.clientX - sx, ev.clientY - sy) < THRESHOLD) return;
    if (!dragging) {
      dragging = true;
      document.body.classList.add("dragging");
    }
    const target = targetAt(ev.clientX, ev.clientY);
    current = { what, label, x: ev.clientX, y: ev.clientY, target };
    handlers.onHover?.(target);
    changed();
  };
  const finish = () => {
    window.removeEventListener("pointermove", move, true);
    window.removeEventListener("pointerup", up, true);
    window.removeEventListener("pointercancel", cancel, true);
    document.body.classList.remove("dragging");
    current = null;
    changed();
  };
  const up = (ev: PointerEvent) => {
    const was = dragging;
    finish();
    if (was) handlers.onDrop(what, targetAt(ev.clientX, ev.clientY));
    else handlers.onClick?.();
  };
  const cancel = () => finish();
  window.addEventListener("pointermove", move, true);
  window.addEventListener("pointerup", up, true);
  window.addEventListener("pointercancel", cancel, true);
}
