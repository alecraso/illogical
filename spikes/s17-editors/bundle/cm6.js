// CodeMirror 6, read-only: line numbers, Lezer grammars for three languages, lint underlines,
// a selection, the terminal's colours.
import { EditorSelection, EditorState } from "@codemirror/state";
import { EditorView, drawSelection, highlightActiveLine, lineNumbers } from "@codemirror/view";
import { defaultHighlightStyle, syntaxHighlighting } from "@codemirror/language";
import { lintGutter, setDiagnostics } from "@codemirror/lint";
import { rust } from "@codemirror/lang-rust";
import { javascript } from "@codemirror/lang-javascript";
import { python } from "@codemirror/lang-python";

const langs = { rust, javascript, python };
const theme = EditorView.theme(
  {
    "&": { color: "#cdd6f4", backgroundColor: "#1e1e2e" },
    ".cm-gutters": { backgroundColor: "#1e1e2e", color: "#585b70", border: "none" },
    ".cm-selectionBackground, &.cm-focused .cm-selectionBackground": { backgroundColor: "#585b7080" },
  },
  { dark: true },
);
const view = new EditorView({
  parent: document.getElementById("code"),
  state: EditorState.create({
    doc: window.SAMPLE,
    selection: EditorSelection.range(60, 120),
    extensions: [
      lineNumbers(),
      highlightActiveLine(),
      drawSelection(),
      syntaxHighlighting(defaultHighlightStyle),
      langs[window.LANG ?? "rust"](),
      lintGutter(),
      theme,
      EditorState.readOnly.of(true),
      EditorView.editable.of(false),
    ],
  }),
});
view.dispatch(setDiagnostics(view.state, [{ from: 40, to: 47, severity: "warning", message: "unused" }]));
window.view = view;
