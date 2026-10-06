// #335: Claude Code (or Codex) is on this machine, and its ACP adapter
// isn't installed or is older than the daemon's pin, so agent panes won't
// start (or run an old one). Said once, as a line under the top bar, with
// Set it up…, which opens Getting started's Agents step (its one click,
// "Use Claude Code with illogical"). Only the owner's own daemon is asked
// (`/api/setup?part=agents`), once per page; each adapter's pin is said
// once per browser, so a release that moves the pin says it again.

import { useEffect, useState } from "preact/hooks";
import type { Client } from "../client";
import { adapterReady, type Adapter } from "./adapter";
import { openGettingStarted } from "./welcome";

const SAID_KEY = "illogical.agents-nudge";

/** `GET /api/setup?part=agents`. */
interface AgentsSetup {
  claude: { installed: boolean; tools: boolean };
  adapters?: Adapter[];
}

function said(): string[] {
  try {
    return JSON.parse(localStorage.getItem(SAID_KEY) ?? "[]") as string[];
  } catch {
    return [];
  }
}

function remember(keys: string[]) {
  try {
    localStorage.setItem(SAID_KEY, JSON.stringify([...new Set([...said(), ...keys])]));
  } catch {
    // Nothing to remember it in: said again next time.
  }
}

const keyOf = (a: Adapter) => `${a.kind}@${a.pinned}`;

/** The adapters worth a word: their agent is here, and they aren't. */
export function nudged(s: AgentsSetup | null): Adapter[] {
  return (s?.adapters ?? []).filter((a) => a.found && !adapterReady(a));
}

function line(a: Adapter, tools: boolean): string {
  const mcp = a.kind === "claude" && !tools ? ", and illogical's MCP server lets it start its helpers as panes" : "";
  if (a.state === "installed") return `${a.label}'s adapter here is ${a.version}, older than this illogical's (${a.pinned})${mcp}.`;
  if (a.state === "no_node") return `${a.label} is on this machine, but agent panes can't run it: ${a.why ?? `its adapter needs Node ${a.node_major}+`}.`;
  return `${a.label} is on this machine, but agent panes need its adapter${mcp}.`;
}

export function AgentsNudge({ client }: { client: Client }) {
  const mine = !client.e2e && !client.state?.roles && !navigator.webdriver;
  const [setup, setSetup] = useState<AgentsSetup | null>(null);
  const [shown, setShown] = useState<Adapter[]>([]);
  useEffect(() => {
    if (!mine) return;
    fetch("/api/setup?part=agents")
      .then((r) => (r.ok ? (r.json() as Promise<AgentsSetup>) : null))
      .then((s) => {
        const before = said();
        const fresh = nudged(s).filter((a) => !before.includes(keyOf(a)));
        if (!fresh.length) return;
        // Once: shown on this page until acted on, not on the next.
        remember(fresh.map(keyOf));
        setSetup(s);
        setShown(fresh);
      })
      .catch(() => {});
  }, [mine]);
  if (!shown.length || !setup) return null;
  const tools = setup.claude.tools;
  return (
    <div class="agents-nudge" role="status" data-agents-nudge={shown.map((a) => a.kind).join(" ")}>
      <p>
        {shown.map((a) => (
          <span key={a.kind}>{line(a, tools)} </span>
        ))}
      </p>
      <div class="agents-nudge-actions">
        <button
          class="primary"
          data-agents-nudge-setup
          onClick={() => {
            setShown([]);
            openGettingStarted("agents", client);
          }}
        >
          Set it up…
        </button>
        <button onClick={() => setShown([])} data-agents-nudge-hide>
          Not now
        </button>
      </div>
    </div>
  );
}
