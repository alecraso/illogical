// An edit Claude Code proposes, waiting as a diff (M28: arugulad as its
// IDE). Beside the terminal it runs in and on the swarm's rail: accept it,
// change it first, or reject it. The terminal's own prompt still works;
// when it answers first, the card goes by itself.

import { useState } from "preact/hooks";
import type { DiffInfo } from "../proto";
import { VIEWER_NOTE } from "./answer-card";

export type DiffAct = (action: "accept" | "reject", extra?: { text?: string }) => Promise<void> | void;

/** The diff, coloured by line. */
export function DiffText({ text }: { text: string }) {
  return (
    <pre class="perm-diff diff-text">
      {text.split("\n").map((l, i) => (
        <div key={i} class={l.startsWith("@@") ? "hunk" : l.startsWith("+") ? "add" : l.startsWith("-") ? "del" : ""}>
          {l || " "}
        </div>
      ))}
    </pre>
  );
}

export function DiffCard({
  diff,
  can,
  act,
  full,
}: {
  diff: DiffInfo;
  can: boolean;
  act: DiffAct;
  /** The proposal whole, for changing it before accepting. */
  full: () => Promise<string | null>;
}) {
  const [editing, setEditing] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const run = async (action: "accept" | "reject", extra?: { text?: string }) => {
    setBusy(true);
    try {
      await act(action, extra);
    } finally {
      setBusy(false);
    }
  };
  return (
    <div class="ask perm diff-card" data-diff={diff.id} role="alertdialog" aria-label={`Claude Code wants to ${diff.new ? "create" : "edit"} ${diff.file}`}>
      <div class="agent-perm-q">
        Claude Code wants to {diff.new ? "create" : "edit"} <b>{diff.file}</b>{" "}
        <span class="diff-counts">
          <i class="add">+{diff.added}</i> <i class="del">−{diff.removed}</i>
        </span>
      </div>
      {editing === null ? (
        <DiffText text={diff.text} />
      ) : (
        <textarea class="diff-edit" data-diff-edit value={editing} spellcheck={false} onInput={(e) => setEditing((e.target as HTMLTextAreaElement).value)} />
      )}
      {!can ? (
        <p class="ask-viewer">{VIEWER_NOTE}</p>
      ) : editing === null ? (
        <div class="agent-perm-buttons">
          <button class="primary" disabled={busy} data-accept onClick={() => void run("accept")}>
            Accept
          </button>
          <button
            disabled={busy}
            data-change
            onClick={() =>
              void full().then((t) => {
                if (t !== null) setEditing(t);
              })
            }
          >
            Change…
          </button>
          <button disabled={busy} data-reject onClick={() => void run("reject")}>
            Reject
          </button>
        </div>
      ) : (
        <div class="agent-perm-buttons">
          <button class="primary" disabled={busy} data-accept-changed onClick={() => void run("accept", { text: editing })}>
            Accept with changes
          </button>
          <button disabled={busy} onClick={() => setEditing(null)}>
            Back
          </button>
        </div>
      )}
    </div>
  );
}
