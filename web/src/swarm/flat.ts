// What the flat 2D themes (M42: hive and timeline) share: a canvas, a camera
// that pans, zooms and pinches, fitting what's drawn into the part of the
// screen the bar and rail leave free until someone moves it, hover, click
// and right-click on a pane, the frame loop and the frame-rate check. Each
// theme lays out its panes in world units and draws them; this does the rest.

import type { WorkKind } from "../proto";
import type { FieldHooks, FieldPane, SwarmScene } from "./field";

/** Kinds that run until stopped (or aren't processes): no command that
 * finishes, so nothing grows. The city's `LONG`. */
export const UNTIL_STOPPED: ReadonlySet<WorkKind> = new Set<WorkKind>(["server", "logs", "app", "editor", "pr", "issue", "fountain"]);

export const reduce = typeof matchMedia === "function" && matchMedia("(prefers-reduced-motion: reduce)").matches;
export const clamp = (v: number, a: number, b: number) => Math.max(a, Math.min(b, v));

/** 0 silent, else 0.15..1 on a log scale of bytes a second. */
export function rateOf(bps: number | undefined): number {
  return bps && bps > 0 ? Math.min(1, 0.15 + Math.log10(bps) / 4.2) : 0;
}

/** How long its command has run (s): stopped where it was while it waits
 * on you; the last one's when it's done; null if it never ran one. */
export function runtimeOf(p: FieldPane, now: number): number | null {
  if (p.started != null) {
    const end = p.att?.since && p.att.since > p.started ? Math.min(now, p.att.since) : now;
    return Math.max(0, (end - p.started) / 1000);
  }
  if (p.lastDur != null) return p.lastDur / 1000;
  return null;
}

/** A pane waits on you (not just "finished"). */
export const needs = (p: FieldPane) => !!p.att;

export function fmtDur(s: number): string {
  if (s < 60) return `${Math.round(s)}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m${String(Math.floor(s % 60)).padStart(2, "0")}s`;
  return `${Math.floor(s / 3600)}h${String(Math.floor((s % 3600) / 60)).padStart(2, "0")}m`;
}

export const FONT = {
  display: (w: number, px: number) => `${w} ${px}px "Big Shoulders Display", "Arial Narrow", sans-serif`,
  mono: (w: number, px: number) => `${w} ${px}px "JetBrains Mono", ui-monospace, monospace`,
};
export const C = { void: "#06080d", ink: "#e6ebf4", mute: "#7c8698", line: "#1a2030", fail: "#ff5468" };

/** A world rectangle. */
export interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}

export abstract class Flat implements SwarmScene {
  protected cx: CanvasRenderingContext2D;
  protected panes = new Map<string, FieldPane>();
  protected W = 0;
  protected H = 0;
  protected dpr = 1;
  /** Screen = world · s + (x, y); it eases toward (tx, ty, ts). */
  readonly cam = { x: 0, y: 0, s: 1, tx: 0, ty: 0, ts: 1 };
  protected minS = 0.2;
  protected maxS = 6;
  /** Someone panned, zoomed or dived: stop fitting by ourselves. */
  protected userMoved = false;
  private raf = 0;
  private measuring: { gaps: number[]; work: number[] } | null = null;
  private ptrs = new Map<number, { x: number; y: number }>();
  private pinch0 = 0;
  private s0 = 1;
  private downAt: { x: number; y: number } | null = null;
  private moved = false;
  private last = 0;

  constructor(
    protected cv: HTMLCanvasElement,
    protected hooks: FieldHooks,
  ) {
    this.cx = cv.getContext("2d")!;
    this.listen();
  }

  // ---- what a theme provides

  /** Lay out the panes (a new set, a regroup or a resize). */
  protected abstract layout(): void;
  /** What to fit in view, in world units. */
  protected abstract bounds(): Rect;
  /** Where a pane is, in world units. */
  protected abstract posOf(key: string): { x: number; y: number } | null;
  /** The pane under a screen point. */
  protected abstract paneAt(sx: number, sy: number): FieldPane | null;
  /** Draw a frame. */
  protected abstract draw(now: number): void;
  abstract get clusters(): { name: string; n: number; need: number }[];
  /** How close a dive zooms. */
  protected diveScale = 2.4;
  /** The tightest a fit zooms in. */
  protected fitMax = 2.2;

  // ---- data

  set(list: FieldPane[]) {
    const before = this.shape();
    this.panes = new Map(list.map((p) => [p.key, p]));
    if (this.shape() !== before) {
      this.layout();
      if (!this.userMoved) this.fit(false);
    }
  }

  /** What changes the layout: the panes and their groups and rows. */
  private shape(): string {
    let s = "";
    for (const p of this.panes.values()) s += `${p.key}\u0001${p.group}\u0001${p.sub ?? ""}\u0002`;
    return s;
  }

  regroup() {
    this.layout();
    this.userMoved = false;
    this.fit(true);
  }

  // ---- camera

  /** The part of the canvas the bar and rail leave free. */
  protected free(): Rect {
    const top = this.hooks.top();
    return { x: 0, y: top, w: Math.max(1, this.W - this.hooks.railW()), h: Math.max(1, this.H - this.hooks.railH() - top) };
  }

  protected toWorld(sx: number, sy: number) {
    return { x: (sx - this.cam.x) / this.cam.s, y: (sy - this.cam.y) / this.cam.s };
  }
  protected toScreen(wx: number, wy: number) {
    return { x: wx * this.cam.s + this.cam.x, y: wy * this.cam.s + this.cam.y };
  }

  fitAll() {
    this.userMoved = false;
    this.fit(true);
  }

  protected fit(ease: boolean) {
    if (!this.W) return;
    const b = this.bounds();
    const f = this.free();
    const pad = 24;
    // Clear of the legend and key under it too, as the field leaves room.
    const h = Math.max(80, f.h - 70);
    const s = clamp(Math.min((f.w - pad * 2) / Math.max(1, b.w), (h - pad) / Math.max(1, b.h)), this.minS, this.fitMax);
    this.cam.ts = s;
    this.cam.tx = f.x + (f.w - b.w * s) / 2 - b.x * s;
    this.cam.ty = f.y + (h - b.h * s) / 2 - b.y * s;
    if (!ease || reduce) Object.assign(this.cam, { x: this.cam.tx, y: this.cam.ty, s: this.cam.ts });
  }

  /** Centre a world point at scale `s`. */
  protected centre(wx: number, wy: number, s: number) {
    const f = this.free();
    this.cam.ts = s;
    this.cam.tx = f.x + f.w / 2 - wx * s;
    this.cam.ty = f.y + f.h / 2 - wy * s;
    if (reduce) Object.assign(this.cam, { x: this.cam.tx, y: this.cam.ty, s: this.cam.ts });
  }

  diveTo(key: string) {
    const p = this.posOf(key);
    if (!p) return;
    this.userMoved = true;
    this.centre(p.x, p.y, Math.max(this.cam.ts, this.diveScale));
  }

  /** Where a pane is on the page now (tests click it). */
  screenOf(key: string): { x: number; y: number } | null {
    const p = this.posOf(key);
    if (!p) return null;
    const s = this.toScreen(p.x, p.y);
    const r = this.cv.getBoundingClientRect();
    return { x: s.x + r.left, y: s.y + r.top };
  }

  resize() {
    this.dpr = Math.min(devicePixelRatio || 1, 2);
    this.W = this.cv.clientWidth;
    this.H = this.cv.clientHeight;
    this.cv.width = this.W * this.dpr;
    this.cv.height = this.H * this.dpr;
    this.layout();
    if (!this.userMoved) this.fit(false);
  }

  // ---- input

  private zoomAt(sx: number, sy: number, s: number) {
    s = clamp(s, this.minS, this.maxS);
    const c = this.cam;
    c.x = sx - (sx - c.x) * (s / c.s);
    c.y = sy - (sy - c.y) * (s / c.s);
    c.s = s;
    c.tx = c.x;
    c.ty = c.y;
    c.ts = s;
  }

  private local(e: { clientX: number; clientY: number }) {
    const r = this.cv.getBoundingClientRect();
    return { x: e.clientX - r.left, y: e.clientY - r.top };
  }

  private listen() {
    const cv = this.cv;
    cv.addEventListener("pointerdown", (e) => {
      try {
        cv.setPointerCapture(e.pointerId);
      } catch {
        // a pointer the browser no longer has
      }
      this.ptrs.set(e.pointerId, this.local(e));
      this.downAt = { x: e.clientX, y: e.clientY };
      this.moved = false;
      if (this.ptrs.size === 2) {
        const [a, b] = [...this.ptrs.values()];
        this.pinch0 = Math.hypot(a.x - b.x, a.y - b.y);
        this.s0 = this.cam.s;
      }
    });
    cv.addEventListener("pointermove", (e) => {
      const prev = this.ptrs.get(e.pointerId);
      const at = this.local(e);
      if (!prev) return this.hover(at.x, at.y);
      this.ptrs.set(e.pointerId, at);
      if (this.ptrs.size === 2) {
        const [a, b] = [...this.ptrs.values()];
        this.zoomAt((a.x + b.x) / 2, (a.y + b.y) / 2, (this.s0 * Math.hypot(a.x - b.x, a.y - b.y)) / (this.pinch0 || 1));
        this.moved = true;
        this.userMoved = true;
        return;
      }
      if (this.downAt && Math.hypot(e.clientX - this.downAt.x, e.clientY - this.downAt.y) > 4) {
        if (!this.moved) this.hooks.hover(null, 0, 0);
        this.moved = true;
        this.userMoved = true;
        cv.classList.add("panning");
      }
      if (!this.moved) return;
      this.cam.x += at.x - prev.x;
      this.cam.y += at.y - prev.y;
      this.cam.tx = this.cam.x;
      this.cam.ty = this.cam.y;
      this.cam.ts = this.cam.s;
    });
    const up = (e: PointerEvent) => {
      this.ptrs.delete(e.pointerId);
      cv.classList.remove("panning");
      // A right-click is the menu's, not a tap: opening the pane on it
      // would also drop the menu item's click (swallowClick).
      if (!this.moved && this.ptrs.size === 0 && e.type === "pointerup" && e.button === 0) {
        const at = this.local(e);
        const p = this.paneAt(at.x, at.y);
        if (p) this.hooks.open(p.key);
      }
      if (this.ptrs.size === 0) this.downAt = null;
    };
    cv.addEventListener("pointerup", up);
    cv.addEventListener("pointercancel", up);
    cv.addEventListener("pointerleave", () => this.hooks.hover(null, 0, 0));
    cv.addEventListener("contextmenu", (e) => {
      const at = this.local(e);
      const p = this.paneAt(at.x, at.y);
      if (p && this.hooks.menu) {
        this.hooks.hover(null, 0, 0);
        this.hooks.menu(p.key, e);
      } else e.preventDefault();
    });
    cv.addEventListener(
      "wheel",
      (e) => {
        e.preventDefault();
        const at = this.local(e);
        this.userMoved = true;
        this.zoomAt(at.x, at.y, this.cam.s * Math.exp(-e.deltaY * 0.0015));
      },
      { passive: false },
    );
  }

  private hover(sx: number, sy: number) {
    const p = this.paneAt(sx, sy);
    this.cv.style.cursor = p ? "pointer" : "";
    this.hooks.hover(p?.key ?? null, sx, sy);
  }

  // ---- loop

  start() {
    this.resize();
    this.last = performance.now();
    const frame = (t: number) => {
      const dt = Math.min(50, t - this.last);
      this.last = t;
      const t0 = performance.now();
      const c = this.cam;
      const k = 0.14;
      c.x += (c.tx - c.x) * k;
      c.y += (c.ty - c.y) * k;
      c.s += (c.ts - c.s) * k;
      this.cx.setTransform(this.dpr, 0, 0, this.dpr, 0, 0);
      this.draw(Date.now());
      if (this.measuring) {
        this.measuring.gaps.push(dt);
        this.measuring.work.push(performance.now() - t0);
      }
      this.raf = requestAnimationFrame(frame);
    };
    this.raf = requestAnimationFrame(frame);
  }

  stop() {
    cancelAnimationFrame(this.raf);
  }

  async measure(ms: number): Promise<{ fps: number; workP50: number; frames: number }> {
    this.measuring = { gaps: [], work: [] };
    await new Promise((r) => setTimeout(r, ms));
    const m = this.measuring;
    this.measuring = null;
    const gaps = m.gaps.slice(5);
    const work = [...m.work].sort((a, b) => a - b);
    const total = gaps.reduce((a, b) => a + b, 0);
    return { fps: gaps.length ? (1000 * gaps.length) / total : 0, workP50: work[Math.floor(work.length / 2)] ?? 0, frames: gaps.length };
  }

  /** The world transform, for drawing in world units. */
  protected world() {
    const { dpr, cam } = this;
    this.cx.setTransform(dpr * cam.s, 0, 0, dpr * cam.s, dpr * cam.x, dpr * cam.y);
  }
  protected screen() {
    this.cx.setTransform(this.dpr, 0, 0, this.dpr, 0, 0);
  }

  /** Groups in drawing order (biggest first, as the field places them),
   * each with its panes by row (machine) and pane number. */
  protected grouped(): { name: string; ps: FieldPane[] }[] {
    const m = new Map<string, FieldPane[]>();
    for (const p of this.panes.values()) {
      let a = m.get(p.group);
      if (!a) m.set(p.group, (a = []));
      a.push(p);
    }
    return [...m.entries()]
      .sort((a, b) => b[1].length - a[1].length || a[0].localeCompare(b[0]))
      .map(([name, ps]) => ({ name, ps: ps.sort((a, b) => (a.sub ?? "").localeCompare(b.sub ?? "") || a.where.localeCompare(b.where) || (a.id ?? 0) - (b.id ?? 0) || a.key.localeCompare(b.key)) }));
  }
}
