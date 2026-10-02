// S17: a workspace extension that reports what an editor presence would carry to a unix socket
// on the machine its extension host runs on. Where it runs, and whether it can reach the socket,
// is the question for Remote-SSH and dev containers; the event stream is the question for M28.
//
// Socket: $ILLOGICAL_PROBE_SOCK, else /tmp/illogical-probe.sock. One JSON line per event, raw
// (no debouncing): the listener measures rates and replays policies.
const vscode = require("vscode");
const net = require("net");
const os = require("os");

let sock;
let queue = [];
const t0 = Date.now();

function send(ev, extra) {
  const line = JSON.stringify({ t: Date.now() - t0, ev, ...extra }) + "\n";
  if (sock && !sock.connecting) sock.write(line);
  else queue.push(line);
}

function snapshot() {
  const ed = vscode.window.activeTextEditor;
  const diags = { e: 0, w: 0, i: 0 };
  for (const [, ds] of vscode.languages.getDiagnostics()) {
    for (const d of ds) {
      if (d.severity === vscode.DiagnosticSeverity.Error) diags.e++;
      else if (d.severity === vscode.DiagnosticSeverity.Warning) diags.w++;
      else diags.i++;
    }
  }
  const dirty = vscode.workspace.textDocuments.filter((d) => d.isDirty).length;
  const dbg = vscode.debug.activeDebugSession;
  const item = vscode.debug.activeStackItem;
  if (!ed) return { file: null, ...diags, dirty, debug: dbg ? dbg.type : null };
  const s = ed.selection;
  const vr = ed.visibleRanges[0];
  return {
    file: vscode.workspace.asRelativePath(ed.document.uri),
    line: s.active.line + 1,
    col: s.active.character,
    sel: s.isEmpty ? null : [s.start.line + 1, s.start.character, s.end.line + 1, s.end.character],
    top: vr ? vr.start.line + 1 : null,
    bot: vr ? vr.end.line + 1 : null,
    text: ed.document.lineAt(s.active.line).text,
    ...diags,
    dirty,
    debug: dbg ? { type: dbg.type, frame: item && "frameId" in item ? item.frameId : null } : null,
  };
}

function activate(ctx) {
  const path = process.env.ILLOGICAL_PROBE_SOCK || "/tmp/illogical-probe.sock";
  sock = net.createConnection(path);
  sock.on("connect", () => {
    for (const l of queue) sock.write(l);
    queue = [];
  });
  sock.on("error", (e) => {
    // Where it ran and why it couldn't connect is the result; leave a note beside the workspace.
    const fs = require("fs");
    const folder = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath ?? os.tmpdir();
    fs.writeFileSync(`${folder}/.probe-error.json`, JSON.stringify({ path, error: String(e), hostname: os.hostname(), pid: process.pid }));
  });
  send("hello", {
    hostname: os.hostname(),
    pid: process.pid,
    platform: process.platform,
    remoteName: vscode.env.remoteName ?? null,
    uiKind: vscode.env.uiKind === vscode.UIKind.Web ? "web" : "desktop",
    appName: vscode.env.appName,
    appHost: vscode.env.appHost,
    version: vscode.version,
    folders: (vscode.workspace.workspaceFolders ?? []).map((f) => f.uri.toString()),
    inContainer: require("fs").existsSync("/.dockerenv"),
  });
  const on = (name, event) => ctx.subscriptions.push(event(() => send(name, snapshot())));
  on("activeEditor", vscode.window.onDidChangeActiveTextEditor);
  on("selection", vscode.window.onDidChangeTextEditorSelection);
  on("visibleRanges", vscode.window.onDidChangeTextEditorVisibleRanges);
  on("textChange", vscode.workspace.onDidChangeTextDocument);
  on("diagnostics", vscode.languages.onDidChangeDiagnostics);
  on("save", vscode.workspace.onDidSaveTextDocument);
  on("debugStart", vscode.debug.onDidStartDebugSession);
  on("debugStop", vscode.debug.onDidTerminateDebugSession);
  on("debugStackItem", vscode.debug.onDidChangeActiveStackItem);
  on("tabs", vscode.window.tabGroups.onDidChangeTabs);
  send("ready", snapshot());
}

function deactivate() {
  send("bye", {});
  sock?.end();
}

module.exports = { activate, deactivate };
