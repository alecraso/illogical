// An agent whose adapter isn't installed here, or has no Node to run on
// (#111): the command that installs it, copyable, and Install, which runs
// it in a pane you can watch. In the Start an agent dialog, the block and
// Getting started's Agents step, which also says when one is out of date
// (#335): older than the daemon's pin, and the same install updates it.

import type { Client } from "../client";
import type { PaneId } from "../proto";
import { CopyText } from "./copy";

/** `GET /api/agents/adapters`: one per agent that runs through npm. */
export interface Adapter {
  kind: string;
  label: string;
  package: string;
  pinned: string;
  /** The `npm install` line, made from the daemon's pins. */
  npm: string;
  state: "installed" | "missing" | "no_node";
  version?: string | null;
  /** Installed, older than `pinned` (#335). */
  outdated?: boolean;
  /** Installed by the person, on PATH, not in illogical's directory. */
  on_path?: boolean;
  /** Why it can't start, when it can't. */
  why?: string;
  /** `/api/setup` only: its agent's CLI is on this machine (#335). */
  found?: boolean;
  /** The Node found, when it's too old. */
  node?: string | null;
  node_major: number;
}

/** Its state in a few words, for a checklist line. */
export function adapterLine(a: Adapter): string {
  if (a.state === "no_node") return a.why ?? `${a.label}'s adapter needs Node ${a.node_major}+`;
  if (a.state === "missing") return `${a.label}'s adapter isn't installed`;
  if (a.outdated) return `${a.label}'s adapter ${a.version}: out of date (illogical uses ${a.pinned})`;
  return a.on_path ? `${a.label}'s adapter, on PATH` : `${a.label}'s adapter ${a.version ?? a.pinned}`;
}

/** Nothing to install: there, at the pin. */
export function adapterReady(a: Adapter): boolean {
  return a.state === "installed" && !a.outdated;
}

/** The adapters' status, or null if this person can't ask (a guest). */
export async function loadAdapters(client: Client): Promise<Adapter[] | null> {
  try {
    const res = await client.request("GET", "/api/agents/adapters");
    if (!res.ok) return null;
    return (await res.json<{ adapters: Adapter[] }>()).adapters;
  } catch {
    return null;
  }
}

/** Run its install in a new pane: beside `split`, else a tab in `session`. */
export async function installAdapter(client: Client, kind: string, where: { split?: PaneId; session?: number; from?: PaneId }) {
  const err = await client.make(`/api/agents/adapters/${kind}/install`, {
    split: where.split ?? null,
    from_pane: where.from ?? where.split ?? null,
    session: where.split === undefined && where.from === undefined ? (where.session?.toString() ?? null) : null,
  });
  if (err) client.toast(err);
}

/**
 * What to do about `a`, or nothing when it's installed. `said`: the
 * headline is shown already (the block's error), so only how to fix it.
 */
export function AdapterHelp({ client, a, said, install, then }: { client: Client; a: Adapter; said?: boolean; install: () => void; then?: string }) {
  if (a.state === "installed" && !a.outdated) return null;
  const owner = !client.state?.roles;
  if (a.state === "installed")
    return (
      <div class="adapter-help" data-adapter="outdated">
        <p>
          {a.label}'s adapter is {a.version}; this illogical uses {a.pinned}. Update it with:
        </p>
        <CopyText text={a.npm} data-adapter-npm />
        {owner && (
          <p class="adapter-install">
            <button type="button" data-adapter-install onClick={install}>
              Update
            </button>{" "}
            runs it in a new pane{then ? `; ${then}` : "."}
          </p>
        )}
      </div>
    );
  if (a.state === "no_node")
    return (
      <div class="adapter-help" data-adapter={a.state}>
        <p>
          {said ? "Install Node" : `Needs Node ${a.node_major}+${a.node ? ` (this machine has ${a.node})` : ""}. Install it`} from{" "}
          <a href="https://nodejs.org/en/download" target="_blank" rel="noreferrer">
            nodejs.org
          </a>
          , or with mise:
        </p>
        <CopyText text="mise use -g node@22" />
        <p>Then the adapter:</p>
        <CopyText text={a.npm} data-adapter-npm />
      </div>
    );
  return (
    <div class="adapter-help" data-adapter={a.state}>
      <p>{said ? "Install it with:" : `${a.label}'s adapter isn't installed. Install it with:`}</p>
      <CopyText text={a.npm} data-adapter-npm />
      {owner && (
        <p class="adapter-install">
          <button type="button" data-adapter-install onClick={install}>
            Install
          </button>{" "}
          runs it in a new pane{then ? `; ${then}` : "."}
        </p>
      )}
    </div>
  );
}
