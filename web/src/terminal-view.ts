// The one place that knows about xterm.js, so the renderer can be swapped
// (ghostty-web) without touching the rest of the client.

import { Terminal, type IDecoration, type IMarker } from "@xterm/xterm";
import { Unicode11Addon } from "@xterm/addon-unicode11";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { WebglAddon } from "@xterm/addon-webgl";
import "@xterm/xterm/css/xterm.css";
import { theme } from "./theme";
import type { Chip } from "./upload";

export const FONT_FAMILY = '"JetBrains Mono", "Fira Code", ui-monospace, Menlo, monospace';
export const FONT_SIZE = 14;
/** Rows of scrollback a pane keeps; snapshots bring no more than this. */
export const SCROLLBACK = 10000;
const enc = new TextEncoder();

/** Touch-first devices draw with the DOM renderer: xterm's WebGL renderer
 * drew nothing at all in Chrome's phone emulation (fractional pixel ratio),
 * and a phone shows one pane at a time, so the speed isn't needed. */
const WEBGL = !matchMedia("(pointer: coarse)").matches;

/** A finished command, from the shell integration's OSC 133/633 marks. */
export interface CommandMark {
  text: string;
  exit: number | null;
  /** First line of its output. */
  start: IMarker;
  /** The line after its output (the next prompt). */
  end: IMarker;
  deco?: IDecoration;
}

/** VS Code's OSC 633;E escaping: `\\` and `\xHH`. */
function unescape633(s: string): string {
  return s.replace(/\\(\\|x([0-9a-fA-F]{2}))/g, (_, all: string, hex?: string) =>
    hex ? String.fromCharCode(parseInt(hex, 16)) : all,
  );
}

export class TerminalView {
  readonly host: HTMLDivElement;
  private term: Terminal;
  private webgl: WebglAddon | undefined;
  readonly marks: CommandMark[] = [];
  private pendingText = "";
  private pending: { text: string; start: IMarker } | null = null;
  private markMenu: ((mark: CommandMark, e: MouseEvent) => void) | undefined;

  /** `link`: a link clicked in the terminal, before the browser opens it;
   * true if it was handled (M36: a pull request opens as a block). */
  constructor(link?: (uri: string, e: MouseEvent) => boolean) {
    this.host = document.createElement("div");
    this.host.className = "term-host";
    this.term = new Terminal({
      theme,
      fontFamily: FONT_FAMILY,
      fontSize: FONT_SIZE,
      cursorBlink: true,
      scrollback: SCROLLBACK,
      allowProposedApi: true,
      macOptionIsMeta: true,
      overviewRuler: { width: 8 },
    });
    this.term.loadAddon(new Unicode11Addon());
    this.term.unicode.activeVersion = "11";
    this.term.loadAddon(
      new WebLinksAddon((e, uri) => {
        if (link?.(uri, e)) return;
        window.open(uri, "_blank", "noopener");
      }),
    );
    this.swallowQueries();
    this.watchCommands();
    this.term.attachCustomKeyEventHandler((e) => this.keys(e));
    this.takeFiles();
    this.term.open(this.host);
    this.touchScroll();
  }

  /** xterm.js 6 scrolls on the wheel only (its viewport is VS Code's
   * scrollable element, which ignores touch), so a swipe does nothing.
   * Turn vertical swipes into scrolling: the scrollback directly, or wheel
   * events when a program wants them (mouse reports, alt-screen arrows).
   * A tap stays a tap, so it still focuses and brings up the keyboard. */
  private touchScroll() {
    const SLOP = 8; // px before a touch counts as a swipe
    let lastY = 0;
    let startY = 0;
    let x = 0;
    let acc = 0; // px not yet scrolled, under one row
    let swiping = false;
    let velocity = 0; // px per ms, for the fling
    let lastT = 0;
    let fling = 0;

    const scrollPx = (dy: number) => {
      const row = this.cellSize()?.height;
      if (!row) return;
      acc += dy;
      const lines = Math.trunc(acc / row);
      if (!lines) return;
      acc -= lines * row;
      // Finger down shows older lines: scroll up.
      if (this.term.buffer.active.type === "normal" && !this.mouseTracking) {
        this.term.scrollLines(-lines);
        return;
      }
      const screen = this.host.querySelector(".xterm-screen");
      screen?.dispatchEvent(
        new WheelEvent("wheel", {
          deltaY: -lines,
          deltaMode: WheelEvent.DOM_DELTA_LINE,
          clientX: x,
          clientY: lastY,
          bubbles: true,
          cancelable: true,
        }),
      );
    };

    this.host.addEventListener(
      "touchstart",
      (e) => {
        cancelAnimationFrame(fling);
        const t = e.touches[0];
        startY = lastY = t.clientY;
        x = t.clientX;
        acc = 0;
        velocity = 0;
        lastT = e.timeStamp;
        swiping = false;
      },
      { passive: true },
    );
    this.host.addEventListener(
      "touchmove",
      (e) => {
        const t = e.touches[0];
        if (!swiping && Math.abs(t.clientY - startY) < SLOP) return;
        swiping = true;
        e.preventDefault();
        const dy = t.clientY - lastY;
        const dt = Math.max(1, e.timeStamp - lastT);
        velocity = 0.8 * (dy / dt) + 0.2 * velocity;
        lastY = t.clientY;
        lastT = e.timeStamp;
        scrollPx(dy);
      },
      { passive: false },
    );
    this.host.addEventListener("touchend", () => {
      if (!swiping) return;
      let prev = performance.now();
      const step = (now: number) => {
        const dt = now - prev;
        prev = now;
        velocity *= Math.pow(0.995, dt);
        if (Math.abs(velocity) < 0.05) return;
        scrollPx(velocity * dt);
        fling = requestAnimationFrame(step);
      };
      fling = requestAnimationFrame(step);
    });
  }

  /** The daemon's terminal answers queries (device attributes, cursor
   * position, colors) so programs get exactly one reply, attached or not.
   * Stop xterm.js from answering too. */
  private swallowQueries() {
    const p = this.term.parser;
    const yes = () => true;
    p.registerCsiHandler({ final: "c" }, yes); // DA1
    p.registerCsiHandler({ prefix: ">", final: "c" }, yes); // DA2
    p.registerCsiHandler({ prefix: "=", final: "c" }, yes); // DA3
    p.registerCsiHandler({ final: "n" }, yes); // DSR
    p.registerCsiHandler({ prefix: "?", final: "n" }, yes); // DEC DSR
    p.registerCsiHandler({ prefix: ">", final: "q" }, yes); // XTVERSION
    p.registerCsiHandler({ intermediates: "$", final: "p" }, yes); // DECRQM
    p.registerCsiHandler({ prefix: "?", intermediates: "$", final: "p" }, yes);
    p.registerCsiHandler({ prefix: "?", final: "u" }, yes); // kitty keyboard query
    p.registerDcsHandler({ intermediates: "$", final: "q" }, yes); // DECRQSS
    for (const osc of [4, 10, 11, 12]) {
      // Color queries contain "?"; setting colors still goes through.
      p.registerOscHandler(osc, (data) => data.includes("?"));
    }
  }

  /** Follow the shell integration: where each command's output starts and
   * ends, and how it exited. Marks live in this browser's terminal; a
   * snapshot (reconnecting, a new window) starts without them. */
  private watchCommands() {
    const p = this.term.parser;
    p.registerOscHandler(633, (data) => {
      if (data.startsWith("E;")) this.pendingText = unescape633(data.slice(2).split(";")[0]);
      return false;
    });
    p.registerOscHandler(133, (data) => {
      const [kind, arg] = data.split(";");
      if (kind === "C") {
        const start = this.term.registerMarker(0);
        if (start) this.pending = { text: this.pendingText, start };
        this.pendingText = "";
      } else if (kind === "D" && this.pending) {
        const end = this.term.registerMarker(0);
        const exit = arg !== undefined && arg !== "" ? Number(arg) : null;
        if (end) this.addMark({ ...this.pending, exit, end });
        this.pending = null;
      }
      return false;
    });
  }

  private addMark(mark: CommandMark) {
    const rows = Math.max(1, mark.end.line - mark.start.line);
    const failed = mark.exit !== null && mark.exit !== 0;
    const deco = this.term.registerDecoration({
      marker: mark.start,
      x: 0,
      width: 1,
      height: rows,
      layer: "top",
      overviewRulerOptions: { color: failed ? "#f38ba8" : "#a6e3a1", position: "left" },
    });
    if (!deco) return;
    mark.deco = deco;
    deco.onRender((el) => {
      el.classList.add("cmd-mark", failed ? "fail" : "ok");
      // A thin stripe at the left edge rather than covering the first cell.
      el.style.width = "3px";
      el.title = `${mark.text || "command"}${mark.exit !== null ? ` · exit ${mark.exit}` : ""}\nClick to select its output`;
      el.onclick = () => this.selectOutput(mark);
      el.oncontextmenu = (e) => {
        e.preventDefault();
        e.stopPropagation();
        this.markMenu?.(mark, e);
      };
    });
    mark.start.onDispose(() => {
      const i = this.marks.indexOf(mark);
      if (i >= 0) this.marks.splice(i, 1);
    });
    this.marks.push(mark);
  }

  selectOutput(mark: CommandMark) {
    if (mark.end.line > mark.start.line) this.term.selectLines(mark.start.line, mark.end.line - 1);
  }

  outputText(mark: CommandMark): string {
    const b = this.term.buffer.active;
    const lines: string[] = [];
    for (let i = mark.start.line; i < mark.end.line; i++) lines.push(b.getLine(i)?.translateToString(true) ?? "");
    return lines.join("\n");
  }

  selection(): string {
    return this.term.getSelection();
  }

  /** M61: find text quoted from this terminal (the newest place its lines
   * appear), scroll to it and select it. False when it's no longer in the
   * scrollback. */
  reveal(text: string): boolean {
    const want = text.split("\n").map((l) => l.trimEnd());
    while (want.length && !want[want.length - 1]) want.pop();
    while (want.length && !want[0]) want.shift();
    if (!want.length) return false;
    const b = this.term.buffer.active;
    const line = (i: number) => b.getLine(i)?.translateToString(true).trimEnd() ?? "";
    for (let i = b.length - want.length; i >= 0; i--) {
      if (!line(i).includes(want[0])) continue;
      if (want.every((w, k) => k === 0 || line(i + k).includes(w))) {
        this.term.scrollToLine(Math.max(0, i - 2));
        this.term.selectLines(i, i + want.length - 1);
        return true;
      }
    }
    return false;
  }

  onMarkMenu(cb: (mark: CommandMark, e: MouseEvent) => void) {
    this.markMenu = cb;
  }

  private filesCb: ((files: File[]) => void) | undefined;
  /** Files pasted or dropped on the terminal (M70). */
  onFiles(cb: (files: File[]) => void) {
    this.filesCb = cb;
  }

  /** A paste or drop with files in it goes to `onFiles`. The listeners
   * capture on the host, so they run before xterm's paste handler, which
   * reads only the text and stops the event. */
  private takeFiles() {
    const take = (e: Event, files: FileList | undefined | null) => {
      if (!files?.length || !this.filesCb) return;
      e.preventDefault();
      e.stopImmediatePropagation();
      this.filesCb([...files]);
    };
    this.host.addEventListener("paste", (e) => take(e, e.clipboardData?.files), true);
    this.host.addEventListener(
      "dragover",
      (e) => {
        if (this.filesCb && e.dataTransfer?.types.includes("Files")) e.preventDefault();
      },
      true,
    );
    this.host.addEventListener("drop", (e) => take(e, e.dataTransfer?.files), true);
  }

  private chipEl: HTMLDivElement | undefined;
  private chipTimer: number | undefined;
  /** A note over the terminal's corner, with buttons (M70: an upload's
   * progress, then what became of it). */
  readonly chip: Chip = {
    show: (text, opts = {}) => {
      clearTimeout(this.chipTimer);
      if (!this.chipEl) {
        this.chipEl = document.createElement("div");
        this.chipEl.addEventListener("pointerdown", (e) => e.stopPropagation());
        this.host.append(this.chipEl);
      }
      const el = this.chipEl;
      el.className = opts.error ? "term-chip error" : "term-chip";
      el.dataset.chip = "";
      el.replaceChildren(Object.assign(document.createElement("span"), { textContent: text }));
      for (const a of opts.actions ?? []) {
        const b = Object.assign(document.createElement("button"), { textContent: a.label });
        b.addEventListener("click", () => a.run());
        el.append(b);
      }
      if (opts.actions?.length || opts.error) {
        const x = Object.assign(document.createElement("button"), { textContent: "×", title: "Dismiss" });
        x.className = "link";
        x.addEventListener("click", () => this.chip.hide());
        el.append(x);
      }
      if (opts.hideAfterMs) this.chipTimer = window.setTimeout(() => this.chip.hide(), opts.hideAfterMs);
    },
    hide: () => {
      clearTimeout(this.chipTimer);
      this.chipEl?.remove();
      this.chipEl = undefined;
    },
  };

  /** Ctrl+Shift+C copies the selection; Ctrl+Shift+V is left to the
   * browser's paste event, which xterm.js handles. Shift+Enter sends ESC CR
   * (Alt+Enter, a new line in Claude Code and line editors, with or without
   * the kitty protocol, whose state the web can't see) where xterm.js sends
   * CR; the keypress is stopped too, or it would send CR as well. */
  private keys(e: KeyboardEvent): boolean {
    if (e.key === "Enter" && e.shiftKey && !e.ctrlKey && !e.altKey && !e.metaKey && !e.isComposing) {
      if (e.type === "keydown") this.term.input("\x1b\r", true);
      return false;
    }
    if (e.type !== "keydown" || !e.ctrlKey || !e.shiftKey) return true;
    if (e.code === "KeyC") {
      const text = this.term.getSelection();
      if (text) void navigator.clipboard?.writeText(text);
      return false;
    }
    if (e.code === "KeyV") return false;
    return true;
  }

  /** WebGL only while on screen: Chrome allows ~16 contexts per page and
   * xterm's addon leaks them on dispose, so hidden panes use the DOM
   * renderer and give their context back. */
  setVisible(visible: boolean) {
    if (visible && !this.webgl && WEBGL) {
      try {
        const webgl = new WebglAddon();
        webgl.onContextLoss(() => this.dropWebgl());
        this.term.loadAddon(webgl);
        this.webgl = webgl;
      } catch {
        // No WebGL: the DOM renderer still works.
      }
      this.term.refresh(0, this.term.rows - 1);
    } else if (!visible && this.webgl) {
      this.dropWebgl();
    }
  }

  private dropWebgl() {
    const canvas = this.host.querySelector("canvas");
    const gl = canvas?.getContext("webgl2");
    this.webgl?.dispose();
    this.webgl = undefined;
    gl?.getExtension("WEBGL_lose_context")?.loseContext();
  }

  get cols() {
    return this.term.cols;
  }
  get rows() {
    return this.term.rows;
  }

  /** Pixel size of one cell, once rendered. */
  cellSize(): { width: number; height: number } | undefined {
    const screen = this.host.querySelector<HTMLElement>(".xterm-screen");
    if (!screen || !screen.offsetWidth) return undefined;
    return { width: screen.offsetWidth / this.term.cols, height: screen.offsetHeight / this.term.rows };
  }

  /** Whether arrow keys should send application sequences (DECCKM). */
  get appCursor(): boolean {
    return this.term.modes.applicationCursorKeysMode;
  }

  /** Whether the program asked for mouse reports (then right-click and
   * drags belong to it). */
  get mouseTracking(): boolean {
    return this.term.modes.mouseTrackingMode !== "none";
  }

  resize(cols: number, rows: number) {
    if (cols > 0 && rows > 0 && (cols !== this.term.cols || rows !== this.term.rows)) this.term.resize(cols, rows);
  }

  write(data: Uint8Array, done?: () => void) {
    this.term.write(data, done);
  }

  /** Before a snapshot of the screen alone, after falling behind: keep the
   * scrollback, push the screen into it under a rule marking what was
   * skipped, and start the screen and modes over. */
  skipGap() {
    const rows = this.term.rows;
    this.write(
      enc.encode(
        // A full-screen app's screen isn't history; leave it.
        "\x1b[?1049l\x1b[0m" +
          `\x1b[${rows};1H\r\n\x1b[2m── output skipped here; illogical tail has it ──\x1b[0m` +
          "\r\n".repeat(rows) +
          // Soft reset, plus the input modes it leaves alone.
          "\x1b[!p\x1b[?7h\x1b[?1l\x1b[?66l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?1004l\x1b[?2004l" +
          "\x1b]104\x1b\\\x1b[H\x1b[2J",
      ),
    );
    this.pending = null;
  }

  /** Clear everything (screen, scrollback, modes) before a snapshot. */
  reset() {
    this.term.reset();
    for (const m of this.marks.splice(0)) m.deco?.dispose();
    this.pending = null;
  }

  focus() {
    this.term.focus();
  }

  onInput(cb: (data: Uint8Array) => void) {
    this.term.onData((s) => cb(enc.encode(s)));
    // Mouse reports in X10 encoding arrive as raw bytes in a string.
    this.term.onBinary((s) => cb(Uint8Array.from(s, (c) => c.charCodeAt(0) & 0xff)));
  }

  onTitle(cb: (title: string) => void) {
    this.term.onTitleChange(cb);
  }

  onFocus(cb: () => void) {
    this.term.textarea?.addEventListener("focus", cb);
  }

  /** Text of the active buffer, for tests and debugging. */
  text(): string {
    const b = this.term.buffer.active;
    const lines: string[] = [];
    for (let i = 0; i < b.length; i++) lines.push(b.getLine(i)?.translateToString(true) ?? "");
    return lines.join("\n");
  }

  /** The visible screen only. */
  screen(): string {
    const b = this.term.buffer.active;
    const lines: string[] = [];
    for (let i = 0; i < this.term.rows; i++) lines.push(b.getLine(b.viewportY + i)?.translateToString(true) ?? "");
    return lines.join("\n");
  }

  dispose() {
    this.dropWebgl();
    this.term.dispose();
    this.host.remove();
  }
}
