// Starting an agent block: which agent, where it runs, and the first prompt.
// Opened from a pane's menu and the phone's sheet.

import { useEffect, useLayoutEffect, useRef, useState } from "preact/hooks";
import type { Client } from "../client";
import type { PaneId } from "../proto";
import { AdapterHelp, installAdapter, loadAdapters, type Adapter } from "./adapter";
import { pickConversation } from "./conversations";

type Kind = "claude" | "codex" | "fountain" | "acp";

export interface AgentWhere {
  /** Split this block (desktop), else a new tab in `session`. */
  split?: PaneId;
  session?: number;
  /** Where the request comes from: its directory is the default. */
  from?: PaneId;
}

let open: { client: Client; where: AgentWhere } | null = null;
const listeners = new Set<() => void>();
const changed = () => listeners.forEach((fn) => fn());

/** Show the "start an agent" dialog. */
export function startAgent(client: Client, where: AgentWhere) {
  open = { client, where };
  changed();
}

export function AgentDialogLayer() {
  const [, setTick] = useState(0);
  useEffect(() => {
    const fn = () => setTick((t) => t + 1);
    listeners.add(fn);
    return () => {
      listeners.delete(fn);
    };
  }, []);
  if (!open) return null;
  const { client, where } = open;
  return (
    <AgentDialog
      client={client}
      where={where}
      close={() => {
        open = null;
        changed();
      }}
    />
  );
}

const remembered = () => {
  try {
    return JSON.parse(localStorage.getItem("arugula.agent") ?? "{}") as { kind?: Kind; fountain?: string; acp?: string; model?: string };
  } catch {
    return {};
  }
};

function AgentDialog({ client, where, close }: { client: Client; where: AgentWhere; close: () => void }) {
  const last = remembered();
  // Fountain agents need a Fountain login here, VMs wisp (#180).
  const hasFountain = client.has("fountain");
  const [kind, setKind] = useState<Kind>(last.kind === "fountain" && !hasFountain ? "claude" : (last.kind ?? "claude"));
  const [fountain, setFountain] = useState(last.fountain ?? "");
  const [acp, setAcp] = useState(last.acp ?? "");
  const [model, setModel] = useState(last.model ?? "");
  const [vm, setVm] = useState(false);
  const [cwd, setCwd] = useState((where.from !== undefined && client.cwd(where.from)) || "");
  const [prompt, setPrompt] = useState("");
  const promptRef = useRef<HTMLTextAreaElement>(null);
  useLayoutEffect(() => promptRef.current?.focus(), []);
  const canVm = kind !== "fountain" && client.has("vms");
  // #111: whether this agent's adapter is installed here (a VM installs
  // its own).
  const [adapters, setAdapters] = useState<Adapter[] | null>(null);
  useEffect(() => {
    void loadAdapters(client).then(setAdapters);
  }, [client]);
  const adapter = vm && canVm ? undefined : adapters?.find((a) => a.kind === kind);
  const blocked = !!adapter && adapter.state !== "installed";

  const remember = () => {
    try {
      localStorage.setItem("arugula.agent", JSON.stringify({ kind, fountain, acp, model }));
    } catch {
      // private mode: nothing remembered
    }
  };

  const submit = async () => {
    if (blocked) return;
    remember();
    const config: Record<string, unknown> = { agent: kind };
    if (kind === "fountain") config.fountain_agent = fountain.trim();
    if (kind === "acp") config.command = acp.trim();
    if (model.trim()) config.model = model.trim();
    if (!(vm && canVm) && cwd.trim()) config.cwd = cwd.trim();
    if (prompt.trim()) config.prompt = prompt.trim();
    close();
    await client.newAgent({ config, vm: vm && canVm, split: where.split, session: where.session, from: where.from });
  };

  return (
    <div class="prompt-backdrop" role="dialog" aria-label="Start an agent" onPointerDown={(e) => e.target === e.currentTarget && close()}>
      <form
        class="prompt agent-dialog"
        aria-label="Start an agent"
        onSubmit={(e) => {
          e.preventDefault();
          void submit();
        }}
        onKeyDown={(e) => e.key === "Escape" && close()}
      >
        <h2>Start an agent</h2>
        <p class="hint">
          Or{" "}
          <button
            type="button"
            class="link"
            onClick={() => {
              close();
              pickConversation(client, { split: where.split, session: where.session, cwd: cwd || undefined });
            }}
          >
            pick up a Claude Code conversation
          </button>{" "}
          from a terminal or the desktop app.
        </p>
        <label>
          Agent
          <select name="agent" value={kind} onChange={(e) => setKind((e.currentTarget as HTMLSelectElement).value as Kind)}>
            <option value="claude">Claude Code</option>
            <option value="codex">Codex</option>
            {hasFountain && <option value="fountain">Fountain agent</option>}
            <option value="acp">Another ACP agent…</option>
          </select>
        </label>
        {kind === "fountain" && (
          <label>
            Fountain agent (name or id)
            <input name="fountain" value={fountain} required onInput={(e) => setFountain(e.currentTarget.value)} />
          </label>
        )}
        {kind === "acp" && (
          <label>
            Command
            <input name="acp" value={acp} required placeholder="gemini --acp" onInput={(e) => setAcp(e.currentTarget.value)} />
          </label>
        )}
        {canVm && (
          <label class="check">
            <input type="checkbox" name="vm" checked={vm} onChange={(e) => setVm(e.currentTarget.checked)} />
            On a new throwaway VM
          </label>
        )}
        {vm && kind === "claude" && (
          <p class="hint">Claude Code in a VM uses the token in ~/.config/arugula/claude-oauth-token (from `claude setup-token`).</p>
        )}
        {adapter && (
          <AdapterHelp
            client={client}
            a={adapter}
            then="start the agent once it's done."
            install={() => {
              remember();
              close();
              void installAdapter(client, adapter.kind, where);
            }}
          />
        )}
        {!(vm && canVm) && kind !== "fountain" && (
          <label>
            Working directory
            <input name="cwd" value={cwd} placeholder="home" onInput={(e) => setCwd(e.currentTarget.value)} />
          </label>
        )}
        {kind !== "fountain" && (
          <label>
            Model
            <input name="model" value={model} placeholder="the agent's default (e.g. haiku)" onInput={(e) => setModel(e.currentTarget.value)} />
          </label>
        )}
        <label>
          Prompt
          <textarea name="prompt" ref={promptRef} rows={4} value={prompt} placeholder="What should it do?" onInput={(e) => setPrompt(e.currentTarget.value)} />
        </label>
        <div class="prompt-buttons">
          <button type="button" onClick={close}>
            Cancel
          </button>
          <button type="submit" class="primary" disabled={blocked}>
            Start
          </button>
        </div>
      </form>
    </div>
  );
}
