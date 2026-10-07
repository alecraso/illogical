// A newer release is out (#176): a small chip in the top bar, and either
// an Update now button (the daemon updates itself, #391:
// crates/daemon/src/selfupdate.rs) or the command that updates this
// install. The daemon checks, at most twice a day (`GET /api/update`,
// crates/daemon/src/update.rs).

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
  /** The daemon can update itself: `POST /api/update/apply`. */
  apply?: boolean;
  applying?: Applying;
};

/** An update under way, or the one that failed. */
type Applying = {
  to: string;
  stage: "downloading" | "installing" | "restarting" | "failed";
  error?: string;
};

const DISMISS_KEY = "arugula.update.dismissed";
/** Ask the daemon again this often (it checks GitHub far less). */
const POLL_MS = 60 * 60 * 1000;
const SOON_MS = 30 * 1000;
/** While updating: ask this often, and give up on the restart after this. */
const APPLY_POLL_MS = 1000;
const APPLY_GIVE_UP_MS = 3 * 60 * 1000;

function dismissed(): string | null {
  try {
    return localStorage.getItem(DISMISS_KEY);
  } catch {
    return null;
  }
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
      <button class="update-chip" title={`Arugula ${latest} is out (this is ${status.current})`} data-update-chip onClick={() => setOpen(!open)}>
        Update {latest}
      </button>
      {open && (
        <div class="update-pop" role="dialog" aria-label="Update Arugula" data-update>
          <p>
            <b>arugula {latest}</b> is out; this daemon is {status.current}. Panes keep running while it restarts.
          </p>
          {status.apply ? <UpdateNow status={status} /> : <How status={status} />}
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
  if (status.command)
    return (
      <p>
        Run this in a terminal on this machine:
        <CopyText text={status.command} data-update-command />
      </p>
    );
  return <p>Build it from the new release's source, then run <code>arugulad install</code> again.</p>;
}

const STAGES: Record<Applying["stage"], string> = {
  downloading: "Downloading and checking it…",
  installing: "Installing it…",
  restarting: "Restarting the daemon…",
  failed: "The update failed.",
};

async function getStatus(): Promise<UpdateStatus | null> {
  return fetch("/api/update", { cache: "no-store" })
    .then((r) => (r.ok ? (r.json() as Promise<UpdateStatus>) : null))
    .catch(() => null);
}

/** The daemon updates itself, then the page reloads onto the new one. */
function UpdateNow({ status }: { status: UpdateStatus }) {
  const [applying, setApplying] = useState<Applying | undefined>(
    status.applying?.stage === "failed" ? undefined : status.applying,
  );
  const [error, setError] = useState(status.applying?.error);
  // Follow it once started: through the restart (when the daemon doesn't
  // answer) until the new version does, then reload for its web client.
  useEffect(() => {
    if (!applying) return;
    const to = applying.to;
    const since = Date.now();
    let t: number | undefined;
    let live = true;
    const look = async () => {
      const s = await getStatus();
      if (!live) return;
      if (s?.current === to) return window.location.reload();
      if (s?.applying?.stage === "failed") {
        setError(s.applying.error ?? "The update failed.");
        return setApplying(undefined);
      }
      if (s?.applying) setApplying(s.applying);
      else if (!s) setApplying((a) => a && { ...a, stage: "restarting" });
      if (Date.now() - since > APPLY_GIVE_UP_MS) {
        setError(`The daemon hasn't come back as ${to}. Its log says why; the command below updates it too.`);
        return setApplying(undefined);
      }
      t = window.setTimeout(look, APPLY_POLL_MS);
    };
    t = window.setTimeout(look, APPLY_POLL_MS);
    return () => {
      live = false;
      clearTimeout(t);
    };
  }, [applying?.to]);
  const start = async () => {
    setError(undefined);
    const r = await fetch("/api/update/apply", { method: "POST" }).catch(() => null);
    if (!r) return setError("The daemon didn't answer.");
    if (!r.ok) return setError((await r.text().catch(() => "")) || `The daemon said ${r.status}.`);
    setApplying((await r.json()) as Applying);
  };
  if (applying)
    return (
      <p data-update-progress aria-live="polite">
        {STAGES[applying.stage]}
      </p>
    );
  return (
    <>
      <p>
        <button class="update-now" onClick={start} data-update-now>
          Update now
        </button>{" "}
        It downloads {status.latest}, checks it against the release's checksums, and restarts the daemon. This page
        reloads when it's back.
      </p>
      {error && (
        <>
          <p class="update-error" role="alert" data-update-error>
            {error}
          </p>
          <How status={status} />
        </>
      )}
    </>
  );
}
