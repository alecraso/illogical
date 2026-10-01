// M0 client: one pane, kept in step with the daemon across reconnects.
//
// Every output frame carries its absolute offset in the pane's stream. The
// client remembers the offset just past the last byte it drew, and on
// reconnect asks to resume from there; the daemon replays the gap or, if it
// no longer has it, sends a snapshot.

import "./style.css";
import { decodeFrame, encodeFrame, FrameKind, type ClientMsg, type ServerMsg } from "./proto";
import { TerminalView } from "./terminal-view";

const PANE = 1;

const host = document.getElementById("pane")!;
const status = document.getElementById("status")!;
const view = new TerminalView(host);

let ws: WebSocket | undefined;
let client: number | undefined;
let owner: number | null = null;
let epoch: number | undefined;
let offset: number | null = null;
let retry = 0;

function send(msg: ClientMsg) {
  if (ws?.readyState === WebSocket.OPEN) ws.send(JSON.stringify(msg));
}

function showStatus(text: string | undefined) {
  status.hidden = !text;
  status.textContent = text ?? "";
}

/** Make this client's size the pane's size. */
function claimSize() {
  const size = view.fittedSize();
  if (!size) return;
  view.setScaleToFit(false);
  view.resize(size.cols, size.rows);
  send({ type: "resize", pane: PANE, ...size });
  owner = client ?? null;
}

function attach(from: number | null) {
  send({ type: "attach", panes: [{ pane: PANE, offset: from }] });
}

function onMessage(msg: ServerMsg) {
  switch (msg.type) {
    case "hello": {
      client = msg.client;
      retry = 0;
      showStatus(undefined);
      const info = msg.panes.find((p) => p.id === PANE);
      if (!info) return;
      if (info.epoch !== epoch) {
        // A different stream (daemon restarted): our offset means nothing.
        epoch = info.epoch;
        offset = null;
      }
      // Resize first so a snapshot comes back already at our size.
      claimSize();
      attach(offset);
      break;
    }
    case "size":
      owner = msg.owner;
      // Someone else's size: draw at it (letterboxed) rather than fight.
      if (msg.owner !== client) {
        view.setScaleToFit(true);
        view.resize(msg.cols, msg.rows);
      }
      break;
    case "resync":
      attach(null);
      break;
    case "exit":
      break;
  }
}

function onFrame(buf: ArrayBuffer) {
  const f = decodeFrame(buf);
  if (f.pane !== PANE) return;
  if (f.kind === FrameKind.Snapshot) {
    view.reset();
    view.write(f.data);
    offset = f.offset;
    return;
  }
  if (f.kind !== FrameKind.Output || offset === null) return;
  let data = f.data;
  if (f.offset > offset) {
    // A gap we can't fill: start over from a snapshot.
    offset = null;
    attach(null);
    return;
  }
  if (f.offset < offset) {
    const skip = offset - f.offset;
    if (skip >= data.length) return;
    data = data.subarray(skip);
  }
  view.write(data);
  offset += data.length;
}

function connect() {
  const url = `${location.protocol === "https:" ? "wss" : "ws"}://${location.host}/ws`;
  const sock = new WebSocket(url);
  sock.binaryType = "arraybuffer";
  ws = sock;
  showStatus(retry ? "reconnecting…" : "connecting…");
  sock.onmessage = (e) => {
    if (typeof e.data === "string") onMessage(JSON.parse(e.data) as ServerMsg);
    else onFrame(e.data as ArrayBuffer);
  };
  sock.onclose = () => {
    if (ws !== sock) return;
    ws = undefined;
    client = undefined;
    const delay = Math.min(250 * 2 ** retry, 5000);
    retry++;
    showStatus(`disconnected; retrying in ${Math.round(delay / 100) / 10}s`);
    setTimeout(connect, delay);
  };
}

view.onInput((data) => {
  // Typing here makes this window the one whose size counts.
  if (owner !== client) claimSize();
  if (ws?.readyState === WebSocket.OPEN) ws.send(encodeFrame(FrameKind.Input, PANE, data));
});
view.onTitle((t) => {
  document.title = t ? `${t} — illogical` : "illogical";
});

let resizeTimer: number | undefined;
new ResizeObserver(() => {
  clearTimeout(resizeTimer);
  resizeTimer = window.setTimeout(() => {
    if (owner === client || owner === null) claimSize();
    else view.setScaleToFit(true);
  }, 80);
}).observe(host);

window.addEventListener("focus", () => {
  if (owner !== client) claimSize();
});
document.addEventListener("visibilitychange", () => {
  // Phones drop sockets in the background; come back quickly.
  if (document.visibilityState === "visible" && !ws) {
    retry = 0;
    connect();
  }
});

// For end-to-end tests.
Object.assign(window, {
  __illogical: { text: () => view.text(), offset: () => offset, size: () => [view.cols, view.rows] },
});

connect();
view.focus();
