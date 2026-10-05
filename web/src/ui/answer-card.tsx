// What a card about an agent's question or approval shows, wherever it is
// (M29, shared with M26's rail): a permission request's tool, input and
// buttons, who answered, and the follow-up box. Beside a terminal it talks
// to the client shown (term-ask.tsx); on the swarm's rail, to whichever host
// the pane is on, through the fleet. Either way it only needs a way to make
// requests to that pane's daemon.

import { useState } from "preact/hooks";
import type { Answered, Ask, Suggestion } from "../blocks/ask";
import type { PaneOp } from "../proto";
import { askText } from "./menu";

/** A request to the daemon a pane is on. */
export type Requester = (method: string, path: string, body?: unknown) => Promise<{ ok: boolean; status: number; json<T = unknown>(): Promise<T> }>;

/** "14:02". */
export function clock(ms: number): string {
  return new Date(ms).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

/** "Allowed by Sam, 14:02". */
export function answeredLine(a: Answered): string {
  const how = a.how.charAt(0).toUpperCase() + a.how.slice(1);
  return a.who === "terminal" ? `${how}, ${clock(a.at_ms)}` : `${how} by ${a.name}, ${clock(a.at_ms)}`;
}

/** What one of Claude Code's suggestions would keep, in words. */
export function describe(s: Suggestion): string {
  if (s.type === "addRules" && s.rules?.length) return s.rules.map((r) => (r.ruleContent ? `${r.toolName}(${r.ruleContent})` : r.toolName)).join(", ");
  if (s.type === "addDirectories" && s.directories?.length) return `files in ${s.directories.join(", ")}`;
  if (s.type === "setMode" && s.mode) return `${s.mode} mode`;
  return s.type;
}

/** The line that says what a permission request would run or touch. */
export function permissionTarget(ask: Ask): string {
  const input = ask.input ?? {};
  const str = (k: string) => (typeof input[k] === "string" ? (input[k] as string) : undefined);
  return str("command") ?? str("file_path") ?? str("path") ?? str("url") ?? str("pattern") ?? ask.message;
}

/** A permission request's tool and input: the command, the file and its
 * diff. */
export function PermissionBody({ ask }: { ask: Ask }) {
  const input = ask.input ?? {};
  const str = (k: string) => (typeof input[k] === "string" ? (input[k] as string) : undefined);
  const diff = str("old_string") !== undefined || str("new_string") !== undefined;
  return (
    <>
      <div class="agent-perm-q">{ask.tool} wants to run</div>
      <pre class="agent-perm-cmd">{permissionTarget(ask)}</pre>
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
    </>
  );
}

/** Allow, Always (one of Claude's suggestions), Deny, Deny with a message. */
export function PermissionButtons({ ask, act }: { ask: Ask; act: (action: "allow" | "deny", extra?: Record<string, unknown>) => void }) {
  return (
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
  );
}

/** An answered card (with its follow-up box) stays this long. */
export const ANSWERED_MS = 60_000;

export const VIEWER_NOTE = "You're watching this session: an editor answers it.";

/** The agent's next instruction (M29), for whoever may drive the pane. On
 * someone's own machine a 403 offers to ask them for trust instead. */
export function FollowUpBox({
  pane,
  request,
  paneOp,
  toast,
}: {
  pane: number;
  request: Requester;
  paneOp: (op: PaneOp) => void;
  toast: (m: string) => void;
}) {
  const [text, setText] = useState("");
  const [needTrust, setNeedTrust] = useState<string | null>(null);
  const [sent, setSent] = useState<string | null>(null);
  const send = async () => {
    const t = text.trim();
    if (!t) return;
    try {
      const res = await request("POST", `/api/panes/${pane}/followup`, { text: t });
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
      else toast(err);
    } catch {
      toast("couldn't send it");
    }
  };
  return (
    <>
      <form
        class="followup"
        onSubmit={(e) => {
          e.preventDefault();
          void send();
        }}
      >
        <input value={text} placeholder="Send a follow-up" aria-label="Send a follow-up" onInput={(e) => setText((e.target as HTMLInputElement).value)} />
        <button class="primary" type="submit" disabled={!text.trim()}>
          Send
        </button>
      </form>
      {needTrust && (
        <div class="followup-trust">
          This runs on {needTrust}'s own machine.{" "}
          <button
            class="link"
            onClick={() => {
              paneOp({ op: "request_trust" });
              setSent(`Asked ${needTrust}. Send it again once they let you.`);
              setNeedTrust(null);
            }}
          >
            Ask {needTrust} for 30 minutes
          </button>
        </div>
      )}
      {sent && <div class="followup-sent">{sent}</div>}
    </>
  );
}
