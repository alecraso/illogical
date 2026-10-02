// Diff blocks (M11): what changed in a git repository on the block's
// machine, read-only. A file list with +/− first; a file opens to its
// unified hunks (highlighted with the follow view's languages and colours),
// and tapping a line opens a file block there. Everything drawn comes from
// the daemon's state, so a shared session's viewers see the same; only
// editors open files and change what's open.

import { render } from "preact";
import { useEffect, useMemo, useState } from "preact/hooks";
import type { Client } from "../client";
import type { PaneId } from "../proto";
import { registerBlock, type BlockView } from "./view";
import type { Span } from "../swarm/code";

type Line = [" " | "+" | "-" | "\\", number, number, string];

export interface DiffFile {
  path: string;
  old?: string;
  status: "modified" | "added" | "deleted" | "renamed" | "untracked" | "mode";
  add: number;
  del: number;
  binary?: boolean;
  big?: boolean;
  open?: boolean;
  hunks?: { at: string; lines: Line[] }[];
}

export interface DiffState {
  repo: string | null;
  name: string | null;
  rev_a: string | null;
  rev_b: string | null;
  against: string;
  files: DiffFile[];
  add: number;
  del: number;
  truncated: boolean;
  loading: boolean;
  error: string | null;
  watching: boolean;
  updated_ms: number;
}

/** What changed where `from` runs (its repository, on its machine), in a
 * diff block beside it. */
export function openChanges(client: Client, from: PaneId) {
  void client.openBlock({ type: "diff", config: {}, from_pane: from, split: from }, "couldn't show the changes");
}

/** The file block each diff block last opened, to point it elsewhere
 * rather than open another. */
const lastFile = new Map<PaneId, PaneId>();

/** A file at a line, beside the diff block `from`, on its machine. */
export async function openFile(client: Client, from: PaneId, path: string, line: number | null) {
  const prev = lastFile.get(from);
  if (prev !== undefined && client.info(prev)?.type === "file") {
    if (await client.api(`/api/blocks/${prev}/call/open`, { path, line }, "couldn't open that file")) {
      client.setActive(prev);
      return;
    }
  }
  const block = await client.openBlock({ type: "file", config: { path, line }, from_pane: from, split: from }, "couldn't open that file");
  if (block !== null) lastFile.set(from, block);
}

const LETTER: Record<DiffFile["status"], string> = { modified: "M", added: "A", deleted: "D", renamed: "R", untracked: "?", mode: "m" };

let highlighter: Promise<typeof import("../swarm/code")> | null = null;

/** The follow view's chunk, for highlighting (loaded once, when a file is
 * first opened). */
function useCode() {
  const [mod, setMod] = useState<typeof import("../swarm/code") | null>(null);
  useEffect(() => {
    highlighter ??= import("../swarm/code");
    let live = true;
    void highlighter.then((m) => live && setMod(m));
    return () => {
      live = false;
    };
  }, []);
  return mod;
}

function Hunks({ f, can, open }: { f: DiffFile; can: boolean; open: (line: number) => void }) {
  const code = useCode();
  const lit = useMemo(() => (code && f.hunks ? f.hunks.map((h) => code.highlightLines(f.path, h.lines.map((l) => l[3]))) : null), [code, f.hunks, f.path]);
  if (f.binary) return <div class="diff-note">Binary file</div>;
  if (f.big) return <div class="diff-note">Too big to show here: open the file.</div>;
  if (!f.hunks?.length) return <div class="diff-note">{f.status === "mode" ? "Only its mode changed." : "No changes to show."}</div>;
  const gone = f.status === "deleted";
  return (
    <div class="diff-hunks">
      {f.hunks.map((h, i) => (
        <div class="diff-hunk" key={`${i}:${h.at}`}>
          <div class="diff-at">{h.at}</div>
          {h.lines.map((l, j) => {
            const [k, o, n, text] = l;
            const spans: Span[] | undefined = lit?.[i]?.[j] ?? undefined;
            const kind = k === "+" ? "add" : k === "-" ? "del" : k === "\\" ? "eol" : "ctx";
            const at = k === "\\" ? null : n;
            const go = can && !gone && at !== null ? () => open(at) : undefined;
            return (
              <div key={j} class={`dl ${kind}${go ? " go" : ""}`} data-line={at ?? undefined} onClick={go} title={go ? `Open ${f.path} at line ${at}` : undefined}>
                <span class="ln">{k === "+" || k === "\\" ? "" : o}</span>
                <span class="ln">{k === "-" || k === "\\" ? "" : n}</span>
                <span class="mk">{k === "\\" ? "" : k}</span>
                <span class="code">{spans ? spans.map(([t, c], x) => (c ? <span key={x} class={c}>{t}</span> : t)) : text}</span>
              </div>
            );
          })}
        </div>
      ))}
    </div>
  );
}

function DiffBlock({ client, id, s }: { client: Client; id: PaneId; s: DiffState | null }) {
  if (!s) return <div class="browser-card dim">…</div>;
  const tab = client.tabOfPane(id);
  const session = tab ? client.sessionOfTab(tab.id) : undefined;
  const can = client.role(session ?? null) !== "viewer";
  const call = (method: string, args: unknown = {}) => void client.api(`/api/blocks/${id}/call/${method}`, args);
  const machine = client.machine(id);
  const where = (p: string) => (s.repo ? `${s.repo.replace(/\/$/, "")}/${p}` : p);
  return (
    <div class="review" data-diff={id}>
      <div class="review-bar">
        <b class="review-name">{s.name ?? "…"}</b>
        {machine && (
          <span class="host-tag" title={`on ${machine.name ?? machine.sprite}`}>
            VM
          </span>
        )}
        <span class="review-what" title={s.repo ?? ""}>
          {s.against}
        </span>
        <span class="adds">+{s.add}</span>
        <span class="dels">−{s.del}</span>
        <span class={`review-live ${s.watching ? "on" : ""}`} title={s.watching ? "Updates as files change" : "Not watching: nobody's looking"}>
          {s.watching ? "live" : "paused"}
        </span>
        {can && (
          <button title="Read it again" onClick={() => call("refresh")}>
            ↻
          </button>
        )}
      </div>
      <div class="review-body">
        {s.error ? (
          <div class="browser-card error">
            <p>Can't show the changes</p>
            <p class="dim">{s.error}</p>
          </div>
        ) : s.loading && !s.files.length ? (
          <div class="browser-card dim">Reading the diff…</div>
        ) : !s.files.length ? (
          <div class="browser-card dim">Nothing changed ({s.against}).</div>
        ) : (
          s.files.map((f) => {
            const slash = f.path.lastIndexOf("/");
            return (
              <div key={f.path} class={`diff-file${f.open ? " open" : ""}`} data-file={f.path}>
                <button class="diff-file-head" disabled={!can} aria-expanded={!!f.open} onClick={() => call("file", { path: f.path })}>
                  <span class={`diff-st st-${f.status}`} title={f.status}>
                    {LETTER[f.status]}
                  </span>
                  <span class="diff-path">
                    {f.old && <span class="dim">{f.old} → </span>}
                    <span class="dim">{f.path.slice(0, slash + 1)}</span>
                    {f.path.slice(slash + 1)}
                  </span>
                  {f.binary ? (
                    <span class="dim">binary</span>
                  ) : (
                    <>
                      <span class="adds">+{f.add}</span>
                      <span class="dels">−{f.del}</span>
                    </>
                  )}
                </button>
                {f.open && <Hunks f={f} can={can} open={(line) => void openFile(client, id, where(f.path), line)} />}
              </div>
            );
          })
        )}
        {s.truncated && <div class="diff-note">More changed than is shown here.</div>}
      </div>
    </div>
  );
}

/** The diff as text: the list, then the open files' hunks. */
function asText(s: DiffState): string {
  const out = [`${s.name ?? ""} ${s.against} +${s.add} -${s.del}`];
  for (const f of s.files) {
    out.push(`${LETTER[f.status]} ${f.path} +${f.add} -${f.del}`);
    for (const h of f.hunks ?? []) {
      out.push(h.at);
      for (const [k, , , t] of h.lines) out.push(`${k}${t}`);
    }
  }
  return out.join("\n");
}

registerBlock("diff", (client, id): BlockView => {
  const host = document.createElement("div");
  host.className = "block block-diff";
  let state: DiffState | null = null;
  const draw = () => render(<DiffBlock client={client} id={id} s={state} />, host);
  draw();
  return {
    host,
    setVisible: (v) => (host.style.display = v ? "" : "none"),
    update: (s) => {
      state = s as DiffState;
      draw();
    },
    title: () => `Changes: ${state?.name ?? "…"}`,
    text: () => (state ? asText(state) : ""),
    focus: () => host.querySelector<HTMLElement>("button")?.focus(),
    dispose: () => {
      lastFile.delete(id);
      render(null, host);
      host.remove();
    },
  };
});
