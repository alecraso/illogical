// The read-only viewer behind a share link (`/share/<token>`, M4c): one
// pane, drawn live, at the size its owner gives it. It sends nothing at
// all; the daemon hangs up on a viewer that tries, and when the link
// expires or is revoked.

import { Terminal } from "@xterm/xterm";
import { Unicode11Addon } from "@xterm/addon-unicode11";
import "@xterm/xterm/css/xterm.css";
import { decodeFrame, FrameKind } from "./proto";
import { theme } from "./theme";
import { FONT_FAMILY, FONT_SIZE } from "./terminal-view";

const style = document.createElement("style");
style.textContent = `
  html, body { margin: 0; height: 100%; background: ${theme.background}; color: ${theme.foreground};
    font: 13px system-ui, sans-serif; }
  #share-bar { display: flex; gap: 12px; justify-content: space-between; padding: 6px 12px;
    background: #181825; border-bottom: 1px solid #313244; }
  #share-state { color: #a6adc8; }
  #share-state.ended { color: #f38ba8; }
  #share-term { padding: 6px; overflow: auto; height: calc(100% - 40px); box-sizing: border-box; }
`;
document.head.append(style);

const token = location.pathname.split("/")[2] ?? "";
const what = document.getElementById("share-what")!;
const state = document.getElementById("share-state")!;

const term = new Terminal({
  theme,
  fontFamily: FONT_FAMILY,
  fontSize: FONT_SIZE,
  cursorBlink: false,
  disableStdin: true,
  scrollback: 10000,
  allowProposedApi: true,
});
term.loadAddon(new Unicode11Addon());
term.unicode.activeVersion = "11";
term.open(document.getElementById("share-term")!);

let offset: number | null = null;
let ended: string | null = null;
let expires = 0;

function left(): string {
  const s = Math.max(0, Math.round((expires - Date.now()) / 1000));
  return s < 60 ? `${s}s` : s < 3600 ? `${Math.round(s / 60)}m` : s < 86400 ? `${Math.round(s / 3600)}h` : `${Math.round(s / 86400)}d`;
}

function showState() {
  if (ended !== null) {
    state.textContent = `Ended: ${ended}`;
    state.className = "ended";
  } else if (expires) {
    state.textContent = `Read-only · live · link expires in ${left()}`;
  }
}
setInterval(showState, 10_000);

const sock = new WebSocket(`${location.protocol === "https:" ? "wss" : "ws"}://${location.host}/share/${token}/ws`);
sock.binaryType = "arraybuffer";
sock.onmessage = (e) => {
  if (typeof e.data === "string") {
    const msg = JSON.parse(e.data) as { type: string; pane?: number; expires_ms?: number; cols?: number; rows?: number };
    if (msg.type === "share") {
      what.textContent = `Pane %${msg.pane}`;
      expires = msg.expires_ms ?? 0;
      showState();
    } else if (msg.type === "size" && msg.cols && msg.rows) {
      term.resize(msg.cols, msg.rows);
    }
    return;
  }
  const f = decodeFrame(e.data as ArrayBuffer);
  if (f.kind === FrameKind.Snapshot) {
    term.reset();
    term.write(f.data);
    offset = f.offset;
    return;
  }
  if (f.kind !== FrameKind.Output || offset === null) return;
  let data = f.data;
  if (f.offset < offset) {
    const skip = offset - f.offset;
    if (skip >= data.length) return;
    data = data.subarray(skip);
  }
  term.write(data);
  offset = Math.max(offset, f.offset) + data.length;
};
sock.onclose = (e) => {
  ended = e.reason || (e.code === 1006 ? "the link doesn't work (expired, revoked, or not for you)" : "disconnected");
  showState();
};

// For end-to-end tests.
Object.assign(window, {
  __share: {
    text: () => {
      const b = term.buffer.active;
      const lines: string[] = [];
      for (let i = 0; i < b.length; i++) lines.push(b.getLine(i)?.translateToString(true) ?? "");
      return lines.join("\n");
    },
    get ended() {
      return ended;
    },
  },
});
