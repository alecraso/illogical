// Follow mode's code view (M28, S17: CodeMirror 6, read-only, loaded only
// when someone follows). It shows the file an editor has open, applies its
// edits as they come, and draws its cursor and selection, the file's
// diagnostics and the debugger's line, in the terminal's colours. M11's file
// block draws in it too (a marked line), and its diff block highlights its
// hunks with the same languages and colours.

import { EditorSelection, EditorState, StateEffect, StateField, type Extension, type Range } from "@codemirror/state";
import { Decoration, type DecorationSet, EditorView, drawSelection, highlightActiveLine, lineNumbers } from "@codemirror/view";
import { HighlightStyle, syntaxHighlighting } from "@codemirror/language";
import { highlightCode, tagHighlighter, tags as t } from "@lezer/highlight";
import { type LanguageSupport } from "@codemirror/language";
import { javascript } from "@codemirror/lang-javascript";
import { python } from "@codemirror/lang-python";
import { rust } from "@codemirror/lang-rust";
import { theme } from "../theme";

/** A position the editor sent: line from 1, column from 0. */
export type Pos = [number, number];

export interface Diagnostic {
  range: [number, number, number, number];
  severity: string;
  message: string;
}

const setMarks = StateEffect.define<DecorationSet>();
const marks = StateField.define<DecorationSet>({
  create: () => Decoration.none,
  update(v, tr) {
    v = v.map(tr.changes);
    for (const e of tr.effects) if (e.is(setMarks)) v = e.value;
    return v;
  },
  provide: (f) => EditorView.decorations.from(f),
});
const setDebug = StateEffect.define<DecorationSet>();
const debugLine = StateField.define<DecorationSet>({
  create: () => Decoration.none,
  update(v, tr) {
    v = v.map(tr.changes);
    for (const e of tr.effects) if (e.is(setDebug)) v = e.value;
    return v;
  },
  provide: (f) => EditorView.decorations.from(f),
});

const setMark = StateEffect.define<DecorationSet>();
const markLine = StateField.define<DecorationSet>({
  create: () => Decoration.none,
  update(v, tr) {
    v = v.map(tr.changes);
    for (const e of tr.effects) if (e.is(setMark)) v = e.value;
    return v;
  },
  provide: (f) => EditorView.decorations.from(f),
});

const look = EditorView.theme(
  {
    "&": { color: theme.foreground!, backgroundColor: theme.background!, height: "100%", fontSize: "13px" },
    ".cm-scroller": { fontFamily: "ui-monospace, SFMono-Regular, Menlo, monospace", lineHeight: "1.5" },
    ".cm-gutters": { backgroundColor: theme.background!, color: theme.brightBlack!, border: "none" },
    ".cm-activeLine": { backgroundColor: "#31324480" },
    ".cm-activeLineGutter": { backgroundColor: "#31324480", color: theme.foreground! },
    ".cm-selectionBackground, &.cm-focused .cm-selectionBackground": { backgroundColor: theme.selectionBackground! },
    ".cm-cursor": { borderLeftColor: theme.cursor!, borderLeftWidth: "2px" },
    ".cm-diag-error": { textDecoration: `underline wavy ${theme.red}`, textUnderlineOffset: "3px" },
    ".cm-diag-warning": { textDecoration: `underline wavy ${theme.yellow}`, textUnderlineOffset: "3px" },
    ".cm-diag-info, .cm-diag-hint": { textDecoration: `underline dotted ${theme.blue}`, textUnderlineOffset: "3px" },
    ".cm-debug-line": { backgroundColor: "#f9e2af30", boxShadow: `inset 3px 0 0 ${theme.yellow}` },
    ".cm-mark-line": { backgroundColor: "#cba6f728", boxShadow: `inset 3px 0 0 ${theme.magenta}` },
  },
  { dark: true },
);

const colours = HighlightStyle.define([
  { tag: [t.keyword, t.controlKeyword, t.moduleKeyword, t.operatorKeyword], color: theme.magenta },
  { tag: [t.string, t.special(t.string), t.regexp], color: theme.green },
  { tag: [t.number, t.bool, t.null, t.atom], color: "#fab387" },
  { tag: [t.comment, t.lineComment, t.blockComment], color: theme.brightBlack, fontStyle: "italic" },
  { tag: [t.function(t.variableName), t.function(t.propertyName), t.macroName], color: theme.blue },
  { tag: [t.typeName, t.className, t.namespace], color: theme.yellow },
  { tag: [t.definition(t.variableName), t.propertyName], color: theme.foreground },
  { tag: [t.operator, t.punctuation, t.bracket], color: "#9399b2" },
  { tag: [t.meta, t.attributeName], color: theme.cyan },
]);

function support(file: string, lang?: string): LanguageSupport | null {
  const ext = (file.split(".").pop() ?? "").toLowerCase();
  const l = lang ?? "";
  if (l === "rust" || ext === "rs") return rust();
  if (l === "python" || ext === "py") return python();
  if (["typescript", "typescriptreact"].includes(l) || ["ts", "tsx", "mts", "cts"].includes(ext)) return javascript({ typescript: true, jsx: ext === "tsx" });
  if (["javascript", "javascriptreact"].includes(l) || ["js", "jsx", "mjs", "cjs", "json"].includes(ext)) return javascript({ jsx: ext === "jsx" });
  return null;
}

function language(file: string, lang?: string): Extension {
  return support(file, lang) ?? [];
}

/** The same colours as classes (`hl-*` in style.css), for text drawn
 * outside an editor: a diff's lines. */
const classes = tagHighlighter([
  { tag: [t.keyword, t.controlKeyword, t.moduleKeyword, t.operatorKeyword], class: "hl-k" },
  { tag: [t.string, t.special(t.string), t.regexp], class: "hl-s" },
  { tag: [t.number, t.bool, t.null, t.atom], class: "hl-n" },
  { tag: [t.comment, t.lineComment, t.blockComment], class: "hl-c" },
  { tag: [t.function(t.variableName), t.function(t.propertyName), t.macroName], class: "hl-f" },
  { tag: [t.typeName, t.className, t.namespace], class: "hl-t" },
  { tag: [t.operator, t.punctuation, t.bracket], class: "hl-p" },
  { tag: [t.meta, t.attributeName], class: "hl-m" },
]);

/** A piece of a line and its class ("" for none). */
export type Span = [string, string];

/** `lines` of `file` highlighted, one list of spans per line; null for a
 * language this doesn't know. Parsed together, so a hunk's lines read as
 * one piece of code. */
export function highlightLines(file: string, lines: string[]): Span[][] | null {
  const lang = support(file);
  if (!lang) return null;
  const text = lines.join("\n");
  const out: Span[][] = [[]];
  highlightCode(
    text,
    lang.language.parser.parse(text),
    classes,
    (code, cls) => out[out.length - 1].push([code, cls]),
    () => out.push([]),
  );
  return out;
}

export class CodeView {
  view: EditorView;
  file: string | null = null;

  /** How its label describes it ("following", "read-only"). */
  private what: string;

  constructor(parent: HTMLElement, what = "following") {
    this.what = what;
    this.view = new EditorView({ parent, state: this.state("", "") });
  }

  private state(doc: string, file: string, lang?: string): EditorState {
    return EditorState.create({
      doc,
      extensions: [
        lineNumbers(),
        highlightActiveLine(),
        drawSelection({ cursorBlinkRate: 0 }),
        syntaxHighlighting(colours),
        language(file, lang),
        marks,
        debugLine,
        markLine,
        look,
        EditorState.readOnly.of(true),
        EditorView.editable.of(false),
        // The page's selection, for "mention these lines", is a reader's own.
        EditorView.contentAttributes.of({ "aria-label": `${file} (${this.what})` }),
      ],
    });
  }

  /** A file, whole. */
  open(file: string, text: string, lang?: string) {
    this.file = file;
    this.view.setState(this.state(text, file, lang));
  }

  private offset(p: Pos): number {
    const doc = this.view.state.doc;
    if (p[0] > doc.lines) return doc.length;
    const line = doc.line(Math.max(1, p[0]));
    return Math.min(line.to, line.from + Math.max(0, p[1]));
  }

  /** The editor's changes, all against the file before them. False if
   * they don't fit (something was missed): follow again. */
  edit(changes: { range: [number, number, number, number]; text: string }[]): boolean {
    try {
      const spec = changes.map((c) => ({ from: this.offset([c.range[0], c.range[1]]), to: this.offset([c.range[2], c.range[3]]), insert: c.text }));
      spec.sort((a, b) => a.from - b.from);
      for (let i = 1; i < spec.length; i++) if (spec[i].from < spec[i - 1].to) return false;
      this.view.dispatch({ changes: spec });
      return true;
    } catch {
      return false;
    }
  }

  /** Where the editor's cursor is, and what it selected; scrolled to. */
  cursor(at: Pos, sel?: [number, number, number, number] | null) {
    const head = this.offset(at);
    const range = sel ? EditorSelection.range(this.offset([sel[0], sel[1]]), this.offset([sel[2], sel[3]])) : EditorSelection.cursor(head);
    this.view.dispatch({
      selection: EditorSelection.create([range]),
      effects: EditorView.scrollIntoView(head, { y: "nearest", yMargin: 80 }),
    });
  }

  diagnostics(items: Diagnostic[]) {
    const ds: Range<Decoration>[] = [];
    for (const d of items) {
      let from = this.offset([d.range[0], d.range[1]]);
      let to = this.offset([d.range[2], d.range[3]]);
      if (to <= from) {
        // A point: underline the word (or character) there.
        const line = this.view.state.doc.lineAt(from);
        to = Math.min(line.to, from + 1);
        if (to <= from) from = Math.max(line.from, from - 1);
        if (to <= from) continue;
      }
      ds.push(Decoration.mark({ class: `cm-diag-${d.severity}`, attributes: { title: d.message } }).range(from, to));
    }
    this.view.dispatch({ effects: setMarks.of(Decoration.set(ds, true)) });
  }

  /** The debugger's line, or none. */
  debug(line: number | null) {
    const doc = this.view.state.doc;
    const set = line && line <= doc.lines ? Decoration.set([Decoration.line({ class: "cm-debug-line" }).range(doc.line(line).from)]) : Decoration.none;
    this.view.dispatch({ effects: setDebug.of(set) });
  }

  /** New text for the same file: only what changed is replaced, so the
   * scroll position and marks move with their lines. */
  replace(text: string) {
    const old = this.view.state.doc.toString();
    if (old === text) return;
    let from = 0;
    while (from < old.length && from < text.length && old[from] === text[from]) from++;
    let a = old.length;
    let b = text.length;
    while (a > from && b > from && old[a - 1] === text[b - 1]) {
      a--;
      b--;
    }
    this.view.dispatch({ changes: { from, to: a, insert: text.slice(from, b) } });
  }

  /** Mark a line (from 1), or none. */
  mark(line: number | null) {
    const doc = this.view.state.doc;
    const set = line && line <= doc.lines ? Decoration.set([Decoration.line({ class: "cm-mark-line" }).range(doc.line(line).from)]) : Decoration.none;
    this.view.dispatch({ effects: setMark.of(set) });
  }

  /** Scroll a line (from 1) to the middle. */
  goto(line: number) {
    const doc = this.view.state.doc;
    const at = doc.line(Math.min(Math.max(1, line), doc.lines)).from;
    this.view.dispatch({ effects: EditorView.scrollIntoView(at, { y: "center" }) });
  }

  /** The text, for tests. */
  text(): string {
    return this.view.state.doc.toString();
  }

  destroy() {
    this.view.destroy();
  }
}
