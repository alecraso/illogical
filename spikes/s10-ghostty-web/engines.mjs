// One interface over the terminal engines under test, configured the way
// web/src/terminal-view.ts configures xterm.js.
//   xterm        @xterm/xterm 6.0.0 (+ unicode11, + webgl unless touch-first)
//   gw           ghostty-web 0.4.0 (npm latest, 2025-12-09)
//   gw-next      ghostty-web 0.4.0-next.20.g1858a59 (npm next, 2026-06-28)
import { Terminal as XTerm } from "/nm/@xterm/xterm/lib/xterm.mjs";
import { Unicode11Addon } from "/nm/@xterm/addon-unicode11/lib/addon-unicode11.mjs";
import { WebglAddon } from "/nm/@xterm/addon-webgl/lib/addon-webgl.mjs";

export const theme = {
  foreground: "#cdd6f4", background: "#1e1e2e", cursor: "#f5e0dc", cursorAccent: "#1e1e2e",
  selectionBackground: "#585b7080",
  black: "#45475a", red: "#f38ba8", green: "#a6e3a1", yellow: "#f9e2af", blue: "#89b4fa",
  magenta: "#f5c2e7", cyan: "#94e2d5", white: "#bac2de", brightBlack: "#585b70", brightRed: "#f38ba8",
  brightGreen: "#a6e3a1", brightYellow: "#f9e2af", brightBlue: "#89b4fa", brightMagenta: "#f5c2e7",
  brightCyan: "#94e2d5", brightWhite: "#a6adc8",
};
const FONT_FAMILY = '"JetBrains Mono", "Fira Code", ui-monospace, Menlo, monospace';
const FONT_SIZE = 14;
const WEBGL = !matchMedia("(pointer: coarse)").matches;

const gwModules = {};
async function ghosttyWeb(which) {
  if (!gwModules[which]) {
    const m = await import(which === "gw" ? "/nm/ghostty-web/dist/ghostty-web.js" : "/nm/ghostty-web-next/dist/ghostty-web.js");
    await m.init();
    gwModules[which] = m;
  }
  return gwModules[which];
}

const hex = (n) => "#" + n.toString(16).padStart(6, "0");

/** A terminal in `host`. `scrollback` is passed through as-is: lines for
 * xterm.js, and (ghostty-web issue #140) bytes for ghostty-web. */
export async function create(engine, host, { cols, rows, scrollback = 10000, webgl = WEBGL }) {
  if (engine === "xterm") {
    const term = new XTerm({ theme, fontFamily: FONT_FAMILY, fontSize: FONT_SIZE, cursorBlink: false, scrollback,
      allowProposedApi: true, cols, rows });
    term.loadAddon(new Unicode11Addon());
    term.unicode.activeVersion = "11";
    term.open(host);
    if (webgl) {
      try { term.loadAddon(new WebglAddon()); } catch { /* DOM renderer */ }
    }
    return new XtermEngine(term);
  }
  const m = await ghosttyWeb(engine);
  const term = new m.Terminal({ theme, fontFamily: FONT_FAMILY, fontSize: FONT_SIZE, cursorBlink: false, scrollback, cols, rows });
  term.open(host);
  return new GhosttyWebEngine(term);
}

class XtermEngine {
  constructor(t) { this.t = t; }
  write(bytes) { return new Promise((r) => this.t.write(bytes, r)); }
  resize(c, r) { this.t.resize(c, r); }
  /** Resolves on the next render after now (the screen as it is). */
  painted() {
    return new Promise((r) => { const d = this.t.onRender(() => { d.dispose(); r(); }); this.t.refresh(0, this.t.rows - 1); });
  }
  get alt() { return this.t.buffer.active.type === "alternate"; }
  get cursor() { const b = this.t.buffer.active; return [b.cursorX, b.cursorY]; }
  text() {
    const b = this.t.buffer.active, out = [];
    for (let i = 0; i < b.length; i++) out.push(b.getLine(i).translateToString(true));
    return out.join("\n");
  }
  get scrollbackRows() { return this.t.buffer.active.length - this.t.rows; }
  /** Visible screen as [text, width, bold, italic, faint, underline, inverse, strike, fg, bg];
   * colors: null (default), palette index, or "#rrggbb". */
  cells() {
    const b = this.t.buffer.active, rows = [];
    const col = (mode, v) => (mode === 0 ? null : mode === 50331648 ? hex(v) : v);
    for (let y = 0; y < this.t.rows; y++) {
      const line = b.getLine(b.viewportY + y), row = [];
      for (let x = 0; x < this.t.cols; x++) {
        const c = line.getCell(x);
        row.push([c.getChars(), c.getWidth(), !!c.isBold(), !!c.isItalic(), !!c.isDim(), !!c.isUnderline(),
          !!c.isInverse(), !!c.isStrikethrough(), col(c.getFgColorMode(), c.getFgColor()), col(c.getBgColorMode(), c.getBgColor())]);
      }
      rows.push(row);
    }
    return rows;
  }
  dispose() { this.t.dispose(); }
}

class GhosttyWebEngine {
  constructor(t) { this.t = t; }
  write(bytes) {
    // Issue #199: an empty write throws.
    if (!bytes.length) return Promise.resolve();
    return new Promise((r) => this.t.write(bytes, r));
  }
  resize(c, r) { this.t.resize(c, r); }
  painted() {
    // Its render loop draws every animation frame; two frames means the
    // current state has been drawn at least once.
    return new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
  }
  get alt() { return this.t.wasmTerm.isAlternateScreen(); }
  get cursor() { const b = this.t.buffer.active; return [b.cursorX, b.cursorY]; }
  get scrollbackRows() { return this.t.wasmTerm.getScrollbackLength(); }
  /** Through the xterm.js-compatible buffer API, as TerminalView.text() would. */
  text() {
    const b = this.t.buffer.active, out = [];
    for (let i = 0; i < b.length; i++) out.push(b.getLine(i).translateToString(true));
    return out.join("\n");
  }
  /** The same, but with full grapheme clusters (getChars() returns only the
   * first codepoint of a cell). */
  gtext() {
    const w = this.t.wasmTerm, sb = this.alt ? 0 : w.getScrollbackLength(), out = [];
    const n = sb + w.rows;
    for (let i = 0; i < n; i++) {
      const cells = i < sb ? w.getScrollbackLine(i) : w.getLine(i - sb);
      let s = "";
      for (let x = 0; x < cells.length; x++) {
        const c = cells[x];
        if (c.codepoint === 0) { if (c.width !== 0) s += " "; continue; }
        s += c.grapheme_len > 0 ? (i < sb ? w.getScrollbackGraphemeString(i, x) : w.getGraphemeString(i - sb, x)) : String.fromCodePoint(c.codepoint);
      }
      out.push(s.trimEnd());
    }
    return out.join("\n");
  }
  /** Visible screen; colors are resolved RGB (the only thing it exposes). */
  cells() {
    const w = this.t.wasmTerm, rows = [];
    for (let y = 0; y < w.rows; y++) {
      const cells = w.getLine(y), row = [];
      for (let x = 0; x < cells.length; x++) {
        const c = cells[x], f = c.flags;
        const text = c.codepoint === 0 ? "" : c.grapheme_len > 0 ? w.getGraphemeString(y, x) : String.fromCodePoint(c.codepoint);
        row.push([text, c.width, !!(f & 1), !!(f & 2), !!(f & 128), !!(f & 4), !!(f & 16), !!(f & 8),
          hex((c.fg_r << 16) | (c.fg_g << 8) | c.fg_b), hex((c.bg_r << 16) | (c.bg_g << 8) | c.bg_b)]);
      }
      rows.push(row);
    }
    return rows;
  }
  dispose() { this.t.dispose(); }
}
