// The swarm's field (M26): every pane as a tile on one Canvas 2D, ported
// from the prototype (spikes/s16-swarm/canvas.html). Tiles are coloured by
// kind and lit by activity; each is pulled toward its cluster's centre in
// proportion to how busy it is, so busy panes sit in the middle and idle
// ones drift out, and they push each other apart through a grid. A pane
// whose reason has a card on the rail flares in place, then flies to the
// card with a thread back to its cluster; one waiting for room pulses in
// place. S16: physics is half of each frame, so it sleeps once everything
// has settled and wakes on a regroup, a new pane, attention or a touch.
// A folded top-right corner: its thread has messages you haven't read
// (yellow: one mentions you).

import type { WorkKind } from "../proto";
import { KINDS } from "./model";

/** M61: an unread thread, and one that mentions you. */
export const UNREAD = "#e6ebf4";
export const UNREAD_MENTION = "#f9e2af";

export interface FieldPane {
  key: string;
  kind: WorkKind;
  group: string;
  /** 0..1: how busy. */
  act: number;
  /** Its host isn't live: drawn greyed. */
  stale: boolean;
  /** `%3 cargo test` */
  label: string;
  /** Where it runs, for its tile at reading zoom. */
  where: string;
  /** When it last printed (ms), to flash when that moves. */
  lastOut: number;
  /** Its reason: a card on the rail (`bundle`), or waiting for room;
   * `since` is when it started wanting someone (ms). */
  att: { col: [number, number, number]; bundle: string | null; since?: number } | null;
  // What the city (M41) draws besides; the field doesn't need them.
  /** Its pane number. */
  id?: number;
  /** Its row inside its block: its machine (its project, by machine). */
  sub?: string;
  /** Bytes of output a second, lately. */
  bps?: number;
  /** When its command started (ms), while one runs. */
  started?: number | null;
  /** How long its last command ran (ms), how it exited, and when it ended. */
  lastDur?: number | null;
  lastExit?: number | null;
  lastEnded?: number | null;
  /** Teammates who have it open, or type in it: `driving` when they hold
   * its driver claim (M13), `typing` when they typed in the last few
   * seconds (#118). */
  people?: { name: string; driving: boolean; typing: boolean }[];
  /** Messages in its thread (M61) this person hasn't read; `mention`: one
   * is for them. */
  unread?: number;
  mention?: boolean;
}

/** What the swarm draws its panes with (M41's themes): the field (blocks)
 * or the city. The view feeds it and asks it to move. */
export interface SwarmScene {
  set(panes: FieldPane[]): void;
  regroup(): void;
  /** Fit everything in view, and keep doing so until someone moves it. */
  fitAll(): void;
  /** Go to a pane (a card's "Show", a notification's deep link). */
  diveTo(key: string): void;
  start(): void;
  stop(): void;
  resize(): void;
  /** Cluster names and sizes, as drawn (for tests and the phone's list). */
  readonly clusters: { name: string; n: number; need: number }[];
  /** Where a pane is on the screen now (tests click it). */
  screenOf(key: string): { x: number; y: number } | null;
  /** Frame-rate check. */
  measure(ms: number): Promise<{ fps: number; workP50: number; frames: number }>;
}

interface Tile extends FieldPane {
  x: number;
  y: number;
  vx: number;
  vy: number;
  flash: number;
  /** Flying to its card (screen space). */
  fly: { sx: number; sy: number; t0: number; d0: number; tx?: number; ty?: number; w?: number; h?: number; arrived: boolean } | null;
  /** Where its card was when it last saw it, for going back. */
  lastCard: DOMRect | null;
}

interface Group {
  x: number;
  y: number;
  tx: number;
  ty: number;
  r: number;
  n: number;
  busy: number;
  need: number;
}

export interface FieldHooks {
  /** Width of the rail on the right (desktop), height of the strip below
   * (phone). */
  railW(): number;
  railH(): number;
  /** Where the field starts below the bar. */
  top(): number;
  /** Where a bundle's card is on the page, if it's shown. */
  cardRect(bundle: string): DOMRect | null;
  open(key: string): void;
  hover(key: string | null, x: number, y: number): void;
  /** A right-click on a pane. */
  menu?(key: string, e: MouseEvent): void;
  /** M42: commands that finished in the last `sinceS` seconds, on every
   * connected host (each daemon's `/api/history`), for the timeline. */
  history?(sinceS: number): Promise<HistoryRun[]>;
}

/** A finished command, from a daemon's history. */
export interface HistoryRun {
  /** The pane's key (`host:id`). */
  key: string;
  text: string | null;
  started: number;
  ended: number;
  exit: number | null;
  /** Bytes it printed. */
  bytes: number;
}

const TW = 16;
const TH = 10;
const clamp = (v: number, a: number, b: number) => Math.max(a, Math.min(b, v));
const rnd = (a: number, b: number) => a + Math.random() * (b - a);
const reduce = typeof matchMedia === "function" && matchMedia("(prefers-reduced-motion: reduce)").matches;

/** Stub lengths for a tile's lines, steady per pane (no live text yet). */
const META_FONT = `500 11px "JetBrains Mono", ui-monospace, monospace`;

/** A cluster's name as drawn, and the box it takes on the screen. */
interface Label {
  name: string;
  title: string;
  meta: string;
  needText: string;
  need: number;
  big: number;
  metaW: number;
  x: number;
  y: number;
  box: { x0: number; x1: number; y0: number; y1: number };
}

function stubs(key: string): number[] {
  let h = 2166136261;
  for (let i = 0; i < key.length; i++) h = Math.imul(h ^ key.charCodeAt(i), 16777619);
  return [0, 1, 2].map((i) => 0.35 + (((h >>> (i * 8)) & 255) / 255) * 0.65);
}

export class Field implements SwarmScene {
  private cx: CanvasRenderingContext2D;
  private tiles = new Map<string, Tile>();
  private groups = new Map<string, Group>();
  /** Cluster names as last drawn. */
  private labels: Label[] = [];
  private W = 0;
  private H = 0;
  private dpr = 1;
  readonly cam = { x: 0, y: 0, z: 0.5, tx: 0, ty: 0, tz: 0.5 };
  private raf = 0;
  private last = 0;
  private lastLayout = 0;
  /** Physics asleep (S16: it's half the frame). */
  asleep = false;
  private settled = 0;
  private dirty = true;
  /** Frame times while measuring (the frame-rate test). */
  private measuring: { gaps: number[]; work: number[] } | null = null;
  private fitted = false;
  /** Someone panned, zoomed or dived: stop fitting by ourselves. */
  private userMoved = false;
  private ptrs = new Map<number, { x: number; y: number }>();
  private pinch0 = 0;
  private z0 = 1;
  private downAt: { x: number; y: number } | null = null;
  private moved = false;

  constructor(
    private cv: HTMLCanvasElement,
    private hooks: FieldHooks,
  ) {
    this.cx = cv.getContext("2d")!;
    this.listen();
  }

  // ---- data

  /** The panes now. New ones start near their cluster; gone ones go. */
  set(panes: FieldPane[]) {
    const seen = new Set<string>();
    let regroup = false;
    for (const p of panes) {
      seen.add(p.key);
      let t = this.tiles.get(p.key);
      if (!t) {
        const g = this.groups.get(p.group);
        t = { ...p, x: (g?.x ?? 0) + rnd(-60, 60), y: (g?.y ?? 0) + rnd(-60, 60), vx: 0, vy: 0, flash: 0.6, fly: null, lastCard: null };
        this.tiles.set(p.key, t);
        regroup = true;
        continue;
      }
      if (t.group !== p.group) regroup = true;
      if (p.lastOut > t.lastOut) t.flash = Math.min(1, t.flash + 0.5);
      const raised = p.att?.bundle && !t.att?.bundle;
      const lowered = t.att && !p.att;
      Object.assign(t, p);
      if (raised) {
        const s = this.toScreen(t.x, t.y);
        t.fly = { sx: s.x, sy: s.y, t0: performance.now(), d0: 0, arrived: false };
        t.flash = 1;
      } else if (!p.att?.bundle && t.fly) {
        // Acted on (or bumped off the rail): back to the swarm, from
        // where its card was.
        const r = t.lastCard;
        if (r && lowered) {
          const c = this.cv.getBoundingClientRect();
          const home = this.toWorld(r.left - c.left + r.width / 2, r.top - c.top + r.height / 2);
          t.x = home.x + rnd(-20, 20) / this.cam.z;
          t.y = home.y + rnd(-20, 20) / this.cam.z;
          t.vx = rnd(-4, 4);
          t.vy = rnd(-4, 4);
          t.flash = 1;
        }
        t.fly = null;
      }
    }
    for (const k of this.tiles.keys()) if (!seen.has(k)) this.tiles.delete(k);
    if (regroup) this.relayout(true);
    this.wake();
  }

  /** A new grouping: clusters start where their members are, so the swarm
   * visibly migrates, then the view fits them. */
  regroup() {
    this.groups.clear();
    this.relayout(false);
    for (const [k, g] of this.groups) {
      let sx = 0;
      let sy = 0;
      let c = 0;
      for (const t of this.tiles.values()) {
        if (t.group === k) {
          sx += t.x;
          sy += t.y;
          c++;
        }
      }
      if (c) {
        g.x = sx / c;
        g.y = sy / c;
      }
    }
    this.wake();
    setTimeout(() => this.fit(), 900);
  }

  private relayout(keep: boolean) {
    const placedBefore = this.groups.size;
    const counts = new Map<string, number>();
    for (const t of this.tiles.values()) counts.set(t.group, (counts.get(t.group) ?? 0) + 1);
    const names = [...counts.keys()].sort((a, b) => counts.get(b)! - counts.get(a)! || a.localeCompare(b));
    const placed: Group[] = [];
    // Spread the clusters to the view's shape: wide on a laptop, tall on a
    // phone held upright.
    const tall = this.W > 0 && this.W - this.hooks.railW() < this.H - this.hooks.railH() - this.hooks.top();
    const [sx, sy] = tall ? [0.75, 1.5] : [1.25, 0.8];
    names.forEach((name, i) => {
      const n = counts.get(name)!;
      const r = 11 * Math.sqrt(n) + 34;
      let tx = 0;
      let ty = 0;
      if (i > 0) {
        for (let d = 60; d < 6000; d += 14) {
          const a = i * 2.39996 + d * 0.004;
          tx = Math.cos(a) * d * sx;
          ty = Math.sin(a) * d * sy;
          // Upright, names sit between clusters stacked above each other:
          // leave room for them.
          if (placed.every((q) => Math.hypot(q.tx - tx, q.ty - ty) > q.r + r + (tall ? 150 : 70))) break;
        }
      }
      let g = this.groups.get(name);
      if (!g) {
        g = { x: tx, y: ty, tx, ty, r, n, busy: 0, need: 0 };
        this.groups.set(name, g);
      }
      Object.assign(g, { tx, ty, r, n });
      placed.push(g);
    });
    for (const k of [...this.groups.keys()]) if (!counts.has(k)) this.groups.delete(k);
    // Until someone moves the view themselves, it keeps everything in it.
    if (!keep || !this.fitted || (!this.userMoved && this.groups.size !== placedBefore)) this.fit();
  }

  /** Cluster names and sizes, as drawn (for tests and the phone's list). */
  get clusters(): { name: string; n: number; need: number }[] {
    return [...this.groups].map(([name, g]) => ({ name, n: g.n, need: g.need }));
  }

  /** Where a tile is on the screen now (tests click it). */
  screenOf(key: string): { x: number; y: number } | null {
    const t = this.tiles.get(key);
    if (!t) return null;
    const s = this.toScreen(t.x, t.y);
    const r = this.cv.getBoundingClientRect();
    return { x: s.x + r.left, y: s.y + r.top };
  }

  /** Each cluster name's box on the page as last drawn (tests check none
   * overlap), and what of it was shown. */
  get labelBoxes(): { name: string; x0: number; y0: number; x1: number; y1: number; meta: string; need: string }[] {
    const r = this.cv.getBoundingClientRect();
    return this.labels.map((l) => ({
      name: l.name, meta: l.meta, need: l.needText,
      x0: l.box.x0 + r.left, x1: l.box.x1 + r.left, y0: l.box.y0 + r.top, y1: l.box.y1 + r.top,
    }));
  }

  /** Where a cluster's name is on the screen now. */
  labelOf(name: string): { x: number; y: number } | null {
    const g = this.groups.get(name);
    if (!g) return null;
    const l = this.labels.find((l) => l.name === name);
    const s = l ? { x: l.x, y: l.y } : this.toScreen(g.x, g.y - g.r);
    const r = this.cv.getBoundingClientRect();
    return { x: s.x + r.left, y: s.y + r.top - 14 };
  }

  // ---- camera

  private viewCenter() {
    const top = this.hooks.top();
    return { x: (this.W - this.hooks.railW()) / 2, y: top + (this.H - this.hooks.railH() - top) / 2 };
  }
  private toScreen(x: number, y: number) {
    const c = this.viewCenter();
    return { x: (x - this.cam.x) * this.cam.z + c.x, y: (y - this.cam.y) * this.cam.z + c.y };
  }
  private toWorld(x: number, y: number) {
    const c = this.viewCenter();
    return { x: (x - c.x) / this.cam.z + this.cam.x, y: (y - c.y) / this.cam.z + this.cam.y };
  }

  /** Fit everything in view (the Fit button: and keep doing so). */
  fitAll() {
    this.userMoved = false;
    this.fit();
  }

  fit() {
    if (!this.groups.size || !this.W) return;
    let x0 = 1e9;
    let y0 = 1e9;
    let x1 = -1e9;
    let y1 = -1e9;
    for (const g of this.groups.values()) {
      x0 = Math.min(x0, g.tx - g.r);
      x1 = Math.max(x1, g.tx + g.r);
      y0 = Math.min(y0, g.ty - g.r - 30);
      y1 = Math.max(y1, g.ty + g.r);
    }
    const vw = this.W - this.hooks.railW() - 60;
    const vh = this.H - this.hooks.railH() - this.hooks.top() - 70;
    this.cam.tx = (x0 + x1) / 2;
    this.cam.ty = (y0 + y1) / 2;
    this.cam.tz = clamp(Math.min(vw / (x1 - x0), Math.max(80, vh) / (y1 - y0)), 0.15, 2);
    if (!this.fitted) {
      this.fitted = true;
      this.cam.x = this.cam.tx;
      this.cam.y = this.cam.ty;
      this.cam.z = this.cam.tz * 0.6;
    }
    this.wake();
  }

  /** Zoom to a cluster (clicking its name). */
  dive(name: string) {
    const g = this.groups.get(name);
    if (!g) return;
    this.userMoved = true;
    this.cam.tx = g.x;
    this.cam.ty = g.y;
    this.cam.tz = clamp(260 / g.r, 0.6, 2.6);
    this.wake();
  }

  /** Zoom to a pane (a card's "Show", a notification's deep link). */
  diveTo(key: string) {
    const t = this.tiles.get(key);
    if (!t) return;
    this.userMoved = true;
    this.cam.tx = t.x;
    this.cam.ty = t.y;
    this.cam.tz = 4;
    this.wake();
  }

  resize() {
    this.dpr = Math.min(devicePixelRatio || 1, 2);
    this.W = this.cv.clientWidth;
    this.H = this.cv.clientHeight;
    this.cv.width = this.W * this.dpr;
    this.cv.height = this.H * this.dpr;
    this.fit();
  }

  // ---- input: pan, zoom, pinch, hover, click

  private listen() {
    const cv = this.cv;
    cv.addEventListener("pointerdown", (e) => {
      try {
        cv.setPointerCapture(e.pointerId);
      } catch {
        // a pointer the browser no longer has
      }
      this.ptrs.set(e.pointerId, { x: e.clientX, y: e.clientY });
      this.downAt = { x: e.clientX, y: e.clientY };
      this.moved = false;
      if (this.ptrs.size === 2) {
        const [a, b] = [...this.ptrs.values()];
        this.pinch0 = Math.hypot(a.x - b.x, a.y - b.y);
        this.z0 = this.cam.tz;
      }
    });
    cv.addEventListener("pointermove", (e) => {
      const prev = this.ptrs.get(e.pointerId);
      if (!prev) return this.hover(e);
      if (this.ptrs.size === 2) {
        this.ptrs.set(e.pointerId, { x: e.clientX, y: e.clientY });
        const [a, b] = [...this.ptrs.values()];
        this.cam.tz = this.cam.z = clamp((this.z0 * Math.hypot(a.x - b.x, a.y - b.y)) / (this.pinch0 || 1), 0.12, 9);
        this.moved = true;
        this.userMoved = true;
        this.wake();
        return;
      }
      const dx = e.clientX - prev.x;
      const dy = e.clientY - prev.y;
      this.ptrs.set(e.pointerId, { x: e.clientX, y: e.clientY });
      if (this.downAt && Math.hypot(e.clientX - this.downAt.x, e.clientY - this.downAt.y) > 4) {
        this.moved = true;
        this.userMoved = true;
        cv.classList.add("panning");
        this.hooks.hover(null, 0, 0);
      }
      this.cam.x -= dx / this.cam.z;
      this.cam.y -= dy / this.cam.z;
      this.cam.tx = this.cam.x;
      this.cam.ty = this.cam.y;
      this.wake();
    });
    const up = (e: PointerEvent) => {
      this.ptrs.delete(e.pointerId);
      cv.classList.remove("panning");
      // A right-click is the menu's, not a tap: opening the pane on it
      // would also drop the menu item's click (swallowClick).
      if (!this.moved && this.ptrs.size === 0 && e.type === "pointerup" && e.button === 0) this.click(e);
    };
    cv.addEventListener("pointerup", up);
    cv.addEventListener("pointercancel", up);
    cv.addEventListener("pointerleave", () => this.hooks.hover(null, 0, 0));
    cv.addEventListener("contextmenu", (e) => {
      const r = cv.getBoundingClientRect();
      const t = this.paneAt(e.clientX - r.left, e.clientY - r.top);
      if (t && this.hooks.menu) {
        this.hooks.hover(null, 0, 0);
        this.hooks.menu(t.key, e);
      } else e.preventDefault();
    });
    cv.addEventListener(
      "wheel",
      (e) => {
        e.preventDefault();
        const r = cv.getBoundingClientRect();
        const before = this.toWorld(e.clientX - r.left, e.clientY - r.top);
        this.userMoved = true;
        this.cam.z = this.cam.tz = clamp(this.cam.z * Math.exp(-e.deltaY * 0.0015), 0.12, 9);
        const after = this.toWorld(e.clientX - r.left, e.clientY - r.top);
        this.cam.x += before.x - after.x;
        this.cam.y += before.y - after.y;
        this.cam.tx = this.cam.x;
        this.cam.ty = this.cam.y;
        this.wake();
      },
      { passive: false },
    );
  }

  private paneAt(sx: number, sy: number): Tile | null {
    const w = this.toWorld(sx, sy);
    let best: Tile | null = null;
    let bd = 1e9;
    const z = this.cam.z;
    for (const t of this.tiles.values()) {
      if (t.fly) continue;
      const d = Math.abs(t.x - w.x) / (TW / 2 + 2 / z) + Math.abs(t.y - w.y) / (TH / 2 + 2 / z);
      if (d < 2 && d < bd) {
        bd = d;
        best = t;
      }
    }
    return best;
  }

  private labelAt(sx: number, sy: number): string | null {
    const l = this.labels.find((l) => sx >= l.box.x0 && sx <= l.box.x1 && sy >= l.box.y0 && sy <= l.box.y1);
    return l?.name ?? null;
  }

  private hover(e: PointerEvent) {
    const r = this.cv.getBoundingClientRect();
    const sx = e.clientX - r.left;
    const sy = e.clientY - r.top;
    const t = this.paneAt(sx, sy);
    this.cv.style.cursor = t || this.labelAt(sx, sy) ? "pointer" : "";
    this.hooks.hover(t && this.cam.z <= 5 ? t.key : null, sx, sy);
  }

  private click(e: PointerEvent) {
    const r = this.cv.getBoundingClientRect();
    const sx = e.clientX - r.left;
    const sy = e.clientY - r.top;
    const g = this.labelAt(sx, sy);
    if (g) return this.dive(g);
    const t = this.paneAt(sx, sy);
    if (t) this.hooks.open(t.key);
  }

  // ---- loop

  start() {
    this.resize();
    this.last = performance.now();
    const frame = (now: number) => {
      const dt = Math.min(50, now - this.last);
      this.last = now;
      const t0 = performance.now();
      const cam = this.cam;
      const moving = Math.abs(cam.tx - cam.x) + Math.abs(cam.ty - cam.y) > 0.05 || Math.abs(cam.tz - cam.z) > 1e-4;
      cam.x += (cam.tx - cam.x) * 0.12;
      cam.y += (cam.ty - cam.y) * 0.12;
      cam.z += (cam.tz - cam.z) * 0.12;
      if (!this.asleep || this.measuring) this.step(now, dt);
      const animating = this.flights(now);
      if (this.dirty || moving || !this.asleep || animating || this.measuring) {
        this.draw(now);
        this.dirty = false;
      }
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

  wake() {
    this.asleep = false;
    this.settled = 0;
    this.dirty = true;
  }

  /** Frame-rate check: run (awake) for `ms`, then report fps and the median
   * work per frame. */
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

  private step(now: number, dt: number) {
    for (const t of this.tiles.values()) t.flash *= Math.pow(0.9, dt / 16);
    if (now - this.lastLayout > 1500) {
      this.relayout(true);
      this.lastLayout = now;
    }
    let groupsMoving = false;
    for (const g of this.groups.values()) {
      g.x += (g.tx - g.x) * 0.03;
      g.y += (g.ty - g.y) * 0.03;
      if (Math.abs(g.tx - g.x) + Math.abs(g.ty - g.y) > 0.5) groupsMoving = true;
      g.busy = 0;
      g.need = 0;
    }
    // Springs to the cluster, repulsion through a grid.
    const cell = 22;
    const grid = new Map<number, Tile[]>();
    for (const t of this.tiles.values()) {
      if (t.fly) continue;
      const k = (Math.floor(t.x / cell) * 73856093) ^ (Math.floor(t.y / cell) * 19349663);
      let a = grid.get(k);
      if (!a) grid.set(k, (a = []));
      a.push(t);
    }
    let maxV = 0;
    for (const t of this.tiles.values()) {
      const g = this.groups.get(t.group);
      if (!g) continue;
      if (t.act > 0.3) g.busy++;
      if (t.att) g.need++;
      if (t.fly) continue;
      const k = 0.0016 + 0.0055 * t.act;
      t.vx += (g.x - t.x) * k;
      t.vy += (g.y - t.y) * k;
      const gx = Math.floor(t.x / cell);
      const gy = Math.floor(t.y / cell);
      for (let ix = -1; ix <= 1; ix++)
        for (let iy = -1; iy <= 1; iy++) {
          const arr = grid.get(((gx + ix) * 73856093) ^ ((gy + iy) * 19349663));
          if (!arr) continue;
          for (const q of arr) {
            if (q === t) continue;
            const dx = t.x - q.x;
            const dy = (t.y - q.y) * 1.5;
            const d = Math.hypot(dx, dy) || 0.01;
            if (d < 21) {
              const f = ((21 - d) / d) * 0.09;
              t.vx += dx * f;
              t.vy += dy * f;
            }
          }
        }
      t.vx *= 0.82;
      t.vy *= 0.82;
      t.x += t.vx;
      t.y += t.vy;
      maxV = Math.max(maxV, Math.abs(t.vx) + Math.abs(t.vy));
    }
    // Settled for half a second: physics sleeps.
    if (maxV < 0.05 && !groupsMoving) {
      if (++this.settled > 30) this.asleep = true;
    } else this.settled = 0;
  }

  /** Flights to the rail, in screen space. True while any is moving. */
  private flights(now: number): boolean {
    const cr = this.cv.getBoundingClientRect();
    let moving = false;
    for (const t of this.tiles.values()) {
      if (t.att && !t.fly) moving = true; // pulsing in place
      const f = t.fly;
      if (!f) continue;
      const r = t.att?.bundle ? this.hooks.cardRect(t.att.bundle) : null;
      if (!r) continue;
      t.lastCard = r;
      if (f.arrived) {
        f.tx = r.left - cr.left + r.width / 2;
        f.ty = r.top - cr.top + r.height / 2;
        continue;
      }
      moving = true;
      if (now - f.t0 < 650 && !reduce) continue; // flare in place first
      const tx = r.left - cr.left + r.width / 2;
      const ty = r.top - cr.top + r.height / 2;
      if (!f.d0) f.d0 = Math.hypot(tx - f.sx, ty - f.sy) || 1;
      f.sx += (tx - f.sx) * (reduce ? 1 : 0.085);
      f.sy += (ty - f.sy) * (reduce ? 1 : 0.085);
      f.tx = tx;
      f.ty = ty;
      f.w = r.width;
      f.h = r.height;
      if (Math.hypot(tx - f.sx, ty - f.sy) < 6) f.arrived = true;
    }
    return moving;
  }

  private draw(now: number) {
    const { cx, W, H } = this;
    const z = this.cam.z;
    cx.setTransform(this.dpr, 0, 0, this.dpr, 0, 0);
    cx.fillStyle = "#06080d";
    cx.fillRect(0, 0, W, H);

    // Dot grid.
    const sp = 80 * z;
    if (sp > 14) {
      const o = this.toScreen(0, 0);
      cx.fillStyle = "rgba(124,134,152,.13)";
      for (let x = ((o.x % sp) + sp) % sp; x < W; x += sp) for (let y = ((o.y % sp) + sp) % sp; y < H; y += sp) cx.fillRect(x, y, 1.2, 1.2);
    }

    // Cluster halos.
    for (const g of this.groups.values()) {
      const s = this.toScreen(g.x, g.y);
      const r = g.r * z * 1.25;
      const grd = cx.createRadialGradient(s.x, s.y, 0, s.x, s.y, r);
      const hot = g.need ? "255,84,104" : "120,150,210";
      grd.addColorStop(0, `rgba(${hot},${g.need ? 0.08 : 0.05})`);
      grd.addColorStop(1, `rgba(${hot},0)`);
      cx.fillStyle = grd;
      cx.beginPath();
      cx.arc(s.x, s.y, r, 0, 7);
      cx.fill();
    }

    // Threads from panes on the rail back to their cluster.
    for (const t of this.tiles.values()) {
      const f = t.fly;
      if (!f || !t.att) continue;
      const g = this.groups.get(t.group);
      if (!g) continue;
      const s = this.toScreen(g.x, g.y);
      cx.strokeStyle = `rgba(${t.att.col},${f.arrived ? 0.1 : 0.3})`;
      cx.lineWidth = 1;
      cx.beginPath();
      cx.moveTo(s.x, s.y);
      cx.quadraticCurveTo((s.x + f.sx) / 2, Math.min(s.y, f.sy) - 60, f.arrived && f.tx !== undefined ? f.tx - (f.w ?? 0) / 2 : f.sx, f.arrived && f.ty !== undefined ? f.ty : f.sy);
      cx.stroke();
    }

    // Panes.
    const w = TW * z;
    const h = TH * z;
    const textMode = w > 70;
    const fs = Math.max(6, h / 9);
    if (textMode) cx.font = `${fs}px "JetBrains Mono", ui-monospace, monospace`;
    for (const t of this.tiles.values()) {
      if (t.fly) continue;
      const s = this.toScreen(t.x, t.y);
      if (s.x < -w || s.x > W + w || s.y < -h || s.y > H + h) continue;
      const c = KINDS[t.kind];
      const act = t.stale ? 0.02 : t.act;
      const a = (t.stale ? 0.08 : 0.16) + 0.84 * act;
      const x = s.x - w / 2;
      const y = s.y - h / 2;
      if (textMode) {
        cx.fillStyle = "rgba(10,13,20,.95)";
        cx.fillRect(x, y, w, h);
        cx.strokeStyle = `rgba(${c},${0.35 + 0.5 * act})`;
        cx.lineWidth = 1;
        cx.strokeRect(x + 0.5, y + 0.5, w - 1, h - 1);
        cx.fillStyle = `rgba(${c},${t.stale ? 0.35 : 0.9})`;
        cx.fillRect(x, y, w, fs * 1.5);
        cx.save();
        cx.beginPath();
        cx.rect(x, y, w, h);
        cx.clip();
        cx.fillStyle = "#06080d";
        cx.fillText(`${t.label} · ${t.where}`, x + fs * 0.6, y + fs * 1.15);
        cx.fillStyle = "rgba(200,210,226,.35)";
        stubs(t.key).forEach((len, i) => cx.fillRect(x + fs * 0.6, y + fs * 2.4 + i * fs * 1.35, (w - fs * 1.2) * len * act, fs * 0.5));
        cx.restore();
      } else {
        cx.fillStyle = `rgba(${c},${a})`;
        cx.fillRect(x, y, w, h);
        if (w > 16) {
          cx.fillStyle = "rgba(6,8,13,.55)";
          stubs(t.key).forEach((len, i) => cx.fillRect(x + w * 0.1, y + h * (0.25 + i * 0.22), w * 0.8 * len, Math.max(1, h * 0.08)));
        }
      }
      if (t.flash > 0.05) {
        cx.fillStyle = `rgba(255,255,255,${t.flash * 0.5})`;
        cx.fillRect(x, y, w, h);
      }
      if (t.unread) {
        // M61: an unread thread folds the tile's top-right corner.
        const k = Math.max(4, Math.min(w, h) * 0.22);
        cx.fillStyle = t.mention ? UNREAD_MENTION : UNREAD;
        cx.beginPath();
        cx.moveTo(x + w - k, y);
        cx.lineTo(x + w, y);
        cx.lineTo(x + w, y + k);
        cx.fill();
      }
      if (t.att) {
        // Waiting for room on the rail: pulsing in place.
        const ph = (now / 500) % 1;
        cx.strokeStyle = `rgba(${t.att.col},${1 - ph})`;
        cx.lineWidth = 2;
        cx.strokeRect(x - ph * 14, y - ph * 14, w + ph * 28, h + ph * 28);
      }
    }

    // Panes flying to their cards (screen space), growing toward them.
    for (const t of this.tiles.values()) {
      const f = t.fly;
      if (!f || f.arrived || !t.att) continue;
      const col = t.att.col;
      const age = now - f.t0;
      if (age < 650 && !reduce) {
        // Flare where it lives.
        const s = this.toScreen(t.x, t.y);
        f.sx = s.x;
        f.sy = s.y;
        const k = age / 650;
        cx.strokeStyle = `rgba(${col},${1 - k})`;
        cx.lineWidth = 2;
        cx.beginPath();
        cx.arc(s.x, s.y, 6 + k * 46, 0, 7);
        cx.stroke();
        cx.fillStyle = `rgba(${col},1)`;
        cx.fillRect(s.x - w / 2 - 2, s.y - h / 2 - 2, w + 4, h + 4);
        continue;
      }
      const prog = f.d0 ? 1 - Math.hypot((f.tx ?? f.sx) - f.sx, (f.ty ?? f.sy) - f.sy) / f.d0 : 0;
      const ww = w + ((f.w ?? 300) - w) * prog * prog;
      const hh = h + ((f.h ?? 130) - h) * prog * prog;
      cx.shadowColor = `rgba(${col},.8)`;
      cx.shadowBlur = 18;
      cx.fillStyle = `rgba(${col},${0.9 - prog * 0.6})`;
      cx.fillRect(f.sx - ww / 2, f.sy - hh / 2, ww, hh);
      cx.shadowBlur = 0;
    }

    // Cluster names, placed so no two overlap (a phone held upright puts
    // clusters close together): each tries its full line, then without
    // "· N busy", then its name alone, then nudged up or down.
    this.labels = this.placeLabels(z);
    for (const l of this.labels) {
      cx.textAlign = "center";
      cx.font = `800 ${l.big}px "Big Shoulders Display", "Arial Narrow", sans-serif`;
      cx.fillStyle = l.need ? "#ffd5da" : "rgba(230,235,244,.92)";
      cx.fillText(l.title, l.x, l.y - 12);
      if (l.meta || l.needText) {
        cx.font = META_FONT;
        cx.textAlign = "start";
        let x = l.x - l.metaW / 2;
        if (l.meta) {
          cx.fillStyle = "rgba(124,134,152,.95)";
          cx.fillText(l.meta, x, l.y + 2);
          x += cx.measureText(l.meta).width;
        }
        if (l.needText) {
          cx.fillStyle = "#ff5468";
          cx.fillText(l.needText, x, l.y + 2);
        }
      }
      cx.textAlign = "start";
    }
  }

  /** Where each cluster's name goes this frame, and what of it fits. */
  private placeLabels(z: number): Label[] {
    const { cx, W } = this;
    const big = clamp(14 + z * 10, 14, 30);
    const placed: Label[] = [];
    const hits = (l: Label) =>
      placed.some((q) => l.box.x0 < q.box.x1 && q.box.x0 < l.box.x1 && l.box.y0 < q.box.y1 && q.box.y0 < l.box.y1);
    // Clusters that need you first, then the biggest: they keep the most.
    const order = [...this.groups].sort(([, a], [, b]) => b.need - a.need || b.n - a.n);
    for (const [name, g] of order) {
      const s = this.toScreen(g.x, g.y - g.r);
      const title = name.toUpperCase();
      cx.font = `800 ${big}px "Big Shoulders Display", "Arial Narrow", sans-serif`;
      const titleW = cx.measureText(title).width;
      cx.font = META_FONT;
      const panes = `${g.n} pane${g.n === 1 ? "" : "s"}`;
      const needText = g.need ? `${g.need} need you` : "";
      // Far out on a small screen, the counts would only crowd it.
      const terse = z < 0.45 && W < 760;
      const variants: [string, string][] = [
        ...(terse ? [] : [[`${panes} · ${g.busy} busy${needText ? " · " : ""}`, needText] as [string, string]]),
        ...(needText ? [[terse ? "" : `${panes} · `, needText] as [string, string], ["", needText] as [string, string]] : []),
        ...(!terse && !needText ? [[panes, ""] as [string, string]] : []),
        ["", ""],
      ];
      const make = (meta: string, need: string, dy: number): Label => {
        cx.font = META_FONT;
        const metaW = cx.measureText(meta + need).width;
        const lines = meta || need;
        const w = Math.max(titleW, metaW) + 8;
        const y = s.y + dy;
        return {
          name, title, meta, needText: need, need: g.need, big, metaW, x: s.x, y,
          box: { x0: s.x - w / 2, x1: s.x + w / 2, y0: y - 12 - big * 0.85, y1: lines ? y + 6 : y - 8 },
        };
      };
      let chosen: Label | null = null;
      for (const [meta, need] of variants) {
        const l = make(meta, need, 0);
        if (!hits(l)) {
          chosen = l;
          break;
        }
      }
      // Still in the way: the shortest form, nudged up or down a little.
      if (!chosen) {
        const [meta, need] = variants[variants.length - (needText ? 2 : 1)];
        for (let k = 1; k <= 8 && !chosen; k++) {
          for (const dy of [-k * 9, k * 9]) {
            const l = make(meta, need, dy);
            if (!hits(l)) {
              chosen = l;
              break;
            }
          }
        }
        chosen ??= make("", "", 0);
      }
      placed.push(chosen);
    }
    return placed;
  }
}
