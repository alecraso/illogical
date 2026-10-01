// Claude Code in a terminal asking a question (M6c): its AskUserQuestion,
// sent by `illogical ask` from Claude Code's PreToolUse hook, as a card over
// the bottom of the pane, on every client. Answering it answers Claude Code,
// which then never shows its picker; "Answer in terminal" gives the picker
// back. It can be tucked away to look at the terminal underneath.

import { useState } from "preact/hooks";
import type { Client } from "../client";
import type { PaneId } from "../proto";
import { AskCard, type Ask } from "../blocks/ask";

export function TermAsk({ client, id, ask }: { client: Client; id: PaneId; ask: Ask }) {
  const [hidden, setHidden] = useState(false);
  const call = (method: string, args: unknown) => void client.api(`/api/blocks/${id}/call/${method}`, args, `couldn't ${method}`);
  if (hidden) {
    return (
      <button class="pane-ask-pill" onPointerDown={(e) => e.stopPropagation()} onClick={() => setHidden(false)}>
        Claude Code asks…
      </button>
    );
  }
  return (
    <div class="pane-ask" onPointerDown={(e) => e.stopPropagation()} onContextMenu={(e) => e.stopPropagation()}>
      <div class="pane-ask-bar">
        <span>Claude Code asks</span>
        <button class="link" title="Look at the terminal; the question stays open" onClick={() => setHidden(true)}>
          Hide
        </button>
      </div>
      <AskCard
        key={ask.id}
        ask={ask}
        actions={{
          answer: (content) => call("answer", { id: ask.id, content }),
          decline: () => call("decline", { id: ask.id }),
          terminal: () => call("terminal", { id: ask.id }),
        }}
      />
    </div>
  );
}
