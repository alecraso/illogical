// Following an editor (M28): a read-only view of the file someone has open
// in VS Code, Cursor or nvim, that follows their cursor across files, with
// their selection, the file's diagnostics and the debugger's line. It reads
// only what that editor sends while someone follows (files open in it), on
// this page's own connection to the editor's machine.
//
// From here: Continue a paused debugger, open the same file and line in
// your own editor ("Open here"), or hand the lines to Claude Code in a
// terminal on the same machine.

import { useEffect, useRef, useState } from "preact/hooks";
import type { Fleet, FleetPane } from "../fleet";
import type { ActRequest, ActResponse, FollowMsg, OpenRequest, OpenResponse } from "../proto";
import { openMenu, type MenuItem } from "../ui/menu";
import { useSubscribe } from "../ui/hooks";
import type { CodeView } from "./code";

/** What an editor is called. */
export function appName(app: string | undefined): string {
  return { vscode: "VS Code", cursor: "Cursor", "code-server": "VS Code", nvim: "nvim" }[app ?? ""] ?? app ?? "editor";
}

type Cursor = Extract<FollowMsg, { line: number }>;

export function FollowView({ fleet, pkey, close, back }: { fleet: Fleet; pkey: string; close: () => void; back: () => void }) {
  useSubscribe((fn) => fleet.subscribe(fn));
  const p = fleet.panes.find((x) => x.key === pkey);
  const [host, id] = [p?.host ?? pkey.split(":")[0], p?.id ?? Number(pkey.split(":").pop())];
  const box = useRef<HTMLDivElement>(null);
  const code = useRef<CodeView | null>(null);
  const [file, setFile] = useState<string | null>(null);
  const [at, setAt] = useState<Cursor | null>(null);
  const [gone, setGone] = useState(false);
  const [big, setBig] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  useEffect(() => {
    let dead = false;
    const queue: FollowMsg[] = [];
    const apply = (m: FollowMsg) => {
      const c = code.current;
      if (!c) {
        queue.push(m);
        return;
      }
      if ("gone" in m) setGone(true);
      else if ("open" in m) {
        setBig(m.open.text === null);
        c.open(m.open.file, m.open.text ?? "", m.open.lang);
        setFile(m.open.file);
      } else if ("edit" in m) {
        // Something was missed: start over from what it shows now.
        if (m.edit.file === c.file && !c.edit(m.edit.changes)) fleet.refollow(host, id);
      } else if ("diagnostics" in m) {
        if (m.diagnostics.file === c.file) c.diagnostics(m.diagnostics.items);
      } else {
        if (m.file === c.file) c.cursor([m.line, m.col], m.sel);
        setAt(m);
      }
    };
    void import("./code").then(({ CodeView }) => {
      if (dead || !box.current) return;
      code.current = new CodeView(box.current);
      (window as unknown as { __follow?: CodeView }).__follow = code.current;
      for (const m of queue.splice(0)) apply(m);
    });
    const off = fleet.follow(host, id, apply);
    return () => {
      dead = true;
      off();
      code.current?.destroy();
      code.current = null;
    };
  }, [host, id]);

  // The debugger's line, from the editor's summary.
  const debug = p?.info.editor?.debug;
  useEffect(() => {
    code.current?.debug(debug?.state === "paused" && debug.file === file ? (debug.line ?? null) : null);
  }, [debug?.state, debug?.file, debug?.line, file]);

  const left = gone || (!p && !!file);
  const can = !!p && fleet.role(p) !== "viewer";
  const ed = p?.info.editor;
  const folder = p?.info.cwd ?? "";
  // The daemon's relative file goes through links (macOS /var is /private/var).
  const known = p?.info.file;
  const rel = file && folder && file.startsWith(folder + "/") ? file.slice(folder.length + 1) : file && known && file.endsWith("/" + known) ? known : file;
  const others = Math.max(0, (ed?.followers ?? 1) - 1);
  const paused = ed?.debug?.state === "paused";

  const act = async (body: ActRequest) => {
    try {
      const res = await fleet.request(host, "POST", "/api/attention/act", body);
      if (!res.ok) setErr((await res.json<Partial<ActResponse> & { error?: string }>().catch(() => null))?.error ?? `couldn't (${res.status})`);
      else setErr(null);
    } catch (e) {
      setErr(String(e));
    }
  };

  return (
    <div class="follow" data-follow={pkey} role="dialog" aria-label={`Following ${appName(ed?.app)}`}>
      <header>
        <div class="follow-title">
          <b>{appName(ed?.app)}</b>
          <span>
            {host}
            {ed?.remote ? ` · ${ed.remote}` : ""}
            {p?.info.project ? ` · ${p.info.project.name}` : ""}
          </span>
        </div>
        <div class="follow-file" data-follow-file={rel ?? ""}>
          {rel ?? (left ? "" : "…")}
          {at && at.file === file ? <span class="follow-at">:{at.line}</span> : null}
          {ed && (ed.diag.e > 0 || ed.diag.w > 0) && (
            <span class="follow-diag">
              {ed.diag.e > 0 && <i class="e">{ed.diag.e} errors</i>}
              {ed.diag.w > 0 && <i class="w">{ed.diag.w} warnings</i>}
            </span>
          )}
        </div>
        <div class="follow-tools">
          {paused && can && (
            <button class="pri" data-continue onClick={() => void act({ action: "continue", pane: id })}>
              Continue
            </button>
          )}
          {file && p && (
            <button data-open-here onClick={(e) => openMenu(e as unknown as MouseEvent, openHere(fleet, p, file, at, back, setErr))}>
              Open here ▾
            </button>
          )}
          {file && p && claudes(fleet, host).length > 0 && can && (
            <button data-mention onClick={(e) => openMenu(e as unknown as MouseEvent, mention(fleet, host, file, at, setErr))}>
              Ask Claude ▾
            </button>
          )}
          <button class="ghost" data-follow-close title="Stop following" onClick={close}>
            ✕
          </button>
        </div>
      </header>
      {paused && <div class="follow-note paused">Paused{ed?.debug?.line ? ` at line ${ed.debug.line}` : ""}{ed?.debug?.reason ? ` (${ed.debug.reason})` : ""}</div>}
      {big && <div class="follow-note">This file is too big to follow here: the cursor is at line {at?.line ?? "?"}.</div>}
      {left && <div class="follow-note gone">The editor left the swarm.</div>}
      {others > 0 && <div class="follow-note">{others === 1 ? "1 other person is" : `${others} others are`} following too.</div>}
      {err && <div class="follow-note error">{err}</div>}
      <div class="follow-code" ref={box} />
    </div>
  );
}

/** Terminals on `host` with Claude Code connected to illogical (M28). */
function claudes(fleet: Fleet, host: string): FleetPane[] {
  return fleet.panes.filter((x) => x.host === host && x.info.claude_ide && !x.stale);
}

function linesOf(at: Cursor | null): [number, number] {
  if (at?.sel) return [Math.min(at.sel[0], at.sel[2]), Math.max(at.sel[0], at.sel[2])];
  return [at?.line ?? 1, at?.line ?? 1];
}

/** "Ask Claude about these lines": `@file#L3-5` in a Claude Code's prompt. */
function mention(fleet: Fleet, host: string, file: string, at: Cursor | null, setErr: (e: string | null) => void): MenuItem[] {
  const [start, end] = linesOf(at);
  return claudes(fleet, host).map((c) => ({
    label: `%${c.id} ${c.info.title ?? c.info.current?.text ?? "Claude Code"} · lines ${start}${end > start ? `–${end}` : ""}`,
    run: async () => {
      const res = await fleet.request(host, "POST", "/api/ide/mention", { pane: c.id, file, start, end });
      setErr(res.ok ? null : ((await res.json<{ error?: string }>().catch(() => null))?.error ?? `couldn't (${res.status})`));
    },
  }));
}

/** Open the same file and line in your own editor: VS Code or Cursor here,
 * or over SSH to the editor's machine, or an editor block (M27) there. */
export function openHere(
  fleet: Fleet,
  p: FleetPane,
  file: string,
  at: { line: number; col: number } | null,
  back: () => void,
  setErr: (e: string | null) => void,
): MenuItem[] {
  const pos = at ? `:${at.line}:${at.col + 1}` : "";
  const ed = p.info.editor;
  const ssh = ed?.authority?.startsWith("ssh-remote+") ? ed.authority : `ssh-remote+${ed?.hostname ?? p.host}`;
  const enc = (s: string) => s.split("/").map(encodeURIComponent).join("/");
  const go = (url: string) => () => {
    location.href = url;
  };
  const items: MenuItem[] = [
    { label: `VS Code over SSH (${ssh.slice("ssh-remote+".length)})`, run: go(`vscode://vscode-remote/${ssh}${enc(file)}${pos}`) },
    { label: `Cursor over SSH (${ssh.slice("ssh-remote+".length)})`, run: go(`cursor://vscode-remote/${ssh}${enc(file)}${pos}`) },
    { label: "VS Code on this computer", run: go(`vscode://file${enc(file)}${pos}`) },
    { label: "Cursor on this computer", run: go(`cursor://file${enc(file)}${pos}`) },
  ];
  // An editor block there: the owner's, like ports.
  if (fleet.role(p) === "owner" && !p.stale && !ed?.remote?.startsWith("dev-container")) {
    items.push({
      label: `VS Code in illogical (on ${p.host})`,
      run: async () => {
        const res = await fleet.request(p.host, "POST", "/api/blocks", { type: "editor", config: { path: file, line: at?.line ?? null } } satisfies OpenRequest);
        const v = await res.json<Partial<OpenResponse> & { error?: string }>().catch(() => null);
        if (!res.ok || v?.block === undefined) return setErr(v?.error ?? `couldn't (${res.status})`);
        back();
        fleet.open(p.host, v.block);
      },
    });
  }
  return items;
}
