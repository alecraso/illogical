// illogical's extension for editor blocks (M27). It runs in code-server's
// extension host, on the block's machine, and tells the daemon what the
// block shows: the active file, the cursor, and the lines around it (the
// swarm's preview). The block's workspace names the block
// (`illogical.block`); the daemon is `$ILLOGICAL_SOCK`.
//
// Reports are throttled, never debounced (S17: a trailing debounce starves
// while someone types), and only sent when something changed.

const vscode = require("vscode");
const http = require("http");

/** At most one report this often (ms). */
const EVERY = 250;
/** Lines of context around the cursor, above it. */
const ABOVE = 3;
const LINES = 7;

let block = 0;
let sock = "";
let timer = null;
let last = 0;
let sent = "";

function activate(context) {
  block = Number(vscode.workspace.getConfiguration("illogical").get("block")) || 0;
  sock = process.env.ILLOGICAL_SOCK || "";
  if (!block || !sock) return;
  const kick = () => schedule();
  context.subscriptions.push(
    vscode.window.onDidChangeActiveTextEditor(kick),
    vscode.window.onDidChangeTextEditorSelection(kick),
    vscode.workspace.onDidChangeTextDocument((e) => {
      if (vscode.window.activeTextEditor && e.document === vscode.window.activeTextEditor.document) kick();
    }),
    vscode.workspace.onDidSaveTextDocument(kick),
    { dispose: () => timer && clearTimeout(timer) },
  );
  report();
}

function schedule(after) {
  if (timer) return;
  const wait = after ?? Math.max(0, last + EVERY - Date.now());
  timer = setTimeout(() => {
    timer = null;
    report();
  }, wait);
}

function snapshot() {
  const dirty = vscode.workspace.textDocuments.filter((d) => d.isDirty).length;
  const ed = vscode.window.activeTextEditor;
  if (!ed || ed.document.uri.scheme !== "file") return { file: null, dirty };
  const doc = ed.document;
  const at = ed.selection.active;
  const top = Math.max(0, Math.min(at.line - ABOVE, doc.lineCount - LINES));
  const lines = [];
  for (let i = top; i < Math.min(doc.lineCount, top + LINES); i++) lines.push(doc.lineAt(i).text.slice(0, 240));
  return { file: doc.uri.fsPath, line: at.line + 1, col: at.character + 1, top: top + 1, lines, dirty };
}

function report() {
  last = Date.now();
  const body = JSON.stringify(snapshot());
  if (body === sent) return;
  sent = body;
  const req = http.request(
    {
      socketPath: sock,
      path: `/api/blocks/${block}/call/report`,
      method: "POST",
      headers: { "content-type": "application/json", "content-length": Buffer.byteLength(body) },
    },
    (res) => {
      res.resume();
      // Not this daemon's block (any more): stop.
      if (res.statusCode === 404) block = 0;
    },
  );
  req.on("error", () => {
    // The daemon is restarting: say it again when it's back.
    sent = "";
    if (block) schedule(2000);
  });
  req.end(body);
}

function deactivate() {}

module.exports = { activate, deactivate };
