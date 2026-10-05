// The swarm as a hive (M42): one hex cell per pane, packed into a comb per
// cluster. It reads flat, so it holds up on a phone. Every channel is one
// thing a pane reports:
//
// - a comb is a cluster; its cells spiral out from the middle by row
//   (machine) and pane number, so a pane keeps its cell until a regroup;
// - the fill is how long its command has run (the city's log scale, full
//   at an hour), bright while running, faded once done; what runs until
//   stopped is full and hatched;
// - the edge is output: pulsing while it prints (faster for more bytes),
//   faint for a while after, dark when quiet;
// - a red rim: its last command failed;
// - needs you: the cell fills with the reason's colour, says how long it
//   has waited, and its glow spills onto its neighbours, wider the longer
//   it waits;
// - a ring: a teammate has it open (dashed and turning while they type);
//   grey: its machine isn't connected;
// - a dot on its top-right edge: its thread has messages you haven't read
//   (yellow: one mentions you), with how many when zoomed in (M61).

import { KINDS, REASON_COL } from "./model";
import { UNREAD, UNREAD_MENTION, type FieldPane } from "./field";
import { C, Flat, FONT, fmtDur, needs, rateOf, reduce, runtimeOf, UNTIL_STOPPED, type Rect } from "./flat";

const R = 16;
const SQ3 = Math.sqrt(3);
const GAP = 46;
const LABEL = 46;
const DIRS = [[1, 0], [1, -1], [0, -1], [-1, 0], [-1, 1], [0, 1]];
const DONE = REASON_COL.done.join(",");

/** Axial hex coordinates spiralling out from the middle. */
function spiral(n: number): [number, number][] {
  const out: [number, number][] = [[0, 0]];
  for (let k = 1; out.length < n; k++) {
    let q = DIRS[4][0] * k;
    let r = DIRS[4][1] * k;
    for (let d = 0; d < 6 && out.length < n; d++)
      for (let s = 0; s < k && out.length < n; s++) {
        out.push([q, r]);
        q += DIRS[d][0];
        r += DIRS[d][1];
      }
  }
  return out;
}

/** Full at an hour, on the city's log scale: a 10 s test and a 40 minute
 * agent turn both read. */
export function fillFor(sec: number): number {
  return Math.min(1, Math.log2(1 + Math.max(0, sec) / 8) / Math.log2(1 + 3600 / 8));
}

/** How far a waiting cell's glow reaches, in cell radii. */
export function spillFor(sec: number): number {
  return Math.min(7, 1.3 + 1.15 * Math.log2(1 + Math.max(0, sec) / 20));
}

function seedOf(key: string): number {
  let h = 2166136261;
  for (let i = 0; i < key.length; i++) h = Math.imul(h ^ key.charCodeAt(i), 16777619);
  return ((h >>> 0) % 1000) / 1000;
}

interface Comb {
  name: string;
  keys: string[];
  pos: [number, number][];
  mid: number;
  y0: number;
  w: number;
  h: number;
  x: number;
  y: number;
}

function hex(cx: CanvasRenderingContext2D, x: number, y: number, r: number) {
  cx.beginPath();
  for (let k = 0; k < 6; k++) {
    const a = Math.PI / 6 + (k * Math.PI) / 3;
    const px = x + r * Math.cos(a);
    const py = y + r * Math.sin(a);
    if (k) cx.lineTo(px, py);
    else cx.moveTo(px, py);
  }
  cx.closePath();
}

export class Hive extends Flat {
  private combs: Comb[] = [];
  private cells = new Map<string, { x: number; y: number; seed: number }>();
  private size = { w: 1, h: 1 };

  get clusters() {
    return this.combs.map((c) => ({ name: c.name, n: c.keys.length, need: this.now(c).filter(needs).length }));
  }

  /** A comb's panes as they are now (layout keeps only their keys). */
  private now(c: Comb): FieldPane[] {
    return c.keys.map((k) => this.panes.get(k)).filter((p): p is FieldPane => !!p);
  }

  /** A cell's place in the world (tests check a regroup moves it). */
  cellOf(key: string) {
    const c = this.cells.get(key);
    return c ? { x: c.x, y: c.y } : null;
  }

  protected layout() {
    this.combs = this.grouped().map((g) => {
      const pos = spiral(g.ps.length).map(([q, r]): [number, number] => [R * SQ3 * (q + r / 2), R * 1.5 * r]);
      let x0 = 1e9;
      let x1 = -1e9;
      let y0 = 1e9;
      let y1 = -1e9;
      for (const [x, y] of pos) {
        x0 = Math.min(x0, x);
        x1 = Math.max(x1, x);
        y0 = Math.min(y0, y);
        y1 = Math.max(y1, y);
      }
      return { name: g.name, keys: g.ps.map((p) => p.key), pos, mid: (x0 + x1) / 2, y0, w: Math.max(x1 - x0 + R * SQ3, 130), h: y1 - y0 + R * 2 + LABEL, x: 0, y: 0 };
    });
    // Combs in rows; try row widths and keep the one that draws biggest in
    // the free part of the screen (wide on a laptop, tall on a phone).
    const pack = (maxW: number) => {
      let x = 0;
      let y = 0;
      let rowH = 0;
      let W = 0;
      for (const c of this.combs) {
        if (x > 0 && x + c.w > maxW) {
          x = 0;
          y += rowH + GAP;
          rowH = 0;
        }
        c.x = x;
        c.y = y;
        x += c.w + GAP;
        rowH = Math.max(rowH, c.h);
        W = Math.max(W, x - GAP);
      }
      return { w: W, h: y + rowH };
    };
    const f = this.free();
    const widest = Math.max(130, ...this.combs.map((c) => c.w));
    const sum = this.combs.reduce((s, c) => s + c.w + GAP, 0);
    let best = { mw: widest, sc: -1 };
    for (let mw = widest; mw <= sum; mw += 10) {
      const r = pack(mw);
      const sc = Math.min(f.w / r.w, f.h / r.h);
      if (sc > best.sc + 1e-6) best = { mw, sc };
    }
    this.size = pack(best.mw);
    this.cells.clear();
    for (const c of this.combs) {
      const oy = c.y + LABEL + R - c.y0;
      const ox = c.x + c.w / 2 - c.mid;
      c.keys.forEach((key, k) => this.cells.set(key, { x: ox + c.pos[k][0], y: oy + c.pos[k][1], seed: seedOf(key) }));
    }
  }

  protected bounds(): Rect {
    return { x: 0, y: 0, w: this.size.w, h: this.size.h };
  }

  protected posOf(key: string) {
    return this.cellOf(key);
  }

  protected paneAt(sx: number, sy: number): FieldPane | null {
    const w = this.toWorld(sx, sy);
    for (const [key, c] of this.cells) if (Math.hypot(c.x - w.x, c.y - w.y) < R * 0.93) return this.panes.get(key) ?? null;
    return null;
  }

  protected draw(now: number) {
    const { cx, W, H } = this;
    const s = this.cam.s;
    const sec = now / 1000;
    cx.fillStyle = C.void;
    cx.fillRect(0, 0, W, H);
    this.world();

    // Comb names, a steady size on the screen.
    cx.textBaseline = "alphabetic";
    for (const c of this.combs) {
      const ps = this.now(c);
      const busy = ps.filter((p) => p.started != null && !UNTIL_STOPPED.has(p.kind) && !p.stale).length;
      const need = ps.filter(needs).length;
      cx.font = FONT.display(800, Math.max(26, 15 / s));
      cx.fillStyle = need ? "#ffd5da" : C.ink;
      cx.fillText(c.name.toUpperCase(), c.x, c.y + 24);
      if (s < 0.6) continue;
      cx.font = FONT.mono(500, 11);
      const meta = `${ps.length} pane${ps.length === 1 ? "" : "s"} · ${busy} running`;
      const needT = need ? ` · ${need} need you` : "";
      const short = cx.measureText(meta + needT).width > c.w + GAP * 0.6;
      const m = short ? `${ps.length} panes` : meta;
      cx.fillStyle = C.mute;
      cx.fillText(m, c.x, c.y + 40);
      if (need) {
        cx.fillStyle = C.fail;
        cx.fillText(needT, c.x + cx.measureText(m).width, c.y + 40);
      }
    }

    // Cells.
    for (const [key, c] of this.cells) {
      const p = this.panes.get(key);
      if (!p) continue;
      const k = KINDS[p.kind];
      const kc = (a: number) => (p.stale ? `rgba(70,76,88,${a})` : `rgba(${k[0]},${k[1]},${k[2]},${a})`);
      hex(cx, c.x, c.y, R - 1.2);
      cx.fillStyle = p.stale ? "#07090d" : "#0c1019";
      cx.fill();
      const long = UNTIL_STOPPED.has(p.kind);
      const running = p.started != null && !p.stale;
      const held = !!p.att?.since && p.started != null && p.att.since > p.started;
      const rt = runtimeOf(p, now);
      const frac = long ? 1 : rt != null ? fillFor(rt) : 0;
      if (frac > 0) {
        cx.save();
        hex(cx, c.x, c.y, R - 1.2);
        cx.clip();
        const top = c.y + R - frac * 2 * R;
        cx.fillStyle = kc(p.stale ? 0.25 : long ? 0.32 : running ? 0.78 : 0.22);
        cx.fillRect(c.x - R, top, 2 * R, c.y + R - top);
        if (long && !p.stale) {
          cx.strokeStyle = kc(0.35);
          cx.lineWidth = 1.2;
          cx.beginPath();
          for (let d = -2 * R; d < 2 * R; d += 5) {
            cx.moveTo(c.x + d, c.y + R);
            cx.lineTo(c.x + d + 2 * R, c.y - R);
          }
          cx.stroke();
        } else if (running && !held) {
          // The growing edge.
          cx.fillStyle = kc(1);
          cx.fillRect(c.x - R, top, 2 * R, 1.6);
        }
        cx.restore();
      }
      // The edge: output.
      const rate = p.stale ? 0 : rateOf(p.bps);
      const glow = p.stale || !p.lastOut ? 0 : Math.exp(-(now - p.lastOut) / 75_000);
      let ea = 0.16 + glow * 0.3;
      let ew = 1;
      if (rate > 0) {
        const pulse = reduce ? 1 : 0.6 + 0.4 * Math.sin(sec * (4 + rate * 14) + c.seed * 50);
        ea = 0.45 + 0.55 * rate * pulse;
        ew = 1.2 + rate * 1.8;
      }
      hex(cx, c.x, c.y, R - 1.2);
      cx.lineWidth = ew;
      cx.strokeStyle = kc(ea);
      cx.stroke();
      if (p.started == null && p.lastExit && !p.att) {
        hex(cx, c.x, c.y, R - 2.6);
        cx.lineWidth = 2.2;
        cx.strokeStyle = C.fail;
        cx.stroke();
      }
    }

    // Needs you: the glow spills onto the neighbours (added, so it tints them).
    cx.globalCompositeOperation = "lighter";
    for (const [key, c] of this.cells) {
      const p = this.panes.get(key);
      if (!p?.att) continue;
      const col = p.att.col.join(",");
      const done = col === DONE;
      const wait = p.att.since ? (now - p.att.since) / 1000 : 0;
      const pulse = reduce ? 1 : 0.85 + 0.15 * Math.sin(sec * 2.4 + c.seed * 40);
      const rad = R * spillFor(wait);
      const a = (done ? 0.22 : 0.42) * pulse;
      const gr = cx.createRadialGradient(c.x, c.y, R * 0.6, c.x, c.y, rad);
      gr.addColorStop(0, `rgba(${col},${a})`);
      gr.addColorStop(1, `rgba(${col},0)`);
      cx.fillStyle = gr;
      cx.beginPath();
      cx.arc(c.x, c.y, rad, 0, Math.PI * 2);
      cx.fill();
    }
    cx.globalCompositeOperation = "source-over";
    cx.textAlign = "center";
    cx.textBaseline = "middle";
    for (const [key, c] of this.cells) {
      const p = this.panes.get(key);
      if (!p?.att) continue;
      const col = p.att.col.join(",");
      hex(cx, c.x, c.y, R - 1.2);
      cx.fillStyle = `rgba(${col},${col === DONE ? 0.5 : 0.92})`;
      cx.fill();
      if (p.att.since) {
        const wait = (now - p.att.since) / 1000;
        cx.font = FONT.mono(700, 9);
        cx.fillStyle = C.void;
        // "21m", "1h04m", "35s": short enough for a cell.
        cx.fillText(wait < 60 ? fmtDur(wait) : fmtDur(wait).replace(/\d+s$/, ""), c.x, c.y + 0.5);
      }
    }
    cx.textAlign = "start";

    // M61: unread threads.
    for (const [key, c] of this.cells) {
      const p = this.panes.get(key);
      if (!p?.unread) continue;
      const x = c.x + R * 0.75;
      const y = c.y - R * 0.75;
      cx.fillStyle = p.mention ? UNREAD_MENTION : UNREAD;
      cx.beginPath();
      cx.arc(x, y, s < 0.7 ? 3.5 : 6, 0, 7);
      cx.fill();
      if (s >= 0.7) {
        cx.font = FONT.mono(700, 8);
        cx.textAlign = "center";
        cx.fillStyle = C.void;
        cx.fillText(p.unread > 9 ? "9+" : String(p.unread), x, y + 0.5);
        cx.textAlign = "start";
      }
    }

    // Teammates with it open.
    const tagged = new Set<string>();
    for (const [key, c] of this.cells) {
      const p = this.panes.get(key);
      if (!p?.people?.length) continue;
      p.people.forEach((m, i) => {
        cx.save();
        hex(cx, c.x, c.y, R + 4.5 + i * 3.5);
        cx.lineWidth = 2;
        cx.strokeStyle = "#80cdc8";
        if (m.typing) {
          cx.setLineDash([5, 4]);
          cx.lineDashOffset = reduce ? 0 : -sec * 12;
        }
        cx.stroke();
        cx.restore();
      });
      if (s < 0.7 || tagged.size > 12) continue;
      tagged.add(key);
      const m = p.people[0];
      cx.font = FONT.mono(600, 10);
      const label = `${m.name} · ${m.typing ? "typing" : m.driving ? "driving" : "watching"}${p.people.length > 1 ? ` +${p.people.length - 1}` : ""}`;
      const tw = cx.measureText(label).width;
      cx.fillStyle = "rgba(12,16,25,0.9)";
      cx.fillRect(c.x + R + 2, c.y - R - 12, tw + 10, 15);
      cx.fillStyle = "#80cdc8";
      cx.fillText(label, c.x + R + 7, c.y - R - 4.5);
    }
    cx.textBaseline = "alphabetic";
    this.screen();
  }

}
