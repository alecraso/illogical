// Editor blocks (M27): VS Code (code-server) on a folder on the block's
// machine, in a frame on the block's own origin, like a browser block on a
// port. The daemon starts the server; until it's up the block says why.

import { render } from "preact";
import type { Client } from "../client";
import type { OpenRequest, PaneId } from "../proto";
import { blocksOff } from "./browser";
import { registerBlock, type BlockView } from "./view";

type Server =
  | { is: "stopped" }
  | { is: "fetching"; got: number; of: number }
  | { is: "starting" }
  | { is: "running" }
  | { is: "failed"; error: string };

export interface EditorState {
  folder: string;
  name: string;
  /** The active file, relative to the folder when it's inside it. */
  file: string | null;
  line: number | null;
  col: number | null;
  /** The lines around the cursor, from line `top`. */
  top: number | null;
  lines: string[];
  dirty: number;
  /** What the frame loads (fixed for the block's life). */
  src: string;
  reloads: number;
  server: Server | null;
  machine: string | null;
  error: string | null;
}

/**
 * Open an editor on a pane's machine, in its directory (the daemon fills
 * both in from `from`), in a tab of its own.
 */
export function openEditor(client: Client, from: PaneId, path?: string) {
  if (!client.has("blocks")) return blocksOff("editor");
  void client.api("/api/blocks", { type: "editor", config: path ? { path } : {}, from_pane: from } satisfies OpenRequest, "couldn't open an editor");
}

const MB = (n: number) => `${Math.round(n / 1e6)} MB`;

function EditorBlock({ client, id, s }: { client: Client; id: PaneId; s: EditorState | null }) {
  if (!s) return <div class="browser-card dim">…</div>;
  const server = s.server ?? { is: "stopped" };
  if (s.error || server.is === "failed") {
    return (
      <div class="browser-card error">
        <p>VS Code couldn't open {s.name || "here"}</p>
        <p class="dim">{s.error ?? (server.is === "failed" ? server.error : "")}</p>
        {!s.error && <button onClick={() => void client.api(`/api/blocks/${id}/call/start`, {})}>Try again</button>}
      </div>
    );
  }
  if (server.is === "fetching") {
    const pct = server.of ? Math.floor((100 * server.got) / server.of) : 0;
    return (
      <div class="browser-card dim" data-editor="fetching">
        <p>Downloading VS Code (code-server) for the first time…</p>
        <p>{server.of ? `${pct}% of ${MB(server.of)}` : MB(server.got)}</p>
      </div>
    );
  }
  if (!s.src) return <div class="browser-card dim">Finding {s.name || "the folder"}…</div>;
  return (
    <div class="editor">
      {/* Its own origin, never the app's; `allow-same-origin` means that one. */}
      <iframe
        key={`${s.src}#${s.reloads}`}
        class="editor-frame"
        src={s.src}
        title={`VS Code: ${s.file ?? s.name}`}
        sandbox="allow-scripts allow-forms allow-same-origin allow-popups allow-downloads"
        allow="clipboard-read; clipboard-write"
        referrerpolicy="no-referrer"
      />
      {server.is === "starting" && <div class="editor-note">Starting VS Code…</div>}
    </div>
  );
}

registerBlock("editor", (client, id): BlockView => {
  const host = document.createElement("div");
  host.className = "block block-editor";
  let state: EditorState | null = null;
  const draw = () => render(<EditorBlock client={client} id={id} s={state} />, host);
  draw();
  return {
    host,
    setVisible: (v) => (host.style.display = v ? "" : "none"),
    update: (s) => {
      state = s as EditorState;
      draw();
    },
    title: () => (state ? (state.file ? `${state.file.split("/").pop()} — ${state.name}` : state.name || "editor") : "editor"),
    text: () => (state ? `${state.file ?? state.folder}${state.line ? `:${state.line}` : ""}\n${state.lines.join("\n")}`.trim() : ""),
    focus: () => host.querySelector<HTMLElement>("iframe")?.focus(),
    dispose: () => {
      render(null, host);
      host.remove();
    },
  };
});
