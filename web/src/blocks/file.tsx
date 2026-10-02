// File blocks (M11): one file on the block's machine, read-only, in the
// follow view's CodeMirror (highlighting, line numbers, the terminal's
// colours), with its line marked. The daemon follows the file while
// someone draws it; edits replace only what changed, so where you've
// scrolled stays put, and the view scrolls to the line only when someone
// moves it there.

import { render } from "preact";
import { useEffect, useRef, useState } from "preact/hooks";
import type { Client } from "../client";
import type { PaneId } from "../proto";
import type { CodeView } from "../swarm/code";
import { registerBlock, type BlockView } from "./view";

export interface FileState {
  path: string;
  real: string | null;
  name: string;
  line: number | null;
  jump: number;
  size: number;
  mtime_ms: number;
  text: string;
  rev: number;
  truncated: boolean;
  binary: boolean;
  loading: boolean;
  error: string | null;
  watching: boolean;
  updated_ms: number;
}

const KB = (n: number) => (n < 1024 ? `${n} B` : `${Math.round(n / 1024)} KB`);

function FileBlock({ client, id, s }: { client: Client; id: PaneId; s: FileState | null }) {
  const box = useRef<HTMLDivElement>(null);
  const code = useRef<CodeView | null>(null);
  const shown = useRef({ path: "", rev: -1, jump: -1 });
  const [ready, setReady] = useState(false);

  useEffect(() => {
    let dead = false;
    void import("../swarm/code").then(({ CodeView }) => {
      if (dead || !box.current) return;
      code.current = new CodeView(box.current, "read-only");
      setReady(true);
    });
    return () => {
      dead = true;
      code.current?.destroy();
      code.current = null;
    };
  }, []);

  useEffect(() => {
    const c = code.current;
    if (!c || !s || s.error || s.binary) return;
    const path = s.real ?? s.path;
    const was = shown.current;
    if (was.path !== path) c.open(path, s.text);
    else if (was.rev !== s.rev) c.replace(s.text);
    c.mark(s.line);
    if (s.line && (was.jump !== s.jump || was.path !== path)) c.goto(s.line);
    shown.current = { path, rev: s.rev, jump: s.jump };
  }, [ready, s?.rev, s?.jump, s?.line, s?.real, s?.path, s?.error, s?.binary]);

  const machine = client.machine(id);
  const path = s ? (s.real ?? s.path) : "";
  const slash = path.lastIndexOf("/");
  const problem = s?.error ?? (s?.binary ? `A binary file (${KB(s.size)})` : null);
  return (
    <div class="review" data-file-block={id}>
      <div class="review-bar">
        <span class="review-path" title={path}>
          <b>{path.slice(slash + 1)}</b>
          {s?.line ? <span class="dim">:{s.line}</span> : null} <span class="dim">{path.slice(0, slash)}</span>
        </span>
        {machine && (
          <span class="host-tag" title={`on ${machine.name ?? machine.sprite}`}>
            VM
          </span>
        )}
        {s?.truncated && <span class="dim" title={`${KB(s.size)}: only the start is shown`}>first {KB(s.text.length)}</span>}
        <span class={`review-live ${s?.watching ? "on" : ""}`} title={s?.watching ? "Updates as the file changes" : "Not watching: nobody's looking"}>
          {s?.watching ? "live" : "paused"}
        </span>
      </div>
      {problem && (
        <div class="browser-card error">
          <p>{s?.error ? "Can't show this file" : problem}</p>
          {s?.error && <p class="dim">{s.error}</p>}
        </div>
      )}
      {!s || (s.loading && !s.text) ? <div class="browser-card dim">Reading {s?.name ?? "the file"}…</div> : null}
      <div class="file-code" ref={box} style={problem || !s || (s.loading && !s.text) ? { display: "none" } : undefined} />
    </div>
  );
}

registerBlock("file", (client, id): BlockView => {
  const host = document.createElement("div");
  host.className = "block block-file";
  let state: FileState | null = null;
  const draw = () => render(<FileBlock client={client} id={id} s={state} />, host);
  draw();
  return {
    host,
    setVisible: (v) => (host.style.display = v ? "" : "none"),
    update: (s) => {
      state = s as FileState;
      draw();
    },
    title: () => (state ? `${state.name}${state.line ? `:${state.line}` : ""}` : "file"),
    text: () => state?.text ?? "",
    focus: () => host.querySelector<HTMLElement>(".cm-content")?.focus(),
    dispose: () => {
      render(null, host);
      host.remove();
    },
  };
});
