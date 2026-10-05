// Claude Code in a terminal asking (M6c, M29): its AskUserQuestion (from
// `illogical ask`) or a tool permission prompt (from `illogical hook` on
// its PermissionRequest), as a card over the bottom of the pane, on every
// client. Answering the card answers Claude Code; the terminal can still
// answer, and then the card closes by itself. Anyone who may edit the
// session can answer; viewers see the card and who answered, without
// buttons. Teammates who have the pane open show on the card, so two
// people don't answer differently (the first answer wins). Once answered,
// the card says who did, and offers to send the agent a follow-up.

import { useEffect, useState } from "preact/hooks";
import { openMenu } from "./menu";
import type { Client } from "../client";
import type { DiffInfo, PaneId } from "../proto";
import { DiffCard } from "./diff-card";
import { AskCard, type Answered, type Ask } from "../blocks/ask";
import { ANSWERED_MS, answeredLine, FollowUpBox, PermissionBody, PermissionButtons, VIEWER_NOTE } from "./answer-card";
import { Avatar } from "./people";

export { answeredLine, clock } from "./answer-card";

/** Whether this client's person may answer in a pane's session. */
export function mayAnswer(client: Client, id: PaneId): boolean {
  const tab = client.tabOfPane(id);
  return client.role(tab ? (client.sessionOfTab(tab.id) ?? null) : null) !== "viewer";
}

/** Teammates looking at a pane, other than this client's person. */
function Watching({ client, id }: { client: Client; id: PaneId }) {
  const me = client.me();
  const seen = new Set<string>();
  const others = (client.state?.presence ?? []).filter((p) => p.pane === id && p.who !== me && !seen.has(p.who) && seen.add(p.who));
  if (!others.length) return null;
  return (
    <span class="ask-watching" title={`${others.map((p) => p.name).join(", ")} ${others.length > 1 ? "have" : "has"} this open`}>
      {others.map((p) => (
        <Avatar key={p.who} p={p} />
      ))}
    </span>
  );
}

export function TermAsk({ client, id, ask }: { client: Client; id: PaneId; ask: Ask }) {
  const [hidden, setHidden] = useState(false);
  const call = (method: string, args: unknown) => void client.api(`/api/blocks/${id}/call/${method}`, args, `couldn't ${method}`);
  const can = mayAnswer(client, id);
  // M35: a question raised on a block names who asks ("hud asks").
  const who = ask.agent ?? "Claude Code";
  const what = ask.kind === "permission" ? `${who} wants to use a tool` : `${who} asks`;
  // Only Claude Code in a terminal has a picker of its own to fall back to.
  const terminal = ask.source === "hook";
  if (hidden) {
    return (
      <button class="pane-ask-pill" onPointerDown={(e) => e.stopPropagation()} onClick={() => setHidden(false)}>
        {what}…
      </button>
    );
  }
  return (
    <div class="pane-ask" onPointerDown={(e) => e.stopPropagation()} onContextMenu={(e) => e.stopPropagation()}>
      <div class="pane-ask-bar">
        <span>{what}</span>
        <Watching client={client} id={id} />
        <button class="link" title={terminal ? "Look at the terminal; the question stays open" : "Look at the page; the question stays open"} onClick={() => setHidden(true)}>
          Hide
        </button>
      </div>
      {ask.kind === "permission" ? (
        <PermissionCard client={client} id={id} ask={ask} can={can} />
      ) : !can ? (
        <div class="ask" data-ask={ask.id}>
          <p class="ask-message">{ask.questions?.[0]?.question ?? ask.message}</p>
          <p class="ask-viewer">{VIEWER_NOTE}</p>
        </div>
      ) : (
        <AskCard
          key={ask.id}
          ask={ask}
          actions={{
            answer: (content) => call("answer", { id: ask.id, content }),
            decline: () => call("decline", { id: ask.id }),
            ...(terminal ? { terminal: () => call("terminal", { id: ask.id }) } : {}),
          }}
        />
      )}
    </div>
  );
}

/** An edit Claude Code proposes here, as a diff (M28): this pane's agent
 * card while it waits. */
export function TermDiff({ client, id, diff }: { client: Client; id: PaneId; diff: DiffInfo }) {
  const [hidden, setHidden] = useState(false);
  // Another IDE registered with Claude Code (VS Code with its extension):
  // the owner can send diffs there from now on.
  const [others, setOthers] = useState<string[]>([]);
  const owner = !client.state?.roles;
  useEffect(() => {
    if (!owner) return;
    void client
      .request("GET", "/api/ide")
      .then((r) => (r.ok ? r.json<{ others?: { name: string; alive: boolean }[] }>() : null))
      .then((v) => setOthers([...new Set((v?.others ?? []).filter((o) => o.alive).map((o) => o.name))]))
      .catch(() => {});
  }, [owner]);
  if (hidden) {
    return (
      <button class="pane-ask-pill" onPointerDown={(e) => e.stopPropagation()} onClick={() => setHidden(false)}>
        Claude Code wants to edit…
      </button>
    );
  }
  return (
    <div class="pane-ask" onPointerDown={(e) => e.stopPropagation()} onContextMenu={(e) => e.stopPropagation()}>
      <div class="pane-ask-bar">
        <span>Claude Code wants to edit a file</span>
        <Watching client={client} id={id} />
        {others.length > 0 && (
          <button
            class="link"
            data-diffs-to
            title="Which IDE shows Claude Code's diffs"
            onClick={(e) =>
              openMenu(
                e,
                others.map((name) => ({
                  label: `Send diffs to ${name} from now on`,
                  run: () => void client.request("PUT", "/api/ide", { diffs: name }).then(() => client.toast(`Claude Code's diffs go to ${name} now`)),
                })),
              )
            }
          >
            Diffs here ▾
          </button>
        )}
        <button class="link" title="Look at the terminal; the edit stays open" onClick={() => setHidden(true)}>
          Hide
        </button>
      </div>
      <DiffCard
        key={diff.id}
        diff={diff}
        can={mayAnswer(client, id)}
        act={async (action, extra) => {
          await client.act({ action, pane: id, id: diff.id, ...extra });
        }}
        full={async () => {
          const res = await client.request("GET", `/api/panes/${id}/diff`);
          return res.ok ? (await res.json<{ new: string }>()).new : null;
        }}
      />
    </div>
  );
}

function PermissionCard({ client, id, ask, can }: { client: Client; id: PaneId; ask: Ask; can: boolean }) {
  return (
    <div class="ask perm" role="alertdialog" aria-label={ask.message} data-ask={ask.id}>
      <PermissionBody ask={ask} />
      {can ? (
        <PermissionButtons ask={ask} act={(action, extra = {}) => void client.act({ action, pane: id, id: ask.id, ...extra })} />
      ) : (
        <p class="ask-viewer">{VIEWER_NOTE}</p>
      )}
    </div>
  );
}

/** Answered cards closed, or seen first, on this page: by pane and answer,
 * so leaving a tab and coming back doesn't bring one back. */
const closedAnswers = new Set<string>();
const firstSeen = new Map<string, number>();

/** After a card is answered (M29): who answered it, and a box for the
 * agent's next instruction, for whoever may drive the pane. It goes by
 * itself after a minute, as on the swarm's rail; the terminal is there for
 * anything later. */
export function TermAnswered({ client, id, answered }: { client: Client; id: PaneId; answered: Answered }) {
  const key = `${id}:${answered.id}:${answered.at_ms}`;
  const [, rerender] = useState(0);
  if (!firstSeen.has(key)) firstSeen.set(key, Date.now());
  // The daemon's clock, or this page's if they disagree: whichever is sooner.
  const until = Math.min(answered.at_ms, firstSeen.get(key)!) + ANSWERED_MS;
  const gone = closedAnswers.has(key) || Date.now() >= until;
  useEffect(() => {
    if (gone) return;
    const t = setTimeout(() => rerender((n) => n + 1), until - Date.now());
    return () => clearTimeout(t);
  }, [key, gone]);
  if (gone) return null;
  const can = mayAnswer(client, id);
  return (
    <div class="pane-answered" onPointerDown={(e) => e.stopPropagation()} data-answered={answered.id}>
      <div class="pane-ask-bar">
        <span class="answered-by">{answeredLine(answered)}</span>
        <button
          class="link"
          title="Close"
          onClick={() => {
            closedAnswers.add(key);
            rerender((n) => n + 1);
          }}
        >
          ✕
        </button>
      </div>
      <div class="answered-what">{answered.headline}</div>
      {can && (
        <FollowUpBox
          pane={id}
          request={(m, p, b) => client.request(m, p, b)}
          paneOp={(op) => client.paneOp(id, op)}
          toast={(m) => client.toast(m)}
        />
      )}
    </div>
  );
}
