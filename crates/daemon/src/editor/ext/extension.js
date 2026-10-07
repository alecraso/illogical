// Arugula's extension for VS Code, Cursor and code-server (M27, M28).
//
// It runs in the workspace's extension host (`extensionKind: workspace`):
// on the machine the files are on, which under Remote-SSH or in a dev
// container is the remote one. There it connects to the arugulad on that
// machine (`$ARUGULA_SOCK`, else the daemon's usual socket) and joins the
// swarm as an editor, speaking the daemon's editor protocol (lines of JSON
// over an HTTP upgrade on the socket; see the daemon's `editor/link.rs`):
//
// - `summary`: the file, diagnostic counts, unsaved files, the debugger, a
//   merge conflict. At most once a second, at once when something wants
//   attention (the debugger stopped, a save brought errors).
// - `peek`: the lines around the cursor, for previews.
// - while someone follows (the daemon says `followers`): `follow` (cursor,
//   selection, visible lines) at most every 100 ms, `open` (the file),
//   `edit` (each change) and `diagnostics` (the file's).
//
// In an editor block (M27) the block's workspace names it
// (`arugula.block`) and it's always on. Anywhere else a workspace joins
// only when asked ("arugula: Show this workspace in the swarm"),
// remembered per folder, and the status bar says when someone follows.
//
// Sends are throttled, never debounced (S17: a trailing debounce starves
// while someone types), and only when something changed.

const vscode = require("vscode");
const http = require("http");
const fs = require("fs");
const os = require("os");
const path = require("path");

/** Summaries at most this often (ms), as M23's tick. */
const SUMMARY_MS = 1000;
/** The follow stream at most this often (ms). */
const FOLLOW_MS = 100;
/** Lines of context around the cursor, above it. */
const ABOVE = 3;
const LINES = 7;
/** Files bigger than this aren't sent to followers. */
const MAX_TEXT = 1 << 20;

let ctx = null;
let block = 0;
let on = false;
let sock = null;
let welcome = null;
let followers = 0;
let backoff = 500;
let reconnect = null;
let status = null;
/** What was last sent, to send only changes. */
let lastSummary = {};
let lastPeek = "";
let lastFollow = "";
let openSent = null;
let saved = false;
/** The debugger, as its adapter said. */
let debug = null;
const timers = {};
/** The upgrade's protocol name. A daemon from before the rename (0.25 and
 * older) knows only the old one and refuses the new with a 400 (#505). */
let proto = "arugula-editor";
const OLD_PROTO = "illogical-editor";
/** The extension's id before the rename (#505). */
const OLD_ID = "illogical.illogical-editor";

/** A setting, or its name from before the rename, `illogical.*` (#505):
 * the workspace of an editor block made before the update says
 * `illogical.block`. */
function setting(key) {
  const v = vscode.workspace.getConfiguration("arugula").get(key);
  if (v) return v;
  return vscode.workspace.getConfiguration("illogical").get(key);
}

function activate(context) {
  ctx = context;
  block = Number(setting("block")) || 0;
  on = block > 0 || remembered().includes(folderPath());
  status = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 0);
  status.command = "arugula.menu";
  context.subscriptions.push(
    status,
    vscode.commands.registerCommand("arugula.join", () => setOn(true)),
    vscode.commands.registerCommand("arugula.leave", () => setOn(false)),
    vscode.commands.registerCommand("arugula.menu", menu),
    vscode.window.onDidChangeActiveTextEditor(() => {
      openSent = null;
      changed({ summary: true, peek: true, follow: true });
    }),
    vscode.window.onDidChangeTextEditorSelection(() => changed({ peek: true, follow: true })),
    vscode.window.onDidChangeTextEditorVisibleRanges(() => changed({ follow: true })),
    vscode.workspace.onDidChangeTextDocument(onEdit),
    vscode.workspace.onDidSaveTextDocument(() => {
      saved = true;
      changed({ summary: true, peek: true, now: true });
    }),
    vscode.workspace.onDidOpenTextDocument(() => changed({ summary: true })),
    vscode.workspace.onDidCloseTextDocument(() => changed({ summary: true })),
    vscode.languages.onDidChangeDiagnostics(onDiagnostics),
    vscode.debug.registerDebugAdapterTrackerFactory("*", { createDebugAdapterTracker: tracker }),
    vscode.debug.onDidTerminateDebugSession(() => {
      if (!vscode.debug.activeDebugSession) setDebug(null);
    }),
    { dispose: () => disconnect() },
  );
  if (on) connect();
  else showStatus();
  replaceOld();
}

// The extension under its old name (#505): it's this one, so it goes, and
// the theme picked under the old name is this one's.
async function replaceOld() {
  try {
    const theme = vscode.workspace.getConfiguration("workbench").inspect("colorTheme");
    if (theme?.globalValue === "illogical") {
      await vscode.workspace.getConfiguration("workbench").update("colorTheme", "arugula", vscode.ConfigurationTarget.Global);
    }
    if (vscode.extensions.getExtension(OLD_ID)) {
      await vscode.commands.executeCommand("workbench.extensions.uninstallExtension", OLD_ID);
    }
  } catch {
    // left as it is; both work
  }
}

// ---- joining and leaving

async function setOn(v) {
  if (block) {
    vscode.window.showInformationMessage("This window is an Arugula editor block: it's always in the swarm.");
    return;
  }
  on = v;
  remember(folderPath(), v);
  if (v) connect();
  else disconnect();
  showStatus();
}

async function menu() {
  const items = on
    ? [{ label: "$(circle-slash) Take this workspace out of the swarm", run: () => setOn(false) }]
    : [{ label: "$(broadcast) Show this workspace in the swarm", run: () => setOn(true) }];
  const pick = await vscode.window.showQuickPick(items, { placeHolder: statusText() });
  if (pick) pick.run();
}

function statusText() {
  if (!on) return "Not in your Arugula swarm";
  if (!welcome) return "Connecting to Arugula…";
  if (followers > 0) return `${followers} following this editor in Arugula`;
  return `In your Arugula swarm as %${welcome}`;
}

function showStatus() {
  if (!status) return;
  // Off and no daemon here: say nothing.
  if (!on && !fs.existsSync(socketPath())) {
    status.hide();
    return;
  }
  status.text = !on ? "$(circle-slash) Arugula" : followers > 0 ? `$(eye) ${followers} following` : welcome ? "$(broadcast) Arugula" : "$(sync~spin) Arugula";
  status.tooltip = statusText();
  status.backgroundColor = followers > 0 ? new vscode.ThemeColor("statusBarItem.warningBackground") : undefined;
  status.show();
}

// Folders that joined, remembered on this machine (where the files are),
// written at once so a reload right after leaving stays out.
function rememberedFile() {
  return path.join(ctx.globalStorageUri.fsPath, "folders.json");
}

function remembered() {
  // The old extension's, until this one remembers something (#505). Its
  // storage is beside this one's, under its id.
  const old = path.join(path.dirname(ctx.globalStorageUri.fsPath), OLD_ID, "folders.json");
  for (const f of [rememberedFile(), old]) {
    try {
      return JSON.parse(fs.readFileSync(f, "utf8"));
    } catch {
      // next
    }
  }
  return [];
}

function remember(folder, yes) {
  if (!folder) return;
  const list = remembered().filter((f) => f !== folder);
  if (yes) list.push(folder);
  try {
    fs.mkdirSync(path.dirname(rememberedFile()), { recursive: true });
    fs.writeFileSync(rememberedFile(), JSON.stringify(list));
  } catch {
    // not remembered; on for now
  }
}

function folderPath() {
  return vscode.workspace.workspaceFolders?.[0]?.uri.fsPath ?? null;
}

// ---- the connection

/** The daemon's socket on this machine. */
function socketPath() {
  const set = setting("socket");
  if (set) return set;
  // The old names too: a shell started before the update, a daemon whose
  // state is still in the old directory (#505).
  if (process.env.ARUGULA_SOCK) return process.env.ARUGULA_SOCK;
  if (process.env.ILLOGICAL_SOCK) return process.env.ILLOGICAL_SOCK;
  const base = process.env.XDG_STATE_HOME || path.join(os.homedir(), ".local", "state");
  for (const name of ["arugula", "illogical"]) {
    const state = path.join(base, name);
    try {
      const p = fs.readFileSync(path.join(state, "sock.path"), "utf8").trim();
      if (p) return p;
    } catch {
      // the usual place
    }
    if (fs.existsSync(path.join(state, "sock"))) return path.join(state, "sock");
  }
  return path.join(base, "arugula", "sock");
}

function appName() {
  const n = (vscode.env.appName || "").toLowerCase();
  if (n.includes("cursor")) return "cursor";
  if (n.includes("code-server")) return "code-server";
  return "vscode";
}

function connect() {
  if (sock || !on) return;
  clearTimeout(reconnect);
  const folder = vscode.workspace.workspaceFolders?.[0];
  const req = http.request({
    socketPath: socketPath(),
    path: "/api/editors/connect",
    headers: { Connection: "Upgrade", Upgrade: proto },
  });
  req.on("upgrade", (_res, s, head) => {
    sock = s;
    backoff = 500;
    s.setNoDelay(true);
    let buf = head.length ? head.toString("utf8") : "";
    s.on("data", (d) => {
      buf += d.toString("utf8");
      let i;
      while ((i = buf.indexOf("\n")) >= 0) {
        const line = buf.slice(0, i);
        buf = buf.slice(i + 1);
        if (line.trim()) onMessage(JSON.parse(line));
      }
    });
    s.on("close", () => lost());
    s.on("error", () => {});
    send({
      t: "hello",
      editor: appName(),
      remote: vscode.env.remoteName || null,
      authority: folder && folder.uri.scheme === "vscode-remote" ? folder.uri.authority : remoteAuthority(),
      hostname: os.hostname(),
      workspace: folder ? folder.uri.fsPath : null,
      block: block || null,
    });
    lastSummary = {};
    lastPeek = "";
    sendSummary();
    sendPeek();
  });
  req.on("response", (res) => {
    res.resume();
    // Refused: the daemon may know the protocol by its other name.
    if (res.statusCode === 400) proto = proto === OLD_PROTO ? "arugula-editor" : OLD_PROTO;
    lost();
  });
  req.on("error", () => lost());
  req.end();
}

/** Under Remote-SSH the desktop opens `vscode-remote://ssh-remote+HOST/…`;
 * here, on the remote side, VS Code says only the remote's kind, so HOST is
 * this machine's name unless `arugula.sshHost` says otherwise. */
function remoteAuthority() {
  const host = setting("sshHost") || os.hostname();
  if (vscode.env.remoteName === "ssh-remote") return `ssh-remote+${host}`;
  return null;
}

function lost() {
  sock = null;
  welcome = null;
  followers = 0;
  openSent = null;
  showStatus();
  // The daemon is restarting (or not running yet): try again.
  if (on) {
    clearTimeout(reconnect);
    reconnect = setTimeout(connect, backoff);
    backoff = Math.min(backoff * 2, 10_000);
  }
}

function disconnect() {
  clearTimeout(reconnect);
  for (const k of Object.keys(timers)) clearTimeout(timers[k]);
  if (sock) {
    const s = sock;
    sock = null;
    s.destroy();
  }
  welcome = null;
  followers = 0;
}

function send(msg) {
  if (sock) sock.write(JSON.stringify(msg) + "\n");
}

function onMessage(m) {
  switch (m.t) {
    case "welcome":
      welcome = m.id;
      showStatus();
      break;
    case "followers": {
      const more = m.n > followers;
      followers = m.n;
      showStatus();
      if (more) sendFollowAll();
      break;
    }
    case "resend":
      sendFollowAll();
      break;
    case "continue":
      vscode.commands.executeCommand("workbench.action.debug.continue");
      break;
  }
}

// ---- what changed, throttled

/** Something changed: send what it touches, at most this often each. */
function changed(what) {
  if (!sock) return;
  if (what.summary) throttle("summary", what.now ? 0 : SUMMARY_MS, sendSummary);
  if (what.peek) throttle("peek", block ? 250 : SUMMARY_MS, sendPeek);
  if (what.follow && followers > 0) throttle("follow", FOLLOW_MS, sendFollow);
}

const lastAt = {};
function throttle(key, every, fn) {
  if (every === 0) {
    clearTimeout(timers[key]);
    timers[key] = null;
    lastAt[key] = Date.now();
    fn();
    return;
  }
  if (timers[key]) return;
  const wait = Math.max(0, (lastAt[key] || 0) + every - Date.now());
  timers[key] = setTimeout(() => {
    timers[key] = null;
    lastAt[key] = Date.now();
    fn();
  }, wait);
}

/** Files on this machine (a remote window's are `vscode-remote`). */
function local(uri) {
  return uri.scheme === "file" || uri.scheme === "vscode-remote";
}

function activeFile() {
  const ed = vscode.window.activeTextEditor;
  return ed && local(ed.document.uri) ? ed : null;
}

function counts() {
  const diag = { e: 0, w: 0, i: 0 };
  for (const [, list] of vscode.languages.getDiagnostics()) {
    for (const d of list) {
      if (d.severity === vscode.DiagnosticSeverity.Error) diag.e++;
      else if (d.severity === vscode.DiagnosticSeverity.Warning) diag.w++;
      else diag.i++;
    }
  }
  return diag;
}

/** An open file with merge conflict markers, if any. */
function conflict() {
  for (const d of vscode.workspace.textDocuments) {
    if (!local(d.uri) || d.lineCount > 50_000) continue;
    const t = d.getText();
    if (/^<{7} /m.test(t) && /^={7}$/m.test(t) && /^>{7} /m.test(t)) return d.uri.fsPath;
  }
  return null;
}

function sendSummary() {
  const ed = activeFile();
  const now = {
    file: ed ? ed.document.uri.fsPath : null,
    diag: counts(),
    dirty: vscode.workspace.textDocuments.filter((d) => d.isDirty).length,
    debug,
    conflict: conflict(),
  };
  const out = { t: "summary" };
  let any = false;
  for (const [k, v] of Object.entries(now)) {
    if (JSON.stringify(v) !== JSON.stringify(lastSummary[k])) {
      out[k] = v;
      any = true;
    }
  }
  if (saved) {
    out.saved = true;
    saved = false;
    any = true;
  }
  if (!any) return;
  lastSummary = now;
  send(out);
}

function sendPeek() {
  const dirty = vscode.workspace.textDocuments.filter((d) => d.isDirty).length;
  const ed = activeFile();
  let msg;
  if (!ed) msg = { t: "peek", file: null, dirty };
  else {
    const doc = ed.document;
    const at = ed.selection.active;
    const top = Math.max(0, Math.min(at.line - ABOVE, doc.lineCount - LINES));
    const lines = [];
    for (let i = top; i < Math.min(doc.lineCount, top + LINES); i++) lines.push(doc.lineAt(i).text.slice(0, 240));
    msg = { t: "peek", file: doc.uri.fsPath, line: at.line + 1, col: at.character + 1, top: top + 1, lines, dirty };
  }
  const s = JSON.stringify(msg);
  if (s === lastPeek) return;
  lastPeek = s;
  send(msg);
}

// ---- the follow stream: lines from 1, columns from 0 (UTF-16 units)

function range(r) {
  return [r.start.line + 1, r.start.character, r.end.line + 1, r.end.character];
}

function sendFollow() {
  const ed = activeFile();
  if (!ed) return;
  if (openSent !== ed.document.uri.fsPath) sendOpen(ed.document);
  const sel = ed.selection;
  const vis = ed.visibleRanges[0];
  const msg = {
    t: "follow",
    file: ed.document.uri.fsPath,
    line: sel.active.line + 1,
    col: sel.active.character,
    sel: sel.isEmpty ? null : range(sel),
    view: vis ? [vis.start.line + 1, vis.end.line + 1] : null,
  };
  const s = JSON.stringify(msg);
  if (s === lastFollow) return;
  lastFollow = s;
  send(msg);
}

function sendOpen(doc) {
  const text = doc.getText();
  const big = text.length > MAX_TEXT;
  openSent = doc.uri.fsPath;
  send({ t: "open", file: doc.uri.fsPath, version: doc.version, lang: doc.languageId, text: big ? null : text, too_big: big || undefined });
  sendDiagnostics(doc.uri);
}

function sendFollowAll() {
  openSent = null;
  lastFollow = "";
  const ed = activeFile();
  if (ed) {
    sendOpen(ed.document);
    sendFollow();
  }
}

function onEdit(e) {
  const ed = activeFile();
  if (ed && e.document === ed.document) {
    if (followers > 0 && openSent === e.document.uri.fsPath && e.contentChanges.length) {
      send({
        t: "edit",
        file: e.document.uri.fsPath,
        version: e.document.version,
        changes: e.contentChanges.map((c) => ({ range: range(c.range), text: c.text })),
      });
    }
    changed({ peek: true, follow: true });
  }
  if (e.document.isDirty !== undefined) changed({ summary: true });
}

const SEVERITY = ["error", "warning", "info", "hint"];

function sendDiagnostics(uri) {
  const items = vscode.languages.getDiagnostics(uri).map((d) => ({ range: range(d.range), severity: SEVERITY[d.severity] || "info", message: d.message.slice(0, 500) }));
  send({ t: "diagnostics", file: uri.fsPath, items });
}

function onDiagnostics(e) {
  changed({ summary: true });
  const ed = activeFile();
  if (ed && followers > 0 && openSent === ed.document.uri.fsPath && e.uris.some((u) => u.toString() === ed.document.uri.toString())) {
    sendDiagnostics(ed.document.uri);
  }
}

// ---- the debugger, from what its adapter says

function setDebug(d) {
  if (JSON.stringify(d) === JSON.stringify(debug)) return;
  debug = d;
  // Attention goes at once.
  changed({ summary: true, now: true });
}

function tracker() {
  return {
    onDidSendMessage(m) {
      if (m.type === "event" && m.event === "stopped") {
        setDebug({ state: "paused", reason: m.body?.reason || null, file: debug?.file ?? null, line: debug?.line ?? null });
      } else if (m.type === "event" && m.event === "continued") {
        setDebug({ state: "running" });
      } else if (m.type === "response" && m.command === "stackTrace" && debug?.state === "paused") {
        const top = m.body?.stackFrames?.[0];
        if (top) setDebug({ ...debug, file: top.source?.path || null, line: top.line || null });
      } else if (m.type === "event" && (m.event === "terminated" || m.event === "exited")) {
        setDebug(null);
      }
    },
    onWillReceiveMessage(m) {
      // Continue, step: running again before the adapter says so.
      if (m.type === "request" && ["continue", "next", "stepIn", "stepOut"].includes(m.command) && debug?.state === "paused") {
        setDebug({ state: "running" });
      }
    },
  };
}

function deactivate() {
  disconnect();
}

module.exports = { activate, deactivate };
