// Invite blocks (#234): an agent's invites into the session, waiting for
// the owner. The oldest waiting one is the card over the block (the
// daemon's ask): edit the role, the note or drive trust and Invite, or
// Decline with a reason the agent is told. Only the session's owner can
// answer it; everyone else here sees it, without the buttons. Below, the
// drafts, and what became of each.

import { render } from "preact";
import type { Client } from "../client";
import type { PaneId } from "../proto";
import { registerBlock, type BlockView } from "./view";

interface Draft {
  id: string;
  by: string;
  from: PaneId;
  name: string;
  role: string;
  note: string;
  session_name: string;
  pane: PaneId;
  at_ms: number;
  status: "waiting" | "sent" | "declined" | "dropped" | "failed";
  settled_by?: string;
  reason?: string;
  delivery?: string;
  delivery_reason?: string;
  error?: string;
}

interface InviteState {
  drafter: string;
  drafts: Draft[];
  waiting: number;
}

function outcome(d: Draft): string {
  switch (d.status) {
    case "waiting":
      return "waits for you: it's on the card";
    case "sent":
      return `sent by ${d.settled_by ?? "the owner"}: ${d.delivery ?? "?"}${d.delivery_reason ? ` (${d.delivery_reason})` : ""}`;
    case "declined":
      return `declined by ${d.settled_by ?? "the owner"}${d.reason ? `: ${d.reason}` : ""}`;
    case "dropped":
      return "dropped: nobody answered";
    case "failed":
      return `failed: ${d.error ?? "?"}`;
  }
}

function InviteBlock({ id, s }: { id: PaneId; s: InviteState | null }) {
  const drafts = s?.drafts ?? [];
  return (
    <div class="review ws" data-invite-block={id}>
      <div class="review-bar">
        <span class="review-path">
          <b>Invites</b> <span class="dim">{s?.waiting ? `${s.waiting} waiting` : "none waiting"}</span>
        </span>
      </div>
      <div class="review-body ws-body">
        {drafts.length === 0 && <p class="dim">Nothing to invite.</p>}
        {drafts.map((d) => (
          <div key={d.id} class={`forge-draft ${d.status}`} data-draft={d.id} data-draft-status={d.status}>
            <div>
              <b>{d.by.replace(/^mcp:/, "")}</b> (pane %{d.from}) wants {d.name} ({d.role}) in {d.session_name} at %{d.pane}
            </div>
            <div class="forge-body">{d.note}</div>
            <div class="dim">{outcome(d)}</div>
          </div>
        ))}
      </div>
    </div>
  );
}

registerBlock("invite", (_client: Client, id): BlockView => {
  const host = document.createElement("div");
  host.className = "block block-invite";
  let state: InviteState | null = null;
  const draw = () => render(<InviteBlock id={id} s={state} />, host);
  draw();
  return {
    host,
    setVisible: (v) => (host.style.display = v ? "" : "none"),
    update: (s) => {
      state = s as InviteState;
      draw();
    },
    title: () => (state?.waiting ? `Invites (${state.waiting})` : "Invites"),
    text: () => (state?.drafts ?? []).map((d) => `${d.id} ${d.status}: ${d.name} (${d.role}): ${d.note}`).join("\n"),
    focus: () => host.querySelector<HTMLElement>("button")?.focus(),
    dispose: () => {
      render(null, host);
      host.remove();
    },
  };
});
