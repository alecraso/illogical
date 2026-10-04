// A newer release is out (#176): a small chip in the top bar, and the
// command that updates this install. The daemon checks, at most twice a
// day (`GET /api/update`, crates/daemon/src/update.rs).

import { useEffect, useState } from "preact/hooks";
import type { Client } from "../client";
import { CopyText } from "./copy";

/** `GET /api/update`. */
export type UpdateStatus = {
  current: string;
  latest?: string;
  newer: boolean;
  enabled: boolean;
  kind: "script" | "brew" | "app" | "source";
  command?: string;
  url: string;
};

const DISMISS_KEY = "illogical.update.dismissed";
/** Ask the daemon again this often (it checks GitHub far less). */
const POLL_MS = 60 * 60 * 1000;
const SOON_MS = 30 * 1000;

function dismissed(): string | null {
  try {
    return localStorage.getItem(DISMISS_KEY);
  } catch {
    return null;
  }
}

/** The desktop app's window says so (crates/desktop/src/cloud.rs). */
function inApp(): boolean {
  return !!(window as { __illogicalApp?: unknown }).__illogicalApp;
}

export function UpdateChip({ client }: { client: Client }) {
  const [status, setStatus] = useState<UpdateStatus | null>(null);
  const [open, setOpen] = useState(false);
  const [gone, setGone] = useState(dismissed);
  // The owner's own daemon only: not a guest, not a page from control.
  const mine = !client.e2e && !client.state?.roles;
  useEffect(() => {
    if (!mine) return;
    let t: number | undefined;
    let live = true;
    const get = async () => {
      const s = await fetch("/api/update")
        .then((r) => (r.ok ? (r.json() as Promise<UpdateStatus>) : null))
        .catch(() => null);
      if (!live) return;
      setStatus(s);
      // A daemon that just started checks in a few seconds: look again soon.
      t = window.setTimeout(get, s?.enabled && !s.latest ? SOON_MS : POLL_MS);
    };
    void get();
    return () => {
      live = false;
      clearTimeout(t);
    };
  }, [mine]);
  if (!mine || !status?.newer || !status.latest || gone === status.latest) return null;
  const latest = status.latest;
  const dismiss = () => {
    try {
      localStorage.setItem(DISMISS_KEY, latest);
    } catch {
      // Shown again next time; harmless.
    }
    setGone(latest);
  };
  return (
    <div class="update">
      <button class="update-chip" title={`illogical ${latest} is out (this is ${status.current})`} data-update-chip onClick={() => setOpen(!open)}>
        Update {latest}
      </button>
      {open && (
        <div class="update-pop" role="dialog" aria-label="Update illogical" data-update>
          <p>
            <b>illogical {latest}</b> is out; this daemon is {status.current}. Panes keep running while it restarts.
          </p>
          <How status={status} />
          <div class="update-actions">
            <a href={status.url} target="_blank" rel="noreferrer">
              What's new
            </a>
            <button onClick={dismiss} data-update-dismiss>
              Not now
            </button>
          </div>
        </div>
      )}
    </div>
  );
}

function How({ status }: { status: UpdateStatus }) {
  if (inApp() || status.kind === "app")
    return (
      <p>
        Download the new app from the release page and open it: it updates the daemon itself.
        {status.command && (
          <>
            {" "}
            Or, in a terminal: <CopyText text={status.command} data-update-command />
          </>
        )}
      </p>
    );
  if (status.command)
    return (
      <p>
        Run this in a terminal on this machine:
        <CopyText text={status.command} data-update-command />
      </p>
    );
  return <p>Build it from the new release's source, then run <code>illogicald install</code> again.</p>;
}
