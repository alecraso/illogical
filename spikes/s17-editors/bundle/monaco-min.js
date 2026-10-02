// Monaco cut down for a read-only follow view: the editor API, Monarch grammars for three
// languages, the base worker, the terminal's colours, a diagnostic and a selection.
import * as monaco from "monaco-editor/editor/editor.api";
import "monaco-editor/languages/definitions/rust/register";
import "monaco-editor/languages/definitions/typescript/register";
import "monaco-editor/languages/definitions/python/register";
import EditorWorker from "monaco-editor/editor/editor.worker?worker";

self.MonacoEnvironment = { getWorker: () => new EditorWorker() };
monaco.editor.defineTheme("illogical", {
  base: "vs-dark",
  inherit: true,
  rules: [],
  colors: {
    "editor.background": "#1e1e2e",
    "editor.foreground": "#cdd6f4",
    "editor.selectionBackground": "#585b7080",
    "editorCursor.foreground": "#f5e0dc",
  },
});
const view = monaco.editor.create(document.getElementById("code"), {
  value: window.SAMPLE,
  language: "rust",
  readOnly: true,
  theme: "illogical",
  minimap: { enabled: false },
});
monaco.editor.setModelMarkers(view.getModel(), "follow", [
  { startLineNumber: 3, startColumn: 5, endLineNumber: 3, endColumn: 12, message: "unused", severity: monaco.MarkerSeverity.Warning },
]);
view.setSelection({ startLineNumber: 5, startColumn: 1, endLineNumber: 7, endColumn: 4 });
view.revealLineInCenter(5);
window.view = view;
