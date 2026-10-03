// Getting started (#110): what isn't on the first screen. Opens by itself
// once per browser, then from the session menu and the phone's sheet.
// Each section has its command to copy, and says so when the daemon can
// tell it's done.

import { useEffect, useState } from "preact/hooks";
import type { Client } from "../client";
import { CopyText } from "./copy";
import { notifyBlocker, pushNow, serveCommand, subscribePush } from "./notify";
import { startAgent } from "./agent-dialog";

const DOCS = "https://git.inevitable.fyi/jhgaylor/illogical/src/branch/main/docs";
const CONTROL = "https://control.illogical.widgets.wtf";
const SEEN_KEY = "illogical.getting-started";

/** What `/api/host` says about this daemon (the parts for this panel). */
interface HostInfo {
  name: string;
  tailnet_url?: string;
  tailnet_seen?: boolean;
  control?: string;
  team?: string;
}

type Section = "phone";

let shown: { client: Client | null; section?: Section } | null = null;
const listeners = new Set<() => void>();
const changed = () => listeners.forEach((fn) => fn());
let lastClient: Client | null = null;

export function openGettingStarted(section?: Section, client?: Client) {
  shown = { client: client ?? lastClient, section };
  changed();
}

function seen(): boolean {
  try {
    return localStorage.getItem(SEEN_KEY) !== null;
  } catch {
    // No storage (a private window, blocked): don't greet every time.
    return true;
  }
}

function markSeen() {
  try {
    localStorage.setItem(SEEN_KEY, "seen");
  } catch {
    // Nothing to remember it in.
  }
}

/** Open it the first time this browser sees its own daemon: not on a page
 * from control, not for a guest, and not under automation (tests expect a
 * clean first screen; welcome.spec.ts turns it back on). */
export function useFirstRun(client: Client) {
  lastClient = client;
  const ready = !!client.state && client.state.sessions.length > 0;
  useEffect(() => {
    if (!ready || client.e2e || client.state?.roles || navigator.webdriver || seen()) return;
    markSeen();
    openGettingStarted(undefined, client);
  }, [client, ready]);
}

export function GettingStartedLayer() {
  const [, setTick] = useState(0);
  useEffect(() => {
    const fn = () => setTick((t) => t + 1);
    listeners.add(fn);
    return () => void listeners.delete(fn);
  }, []);
  if (!shown) return null;
  return (
    <GettingStarted
      client={shown.client}
      section={shown.section}
      close={() => {
        markSeen();
        shown = null;
        changed();
      }}
    />
  );
}

function GettingStarted({ client, section, close }: { client: Client | null; section?: Section; close: () => void }) {
  const [host, setHost] = useState<HostInfo | null>(null);
  const [, setTick] = useState(0);
  useEffect(() => subscribePush(() => setTick((t) => t + 1)), []);
  useEffect(() => {
    fetch("/api/host")
      .then((r) => (r.ok ? (r.json() as Promise<HostInfo>) : null))
      .then(setHost)
      .catch(() => {});
  }, []);
  useEffect(() => {
    if (section) document.querySelector(`[data-start="${section}"]`)?.scrollIntoView({ block: "start" });
  }, [section]);
  const session = client?.session ?? null;
  const blocked = notifyBlocker();
  return (
    <div class="prompt-backdrop" role="dialog" aria-label="Getting started" onPointerDown={(e) => e.target === e.currentTarget && close()}>
      <div class="prompt getting-started" data-getting-started onKeyDown={(e) => e.key === "Escape" && close()}>
        <h2>Getting started</h2>
        <div class="start-sections">
          <section data-start="menus">
            <h3>Right-click anything</h3>
            <p>Tabs, panes and the tab bar have menus; that's where everything is. Drag a tab onto a pane's edge to split it there.</p>
          </section>

          <section data-start="phone">
            <h3>
              On your phone
              {host?.tailnet_seen && <span class="start-done"> ✓ reached over the tailnet</span>}
            </h3>
            <p>Put it on your tailnet with Tailscale, on this machine:</p>
            <CopyText text={serveCommand()} data-serve-command />
            {host?.tailnet_url ? (
              <>
                <p>Then open it on the phone (signed in to Tailscale as this machine's owner):</p>
                <CopyText text={host.tailnet_url} share data-tailnet-url />
              </>
            ) : (
              <p>
                Then open <code>https://&lt;this machine&gt;.&lt;tailnet&gt;.ts.net</code> on the phone (<code>tailscale status</code> shows the
                name).
              </p>
            )}
            <details>
              <summary>The first time</summary>
              <ul>
                <li>
                  Turn on MagicDNS and HTTPS certificates in the{" "}
                  <a href="https://login.tailscale.com/admin/dns" target="_blank" rel="noreferrer">
                    Tailscale admin console
                  </a>
                  .
                </li>
                <li>
                  On Linux, let yourself run serve without sudo, once: <CopyText inline text="sudo tailscale set --operator=$USER" />
                </li>
                <li>
                  On macOS, the CLI is in the app: <code>/Applications/Tailscale.app/Contents/MacOS/Tailscale</code>
                </li>
              </ul>
            </details>
            <p>
              On the phone, add it to the Home Screen, open it from there, and turn on <em>Notify this device</em> in the menu (☰).
              {blocked && pushNow() !== "insecure" && <span class="dim"> This device: {blocked}.</span>}
            </p>
          </section>

          <section data-start="control">
            <h3>
              From anywhere, or with your team
              {host?.control && (
                <span class="start-done" data-start-joined>
                  {" "}
                  ✓ joined{host.team ? ` to ${host.team}` : ""}
                </span>
              )}
            </h3>
            <p>No tailnet, or other people: add this machine to an account on illogical control and use it from any browser.</p>
            <CopyText text={`illogicald join ${CONTROL}`} data-join-command />
            <p class="dim">
              When you approve it, you choose: your own, or one of your teams'. <code>--team NAME</code> picks the team ahead of time.
            </p>
          </section>

          <section data-start="agents">
            <h3>Agents</h3>
            <p>
              Claude and Codex run as agent blocks: they ask on cards, and you answer from here or the phone.{" "}
              {client && session !== null && (
                <button
                  class="control-linkish"
                  data-start-agent
                  onClick={() => {
                    close();
                    startAgent(client, { session, from: client.active() });
                  }}
                >
                  Start an agent…
                </button>
              )}
            </p>
          </section>

          <section data-start="claude-code">
            <h3>Claude Code</h3>
            <p>Give it illogical's tools: builds and servers in panes you can watch and take over.</p>
            <CopyText text="claude mcp add illogical -- illogical mcp" data-mcp-command />
            <p>
              <a href={`${DOCS}/cli.md#mcp`} target="_blank" rel="noreferrer">
                Which tools to allow
              </a>
              , and{" "}
              <a href={`${DOCS}/cli.md#claude-code-in-a-pane`} target="_blank" rel="noreferrer">
                Claude Code in a pane
              </a>
              : its questions and permission prompts on cards.
            </p>
          </section>

          <section data-start="terminal">
            <h3>In a terminal</h3>
            <p>The same tabs and splits, over ssh too. Ctrl-] is the menu key.</p>
            <CopyText text="illogical tui" />
          </section>
        </div>
        <div class="prompt-buttons">
          <button class="primary" onClick={close}>
            Close
          </button>
        </div>
      </div>
    </div>
  );
}
