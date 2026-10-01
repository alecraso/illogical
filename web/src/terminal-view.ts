// The one place that knows about xterm.js, so the renderer can be swapped
// (ghostty-web) without touching the rest of the client.

import { Terminal } from "@xterm/xterm";
import { Unicode11Addon } from "@xterm/addon-unicode11";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { WebglAddon } from "@xterm/addon-webgl";
import "@xterm/xterm/css/xterm.css";
import { theme } from "./theme";

export const FONT_FAMILY = '"JetBrains Mono", "Fira Code", ui-monospace, Menlo, monospace';
export const FONT_SIZE = 14;

/** Touch-first devices draw with the DOM renderer: xterm's WebGL renderer
 * drew nothing at all in Chrome's phone emulation (fractional pixel ratio),
 * and a phone shows one pane at a time, so the speed isn't needed. */
const WEBGL = !matchMedia("(pointer: coarse)").matches;

export class TerminalView {
  readonly host: HTMLDivElement;
  private term: Terminal;
  private webgl: WebglAddon | undefined;

  constructor() {
    this.host = document.createElement("div");
    this.host.className = "term-host";
    this.term = new Terminal({
      theme,
      fontFamily: FONT_FAMILY,
      fontSize: FONT_SIZE,
      cursorBlink: true,
      scrollback: 10000,
      allowProposedApi: true,
      macOptionIsMeta: true,
    });
    this.term.loadAddon(new Unicode11Addon());
    this.term.unicode.activeVersion = "11";
    this.term.loadAddon(new WebLinksAddon());
    this.swallowQueries();
    this.term.attachCustomKeyEventHandler((e) => this.clipboardKeys(e));
    this.term.open(this.host);
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

  /** Ctrl+Shift+C copies the selection; Ctrl+Shift+V is left to the
   * browser's paste event, which xterm.js handles. */
  private clipboardKeys(e: KeyboardEvent): boolean {
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

  /** Clear everything (screen, scrollback, modes) before a snapshot. */
  reset() {
    this.term.reset();
  }

  focus() {
    this.term.focus();
  }

  onInput(cb: (data: Uint8Array) => void) {
    const enc = new TextEncoder();
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
