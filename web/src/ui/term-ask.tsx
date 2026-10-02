// Claude Code in a terminal asking (M6c, M29): its AskUserQuestion (from
// `illogical ask`) or a tool permission prompt (from `illogical hook` on
// its PermissionRequest), as a card over the bottom of the pane, on every
// client. Answering the card answers Claude Code; the terminal can still
// answer, and then the card closes by itself. Anyone who may edit the
// session can answer; viewers see the card and who answered, without
// buttons. Teammates who have the pane open show on the card, so two
// people don't answer differently (the first answer wins). Once answered,
// the card says who did, and offers to send the agent a follow-up.

import { useState } from "preact/hooks";
import type { Client } from "../client";
import type { PaneId } from "../proto";
import { AskCard, type Answered, type Ask, type Suggestion } from "../blocks/ask";
import { askText } from "./menu";
import { Avatar } from "./people";

/** "14:02". */
export function clock(ms: number): string {
  return new Date(ms).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

/** "Allowed by Sam, 14:02". */
export function answeredLine(a: Answered): string {
  const how = a.how.charAt(0).toUpperCase() + a.how.slice(1);
  return a.who === "terminal" ? `${how}, ${clock(a.at_ms)}` : `${how} by ${a.name}, ${clock(a.at_ms)}`;
}

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
  const what = ask.kind === "permission" ? "Claude Code wants to use a tool" : "Claude Code asks";
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
        <button class="link" title="Look at the terminal; the question stays open" onClick={() => setHidden(true)}>
          Hide
        </button>
      </div>
      {ask.kind === "permission" ? (
        <PermissionCard client={client} id={id} ask={ask} can={can} />
      ) : !can ? (
        <div class="ask" data-ask={ask.id}>
          <p class="ask-message">{ask.questions?.[0]?.question ?? ask.message}</p>
          <p class="ask-viewer">You're watching this session: an editor answers it.</p>
        </div>
      ) : (
        <AskCard
          key={ask.id}
          ask={ask}
          actions={{
            answer: (content) => call("answer", { id: ask.id, content }),
            decline: () => call("decline", { id: ask.id }),
            terminal: () => call("terminal", { id: ask.id }),
          }}
        />
      )}
    </div>
  );
}

/** What a suggestion would keep, in words. */
function describe(s: Suggestion): string {
  if (s.type === "addRules" && s.rules?.length) return s.rules.map((r) => (r.ruleContent ? `${r.toolName}(${r.ruleContent})` : r.toolName)).join(", ");
  if (s.type === "addDirectories" && s.directories?.length) return `files in ${s.directories.join(", ")}`;
  if (s.type === "setMode" && s.mode) return `${s.mode} mode`;
  return s.type;
}

function PermissionCard({ client, id, ask, can }: { client: Client; id: PaneId; ask: Ask; can: boolean }) {
  const input = ask.input ?? {};
  const str = (k: string) => (typeof input[k] === "string" ? (input[k] as string) : undefined);
  const main = str("command") ?? str("file_path") ?? str("path") ?? str("url") ?? str("pattern") ?? ask.message;
  const diff = str("old_string") !== undefined || str("new_string") !== undefined;
  const act = (action: "allow" | "deny", extra: Record<string, unknown> = {}) =>
    void client.act({ action, pane: id, id: ask.id, ...extra });
  return (
    <div class="ask perm" role="alertdialog" aria-label={ask.message} data-ask={ask.id}>
      <div class="agent-perm-q">{ask.tool} wants to run</div>
      <pre class="agent-perm-cmd">{main}</pre>
      {str("description") && <p class="ask-desc">{str("description")}</p>}
      {diff && (
        <pre class="perm-diff">
          {(str("old_string") ?? "").split("\n").map((l, i) => (
            <div key={`o${i}`} class="del">
              - {l}
            </div>
          ))}
          {(str("new_string") ?? "").split("\n").map((l, i) => (
            <div key={`n${i}`} class="add">
              + {l}
            </div>
          ))}
        </pre>
      )}
      {str("content") !== undefined && <pre class="perm-diff">{str("content")!.slice(0, 2000)}</pre>}
      {can ? (
        <div class="agent-perm-buttons">
          <button class="primary" onClick={() => act("allow")}>
            Allow
          </button>
          {(ask.suggestions ?? []).slice(0, 2).map((s, i) => (
            <button key={i} title={`Allow, and from now on: ${describe(s)}`} onClick={() => act("allow", { option: "always", suggestion: i })}>
              Always: {describe(s)}
            </button>
          ))}
          <button class="danger" onClick={() => act("deny")}>
            Deny
          </button>
          <button
            class="link"
            onClick={async () => {
              const message = await askText("Deny, and say why", "", "what Claude should do instead");
              if (message !== null) act("deny", { message });
            }}
          >
            Deny with a message…
          </button>
        </div>
      ) : (
        <p class="ask-viewer">You're watching this session: an editor answers it.</p>
      )}
    </div>
  );
}

/** After a card is answered (M29): who answered it, and a box for the
 * agent's next instruction, for whoever may drive the pane. */
export function TermAnswered({ client, id, answered }: { client: Client; id: PaneId; answered: Answered }) {
  const [closed, setClosed] = useState<string | null>(null);
  const [text, setText] = useState("");
  const [needTrust, setNeedTrust] = useState<string | null>(null);
  const [sent, setSent] = useState<string | null>(null);
  if (closed === answered.id + answered.at_ms) return null;
  const can = mayAnswer(client, id);
  const owner = needTrust ?? "the owner";
  const send = async () => {
    const t = text.trim();
    if (!t) return;
    try {
      const res = await client.request("POST", `/api/panes/${id}/followup`, { text: t });
      if (res.ok) {
        const body = await res.json<{ delivered?: boolean }>();
        setText("");
        setNeedTrust(null);
        setSent(body.delivered ? "Sent." : "Queued: it goes in when the agent is next ready.");
        return;
      }
      const err = (await res.json<{ error?: string }>().catch(() => null))?.error ?? `couldn't send it (${res.status})`;
      // Someone's own machine (M14): its owner trusts you first.
      const mine = /runs on (.+)'s own machine/.exec(err);
      if (res.status === 403 && mine) setNeedTrust(mine[1]);
      else client.toast(err);
    } catch {
      client.toast("couldn't send it");
    }
  };
  return (
    <div class="pane-answered" onPointerDown={(e) => e.stopPropagation()} data-answered={answered.id}>
      <div class="pane-ask-bar">
        <span class="answered-by">{answeredLine(answered)}</span>
        <button class="link" title="Close" onClick={() => setClosed(answered.id + answered.at_ms)}>
          ✕
        </button>
      </div>
      <div class="answered-what">{answered.headline}</div>
      {can && (
        <form
          class="followup"
          onSubmit={(e) => {
            e.preventDefault();
            void send();
          }}
        >
          <input
            value={text}
            placeholder="Send a follow-up"
            aria-label="Send a follow-up"
            onInput={(e) => setText((e.target as HTMLInputElement).value)}
          />
          <button class="primary" type="submit" disabled={!text.trim()}>
            Send
          </button>
        </form>
      )}
      {needTrust && (
        <div class="followup-trust">
          This runs on {owner}'s own machine.{" "}
          <button
            class="link"
            onClick={() => {
              client.paneOp(id, { op: "request_trust" });
              setNeedTrust(null);
              setSent(`Asked ${owner}. Send it again once they let you.`);
            }}
          >
            Ask {owner} for 30 minutes
          </button>
        </div>
      )}
      {sent && <div class="followup-sent">{sent}</div>}
    </div>
  );
}
