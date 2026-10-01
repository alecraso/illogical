// The one place that knows about xterm.js, so the renderer can be swapped
// (ghostty-web) without touching the rest of the client.

import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { Unicode11Addon } from "@xterm/addon-unicode11";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { WebglAddon } from "@xterm/addon-webgl";
import "@xterm/xterm/css/xterm.css";
import { theme } from "./theme";

export class TerminalView {
  private term: Terminal;
  private fit = new FitAddon();
  private webgl: WebglAddon | undefined;

  constructor(private host: HTMLElement) {
    this.term = new Terminal({
      theme,
      fontFamily: '"JetBrains Mono", "Fira Code", ui-monospace, Menlo, monospace',
      fontSize: 14,
      cursorBlink: true,
      scrollback: 10000,
      allowProposedApi: true,
      macOptionIsMeta: true,
    });
    this.term.loadAddon(this.fit);
    this.term.loadAddon(new Unicode11Addon());
    this.term.unicode.activeVersion = "11";
    this.term.loadAddon(new WebLinksAddon());
    this.swallowQueries();
    this.term.attachCustomKeyEventHandler((e) => this.clipboardKeys(e));
    this.term.open(host);
    this.enableWebgl();
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

  private enableWebgl() {
    try {
      const webgl = new WebglAddon();
      webgl.onContextLoss(() => this.disableWebgl());
      this.term.loadAddon(webgl);
      this.webgl = webgl;
    } catch {
      // No WebGL (or too many contexts): the DOM renderer still works.
    }
  }

  private disableWebgl() {
    this.webgl?.dispose();
    this.webgl = undefined;
  }

  get cols() {
    return this.term.cols;
  }
  get rows() {
    return this.term.rows;
  }

  /** The size that fills the host element, or undefined while hidden. */
  fittedSize(): { cols: number; rows: number } | undefined {
    const d = this.fit.proposeDimensions();
    if (!d || !Number.isFinite(d.cols) || !Number.isFinite(d.rows) || d.cols < 2 || d.rows < 1) return undefined;
    return { cols: d.cols, rows: d.rows };
  }

  resize(cols: number, rows: number) {
    if (cols !== this.term.cols || rows !== this.term.rows) this.term.resize(cols, rows);
    this.applyScale();
  }

  private scaleToFit = false;

  /** When drawing at another client's size, shrink to fit instead of
   * cropping (a phone watching a desktop-sized pane). */
  setScaleToFit(on: boolean) {
    this.scaleToFit = on;
    this.applyScale();
  }

  private applyScale() {
    requestAnimationFrame(() => {
      const el = this.term.element;
      if (!el) return;
      el.style.transform = "";
      if (!this.scaleToFit) return;
      const cs = getComputedStyle(this.host);
      const availW = this.host.clientWidth - parseFloat(cs.paddingLeft) - parseFloat(cs.paddingRight);
      const availH = this.host.clientHeight - parseFloat(cs.paddingTop) - parseFloat(cs.paddingBottom);
      const scale = Math.min(1, availW / el.offsetWidth, availH / el.offsetHeight);
      if (scale < 1) el.style.transform = `scale(${scale})`;
    });
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

  /** Text of the active buffer, for tests and debugging. */
  text(): string {
    const b = this.term.buffer.active;
    const lines: string[] = [];
    for (let i = 0; i < b.length; i++) lines.push(b.getLine(i)?.translateToString(true) ?? "");
    return lines.join("\n");
  }

  get element(): HTMLElement {
    return this.host;
  }
}
