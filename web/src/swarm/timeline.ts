// The swarm as a timeline (M42): one lane per pane under its cluster's
// name, time running left to right with now at the right edge, so it
// answers "what happened while I was away". Every mark is one thing a
// pane reported:
//
// - a bar is a command: its length is how long it ran, its colour its
//   kind, its stripes how much it printed (denser for more bytes a
//   second); a red cap: it failed;
// - the running command is brighter, its last four minutes shaded by the
//   bytes a second it printed, with a lit leading edge while it grows;
// - what runs until stopped (servers, logs, apps, editors) is a thin line;
// - needs you: a band in the reason's colour from when it started wanting
//   you until now, and a tag past the now edge saying how long;
// - a dot at the now edge: a teammate has it open (ringed while they
//   type); a grey lane: its machine isn't connected;
// - a square past the teammates' dots: its thread has messages you
//   haven't read (yellow: one mentions you) (M61).
//
// The past comes from each daemon's command history (`/api/history`, the
// last 45 minutes, refreshed every half minute) and from what the view
// sees finish while it's open; the live output from the panes' `bps`.

import { KINDS, REASON_COL } from "./model";
import { UNREAD, UNREAD_MENTION, type FieldHooks, type FieldPane } from "./field";
import { C, Flat, FONT, fmtDur, needs, rateOf, reduce, UNTIL_STOPPED, type Rect } from "./flat";

/** Seconds shown when it fits. */
const SPAN = 40 * 60;
/** Seconds of past kept. */
const KEEP = 45 * 60;
const LANE = 11;
const BAR = 7;
const HEAD = 34;
const AXIS = 26;
/** Room right of now for the "waits 4m" tags (screen px). */
const RIGHT = 92;
/** Seconds of live output kept per pane, one sample a second. */
const LIVE = 240;
const DONE = REASON_COL.done.join(",");

export interface Run {
  text: string | null;
  t0: number;
  t1: number;
  exit: number | null;
  /** 0..1: how much it printed, on the bytes-a-second scale. */
  out: number;
}

interface Lane {
  key: string;
  y: number;
}

/** Ticks far enough apart on the screen. */
const STEPS = [60, 120, 300, 600, 900, 1800, 3600, 7200];
function fmtAgo(s: number): string {
  if (s <= 0) return "now";
  if (s < 3600) return `−${Math.round(s / 60)}m`;
  return `−${Math.floor(s / 3600)}h${String(Math.round((s % 3600) / 60)).padStart(2, "0")}`;
}

export class Timeline extends Flat {
  private lanes: Lane[] = [];
  private byKey = new Map<string, Lane>();
  private heads: { name: string; y: number; keys: string[] }[] = [];
  private total = 0;
  /** World units per second at scale 1. */
  private pps = 0.4;
  private labelW = 170;
  /** Finished commands per pane, by start time. */
  private runs = new Map<string, Map<number, Run>>();
  /** Bytes a second, a sample a second, newest last. */
  private live = new Map<string, number[]>();
  /** What each pane was running when last seen, to notice it finishing. */
  private seen = new Map<string, { started: number; sum: number; n: number }>();
  private lastSample = 0;
  private lastFetch = 0;
  private fetching = false;
  private pats = new Map<string, CanvasPattern[]>();

  constructor(cv: HTMLCanvasElement, hooks: FieldHooks) {
    super(cv, hooks);
    this.minS = 0.35;
    this.maxS = 4;
  }

  get clusters() {
    return this.heads.map((h) => ({ name: h.name, n: h.keys.length, need: h.keys.filter((k) => needs(this.panes.get(k)!)).length }));
  }

  /** A pane's finished commands, oldest first (tests count them). */
  runsOf(key: string): Run[] {
    return [...(this.runs.get(key)?.values() ?? [])].sort((a, b) => a.t0 - b.t0);
  }

  set(list: FieldPane[]) {
    super.set(list);
    const now = Date.now();
    for (const p of list) {
      // The last command, as the pane reports it.
      if (p.lastEnded != null && p.lastDur != null) this.add(p.key, { text: null, t0: p.lastEnded - p.lastDur, t1: p.lastEnded, exit: p.lastExit ?? null, out: -1 });
      // One that was running and isn't now: it finished while we watched.
      const was = this.seen.get(p.key);
      if (was && was.started !== p.started) {
        const t1 = p.lastEnded ?? now;
        if (t1 > was.started) this.add(p.key, { text: null, t0: was.started, t1, exit: p.lastExit ?? null, out: was.n ? rateOf(was.sum / was.n) : -1 });
        this.seen.delete(p.key);
      }
      if (p.started != null && !UNTIL_STOPPED.has(p.kind) && !this.seen.has(p.key)) this.seen.set(p.key, { started: p.started, sum: 0, n: 0 });
    }
  }

  /** Keep a run, merging with what the daemon's history said about it. */
  private add(key: string, r: Run) {
    let m = this.runs.get(key);
    if (!m) this.runs.set(key, (m = new Map()));
    const t0 = Math.round(r.t0 / 1000);
    const had = m.get(t0);
    m.set(t0, had ? { ...had, text: had.text ?? r.text, exit: had.exit ?? r.exit, out: had.out >= 0 ? had.out : r.out } : r);
  }

  private async fetchHistory(now: number) {
    if (!this.hooks.history || this.fetching) return;
    this.fetching = true;
    this.lastFetch = now;
    try {
      for (const h of await this.hooks.history(KEEP)) {
        const dur = Math.max(1, (h.ended - h.started) / 1000);
        this.add(h.key, { text: h.text, t0: h.started, t1: h.ended, exit: h.exit, out: h.bytes > 0 ? rateOf(h.bytes / dur) : 0 });
      }
    } catch {
      // a host went away: try again next time
    } finally {
      this.fetching = false;
    }
  }

  private sample(now: number) {
    for (const p of this.panes.values()) {
      let a = this.live.get(p.key);
      if (!a) this.live.set(p.key, (a = []));
      const b = p.stale ? 0 : (p.bps ?? 0);
      a.push(b);
      if (a.length > LIVE) a.shift();
      const s = this.seen.get(p.key);
      if (s) {
        s.sum += b;
        s.n++;
      }
    }
    for (const k of this.live.keys()) if (!this.panes.has(k)) this.live.delete(k);
    for (const [k, m] of this.runs) {
      for (const [t, r] of m) if (r.t1 < now - KEEP * 1000) m.delete(t);
      if (!m.size) this.runs.delete(k);
    }
  }

  // ---- layout and camera

  protected layout() {
    this.lanes = [];
    this.byKey.clear();
    this.heads = [];
    let y = 0;
    for (const g of this.grouped()) {
      this.heads.push({ name: g.name, y, keys: g.ps.map((p) => p.key) });
      y += HEAD;
      for (const p of g.ps) {
        const l = { key: p.key, y };
        this.lanes.push(l);
        this.byKey.set(p.key, l);
        y += LANE;
      }
      y += 8;
    }
    this.total = y;
    const f = this.free();
    this.labelW = f.w < 560 ? 92 : 170;
    this.pps = Math.max(0.05, (f.w - this.labelW - RIGHT - 16) / SPAN);
  }

  protected bounds(): Rect {
    return { x: -SPAN * this.pps, y: 0, w: SPAN * this.pps, h: this.total };
  }

  /** Fitting means the last 40 minutes across, lanes at their own height
   * from the top: 150 lanes scroll rather than shrink to hairlines. */
  protected fit(ease: boolean) {
    if (!this.W) return;
    const f = this.free();
    this.cam.ts = 1;
    this.cam.tx = f.x + f.w - RIGHT;
    this.cam.ty = f.y + AXIS + 4;
    if (!ease || reduce) Object.assign(this.cam, { x: this.cam.tx, y: this.cam.ty, s: 1 });
  }

  /** A pane: its lane in the middle, now where it was. */
  diveTo(key: string) {
    const l = this.byKey.get(key);
    if (!l) return;
    const f = this.free();
    this.userMoved = true;
    const s = Math.max(1, this.cam.ts);
    this.cam.ts = s;
    this.cam.tx = f.x + f.w - RIGHT;
    this.cam.ty = f.y + f.h / 2 - (l.y + LANE / 2) * s;
    if (reduce) Object.assign(this.cam, { x: this.cam.tx, y: this.cam.ty, s });
  }

  protected posOf(key: string) {
    const l = this.byKey.get(key);
    return l ? { x: -40 / this.cam.s, y: l.y + LANE / 2 - 1 } : null;
  }

  protected paneAt(_sx: number, sy: number): FieldPane | null {
    if (sy < this.free().y + AXIS) return null;
    const wy = (sy - this.cam.y) / this.cam.s;
    for (const l of this.lanes) if (wy >= l.y && wy < l.y + LANE) return this.panes.get(l.key) ?? null;
    return null;
  }

  // ---- drawing

  private patterns() {
    for (const [k, c] of Object.entries(KINDS)) {
      this.pats.set(
        k,
        [8, 5, 3].map((w) => {
          const t = document.createElement("canvas");
          t.width = w;
          t.height = 4;
          const x = t.getContext("2d")!;
          x.fillStyle = `rgba(${c},0.55)`;
          x.fillRect(0, 0, 1, 4);
          return this.cx.createPattern(t, "repeat")!;
        }),
      );
    }
  }

  protected draw(now: number) {
    if (now - this.lastSample >= 1000) {
      this.lastSample = now;
      this.sample(now);
    }
    if (now - this.lastFetch > 30_000) void this.fetchHistory(now);
    if (!this.pats.size) this.patterns();
    const { cx, cam, W, H } = this;
    const s = cam.s;
    const f = this.free();
    const top = f.y + AXIS;
    const pps = this.pps;
    const labelW = this.labelW;
    const X = (ms: number) => ((ms - now) / 1000) * pps;
    const wl = (labelW - cam.x) / s;
    const wr = (W - cam.x) / s;
    const vis = (l: Lane) => {
      const y = l.y * s + cam.y;
      return y + LANE * s > top && y < H;
    };
    cx.fillStyle = C.void;
    cx.fillRect(0, 0, W, H);

    const step = STEPS.find((st) => st * pps * s >= 80) ?? 7200;
    cx.lineWidth = 1;
    cx.strokeStyle = "rgba(124,134,152,0.10)";
    for (let a = step; ; a += step) {
      const x = cam.x - a * pps * s;
      if (x < labelW) break;
      if (x > W) continue;
      cx.beginPath();
      cx.moveTo(Math.round(x) + 0.5, top);
      cx.lineTo(Math.round(x) + 0.5, H);
      cx.stroke();
    }

    this.world();
    const by = (LANE - BAR) / 2 - 1;
    for (const l of this.lanes) {
      const p = this.panes.get(l.key);
      if (!p || !vis(l)) continue;
      const y = l.y;
      const stale = p.stale;
      const k = KINDS[p.kind];
      const kc = (a: number) => (stale ? `rgba(90,98,112,${a * 0.6})` : `rgba(${k},${a})`);
      if (stale) {
        cx.fillStyle = "rgba(60,66,80,0.18)";
        cx.fillRect(wl, y - 1, wr - wl, LANE);
      }
      cx.fillStyle = "rgba(124,134,152,0.06)";
      cx.fillRect(wl, y + LANE - 1.5, wr - wl, 0.6 / s);
      // Finished commands.
      for (const r of this.runs.get(l.key)?.values() ?? []) {
        const x0 = X(r.t0);
        const x1 = X(r.t1);
        if (x1 < wl || x0 > wr) continue;
        const w = Math.max(1.2 / s, x1 - x0);
        cx.fillStyle = kc(0.3);
        cx.fillRect(x0, y + by, w, BAR);
        if (!stale && r.out > 0) {
          cx.fillStyle = this.pats.get(p.kind)![r.out > 0.75 ? 2 : r.out > 0.45 ? 1 : 0];
          cx.fillRect(x0, y + by, w, BAR);
        }
        if (r.exit) {
          cx.fillStyle = C.fail;
          cx.fillRect(x0 + w - 2 / s, y + by - 1, 2 / s, BAR + 2);
        }
      }
      // The running command.
      if (p.started != null) {
        const long = UNTIL_STOPPED.has(p.kind);
        const held = !!p.att?.since && p.att.since > p.started;
        const x0 = Math.max(wl, X(p.started));
        const x1 = X(held ? p.att!.since! : now);
        const bh = long ? 3 : BAR;
        const yy = y + (LANE - bh) / 2 - 1;
        if (x1 > wl) {
          cx.fillStyle = kc(long ? 0.35 : 0.45);
          cx.fillRect(x0, yy, Math.max(1 / s, x1 - x0), bh);
          // Its last four minutes of output, a bucket per ~3 screen px.
          const series = this.live.get(l.key);
          if (series && !stale) {
            const per = Math.max(1, Math.ceil(3 / (pps * s)));
            const n = series.length;
            for (let j = n; j > 0; j -= per) {
              let m = 0;
              for (let i = Math.max(0, j - per); i < j; i++) m = Math.max(m, series[i]);
              if (!m) continue;
              const xa = X(now - (n - j + per) * 1000);
              const xb = X(now - (n - j) * 1000);
              if (xb < x0 || xa > x1) continue;
              const r = rateOf(m);
              cx.fillStyle = `rgba(${k},${0.25 + 0.65 * r})`;
              const a = Math.max(xa, x0);
              cx.fillRect(a, yy + (long ? 0 : bh * (1 - r) * 0.5), Math.min(xb, x1) - a, long ? bh : bh * (0.4 + 0.6 * r));
            }
          }
          if (!long && !held && !stale) {
            cx.fillStyle = kc(1);
            cx.fillRect(x1 - 1.6 / s, y + by - 1.5, 1.6 / s, BAR + 3);
          }
        }
      }
      // Needs you: a band from when it started wanting you until now.
      if (p.att?.since) {
        const col = p.att.col.join(",");
        const done = col === DONE;
        const x0 = X(p.att.since);
        const pulse = reduce || done ? 1 : 0.85 + 0.15 * Math.sin(now / 380 + l.y);
        const hh = done ? 3 : LANE + 1;
        const yy = y + (LANE - hh) / 2 - 1;
        const a = Math.max(wl, x0);
        cx.fillStyle = `rgba(${col},${(done ? 0.5 : 0.8) * pulse})`;
        cx.fillRect(a, yy, -a, hh);
        if (x0 >= wl) {
          cx.fillStyle = `rgb(${col})`;
          cx.beginPath();
          cx.moveTo(x0, yy - 2);
          cx.lineTo(x0 + 5 / s, yy + hh / 2);
          cx.lineTo(x0, yy + hh + 2);
          cx.closePath();
          cx.fill();
        }
      }
    }
    this.screen();

    // Now.
    cx.fillStyle = "rgba(230,235,244,0.55)";
    cx.fillRect(Math.round(cam.x), top, 1, H - top);

    // Past now: how long each has waited, and who has it open.
    cx.textBaseline = "middle";
    for (const l of this.lanes) {
      const p = this.panes.get(l.key);
      if (!p || !vis(l)) continue;
      const y = (l.y + LANE / 2 - 1) * s + cam.y;
      if (p.att?.since && p.att.col.join(",") !== DONE) {
        cx.font = FONT.mono(700, 10.5);
        const txt = `waits ${fmtDur((now - p.att.since) / 1000)}`;
        const w = cx.measureText(txt).width + 10;
        cx.fillStyle = `rgb(${p.att.col})`;
        cx.fillRect(cam.x + 6, y - 7, w, 14);
        cx.fillStyle = C.void;
        cx.fillText(txt, cam.x + 11, y + 0.5);
      }
      if (p.unread) {
        const x = cam.x - 6 - (p.people?.length ?? 0) * 7 - 2;
        cx.fillStyle = p.mention ? UNREAD_MENTION : UNREAD;
        cx.fillRect(x - 3, y - 3, 6, 6);
      }
      (p.people ?? []).forEach((m, i) => {
        const x = cam.x - 6 - i * 7;
        cx.fillStyle = "#80cdc8";
        cx.beginPath();
        cx.arc(x, y, 2.6, 0, 7);
        cx.fill();
        if (m.driving) {
          cx.globalAlpha = m.typing ? 1 : 0.4;
          cx.strokeStyle = "#80cdc8";
          cx.lineWidth = 1;
          cx.beginPath();
          cx.arc(x, y, 4.6, 0, 7);
          cx.stroke();
          cx.globalAlpha = 1;
        }
      });
    }

    // The label column.
    cx.fillStyle = C.void;
    cx.fillRect(0, top, labelW, H - top);
    cx.fillStyle = C.line;
    cx.fillRect(labelW - 1, top, 1, H - top);
    const fs = Math.max(7, Math.min(11, LANE * s * 0.85));
    if (LANE * s >= 7) {
      for (const l of this.lanes) {
        const p = this.panes.get(l.key);
        if (!p || !vis(l)) continue;
        const y = (l.y + LANE / 2 - 1) * s + cam.y;
        if (y < top + 4) continue;
        const need = needs(p);
        const [id, ...rest] = p.label.split(" ");
        cx.font = FONT.mono(need ? 700 : 500, fs);
        cx.fillStyle = p.stale ? "#4a5263" : need ? `rgb(${p.att!.col})` : `rgb(${KINDS[p.kind]})`;
        cx.fillText(id, 10, y);
        if (labelW > 100) {
          let c = rest.join(" ");
          if (c.length > 17) c = c.slice(0, 16) + "…";
          cx.font = FONT.mono(400, fs);
          cx.fillStyle = p.stale ? "#3a4150" : C.mute;
          cx.fillText(c, 52, y);
        }
      }
    }

    // Cluster names, kept above their lanes.
    for (const h of this.heads) {
      const y = h.y * s + cam.y;
      if (y + HEAD * s < top || y > H) continue;
      const yy = Math.max(top, y);
      cx.fillStyle = C.void;
      cx.fillRect(0, yy, W, Math.min(HEAD * s, Math.max(0, y + HEAD * s - yy)) - 2);
      if (y < top - 2) continue;
      const ps = h.keys.map((k) => this.panes.get(k)!);
      const busy = ps.filter((p) => p.started != null && !UNTIL_STOPPED.has(p.kind) && !p.stale).length;
      const need = ps.filter(needs).length;
      cx.textBaseline = "alphabetic";
      cx.font = FONT.display(800, 19);
      cx.fillStyle = need ? "#ffd5da" : C.ink;
      const name = h.name.toUpperCase();
      cx.fillText(name, 10, y + HEAD * s - 9);
      const nx = 10 + cx.measureText(name).width + 12;
      cx.font = FONT.mono(500, 11);
      cx.fillStyle = C.mute;
      const meta = `${ps.length} pane${ps.length === 1 ? "" : "s"} · ${busy} running`;
      cx.fillText(meta, nx, y + HEAD * s - 10);
      if (need) {
        cx.fillStyle = C.fail;
        cx.fillText(` · ${need} need you`, nx + cx.measureText(meta).width, y + HEAD * s - 10);
      }
      cx.textBaseline = "middle";
    }

    // The time axis.
    cx.fillStyle = C.void;
    cx.fillRect(0, f.y, W, AXIS);
    cx.fillStyle = C.line;
    cx.fillRect(0, top - 1, W, 1);
    cx.font = FONT.mono(500, 11);
    cx.textAlign = "center";
    for (let a = 0; ; a += step) {
      const x = cam.x - a * pps * s;
      if (x < labelW + 16) break;
      if (x > f.x + f.w - 10) continue;
      cx.fillStyle = a === 0 ? C.ink : C.mute;
      cx.fillText(fmtAgo(a), x, f.y + AXIS / 2);
      cx.fillRect(Math.round(x), top - 5, 1, 4);
    }
    cx.textAlign = "start";
    cx.fillStyle = C.mute;
    cx.fillText(labelW > 100 ? "pane · command" : "pane", 10, f.y + AXIS / 2);
    cx.textBaseline = "alphabetic";
  }
}
