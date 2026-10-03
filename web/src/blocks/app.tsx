// Studio app blocks (M35): a studio box in a frame, on its own origin.
//
// Getting in: the frame asks the daemon for a way in (the block's `enter`
// method), which mints a fresh ten-minute entry link from studio, and
// navigates the frame to it. The link lives only in this call: it isn't
// the frame's `src` (that's the box itself, so a frame that reloads, or is
// moved, comes back through the cookie the door set), and it's never kept.
// A cross-site frame's 401 can't be seen from here, so a client enters each
// time it draws the block anew, and ↻ enters again. Only the owner may
// enter; anyone else sees a card.
//
// The frame shows once a page has loaded in it after entering. A box that
// takes the connection and never answers (asleep, or hung) leaves a frame
// loading forever, which draws as a blank white page: after `STUCK_AFTER`
// the card says so instead, and the frame comes in if the box answers late.

import { render } from "preact";
import { useEffect, useRef, useState } from "preact/hooks";
import type { Client } from "../client";
import { gateKey, type Gate, type PaneId } from "../proto";
import { openMenu } from "../ui/menu";
import { registerBlock, type BlockView } from "./view";

export interface AppState {
  app: string;
  title: string | null;
  box_url: string;
  studio: string;
  follower_credential: boolean;
  follower: {
    state: string;
    error: string | null;
    tabs: number;
    questions: number;
    last_answer: { ok: boolean; error?: string } | null;
  };
  reloads: number;
  /** Gates waiting in the box, from hud's work board. */
  gates: Gate[];
  /** The last approve that failed: the gate's key, and why. */
  gate_error: [string, string] | null;
}

/** How long entering may take before the card says the box isn't answering. */
const STUCK_AFTER = 15_000;

/** hud's own pages, for the frame. */
const PAGES: [string, string][] = [
  ["Decisions", "/__hud/decisions"],
  ["Work", "/__hud/work"],
  ["Intent", "/__hud/intent"],
  ["Sessions", "/__hud/sessions"],
];

function hostOf(url: string): string {
  try {
    return new URL(url).host;
  } catch {
    return url;
  }
}

function followerLine(s: AppState): string {
  const f = s.follower;
  if (f.state === "error") return `hud: ${f.error ?? "can't follow"}`;
  if (f.state !== "following") return "hud: getting in…";
  const q = f.questions === 1 ? "1 question" : `${f.questions} questions`;
  return `hud: ${f.tabs} tab${f.tabs === 1 ? "" : "s"}, ${q}`;
}

function AppBlock({ client, id, s }: { client: Client; id: PaneId; s: AppState | null }) {
  const frame = useRef<HTMLIFrameElement>(null);
  // A way in waiting for the frame's own first load (the box's page), so
  // that load can't land on top of it.
  const pending = useRef<string | null>(null);
  const loaded = useRef(false);
  // Whether a load in the frame is from entering, not the box's first page.
  const sent = useRef(false);
  const go = (url: string) => {
    const f = frame.current;
    if (!f || !loaded.current) {
      pending.current = url;
      return;
    }
    pending.current = null;
    sent.current = true;
    try {
      f.contentWindow!.location.replace(url);
    } catch {
      f.src = url;
    }
  };
  const [error, setError] = useState<string | null>(null);
  // opening: no page from entering yet; in: one loaded; stuck: none came in
  // time.
  const [phase, setPhase] = useState<"opening" | "in" | "stuck">("opening");
  const stuck = useRef<number | null>(null);
  // Bumped to enter again (↻, or another client's `reload`).
  const [want, setWant] = useState<{ n: number; to?: string }>({ n: 0 });
  const owner = !client.state?.roles;
  const [busy, setBusy] = useState<string | null>(null);
  const may = client.role(client.sessionOfTab(client.tabOfPane(id)?.id ?? -1) ?? null) !== "viewer";
  const approve = async (g: Gate) => {
    setBusy(gateKey(g));
    // The card goes when hud's board no longer lists the gate; a failure
    // comes back on it (and as a toast).
    await client.act({ action: "allow", pane: id, id: gateKey(g) });
    setBusy(null);
  };

  const enter = async (to?: string) => {
    setError(null);
    setPhase((p) => (p === "stuck" ? "opening" : p));
    try {
      const res = await client.request("POST", `/api/blocks/${id}/call/enter`, to ? { to } : {});
      const v = await res.json<{ url?: string; error?: string }>();
      if (!res.ok || !v.url) throw new Error(v.error ?? `couldn't get in (${res.status})`);
      // Into the frame, and nowhere else: not its src, not the URL bar.
      go(v.url);
      // Entering again over a page that's in keeps it showing until then.
      if (stuck.current !== null) clearTimeout(stuck.current);
      stuck.current = window.setTimeout(() => {
        stuck.current = null;
        setPhase("stuck");
      }, STUCK_AFTER);
    } catch (e) {
      setError((e as Error).message);
    }
  };

  useEffect(() => {
    if (s && owner) void enter(want.to);
  }, [!!s, owner, want.n, s?.reloads]);
  useEffect(
    () => () => {
      if (stuck.current !== null) clearTimeout(stuck.current);
    },
    [],
  );

  if (!s) return <div class="browser-card">…</div>;
  const name = s.title ?? s.app;
  return (
    <div class="browser app-block" data-app={s.app}>
      <div class="browser-bar">
        <button title="Enter again (a fresh way in from the studio)" disabled={!owner} onClick={() => setWant((w) => ({ n: w.n + 1 }))}>
          ↻
        </button>
        <span class="app-name" title={s.box_url}>
          {name}
        </span>
        <span class="dim app-host">{hostOf(s.box_url)}</span>
        <span class={`app-follower ${s.follower.state}`} data-follower={s.follower.state} title={s.follower.error ?? undefined}>
          {followerLine(s)}
        </span>
        <button
          class="link"
          disabled={!owner}
          title="hud's pages for this box"
          onClick={(e) =>
            openMenu(
              e,
              PAGES.map(([label, to]) => ({ label, run: () => setWant((w) => ({ n: w.n + 1, to })) })),
            )
          }
        >
          Records ▾
        </button>
        <a class="browser-open" href={s.box_url} target="_blank" rel="noopener noreferrer" title="Open the box in a new tab">
          ↗
        </a>
      </div>
      {s.gates.length > 0 && (
        <div class="app-gates" data-app-gates>
          {s.gates.map((g) => (
            <div class="app-gate" key={gateKey(g)} data-gate={gateKey(g)}>
              <span>
                <b>{g.member}</b>: {g.op} waits at gate <b>{g.gate}</b>
                {g.env ? ` in ${g.env}` : ""}
                {g.needed > 1 ? ` (${g.approvals} of ${g.needed})` : ""}
              </span>
              {s.gate_error?.[0] === gateKey(g) && <span class="error app-gate-error">{s.gate_error[1]}</span>}
              {may && (
                <button class="primary" disabled={busy === gateKey(g)} onClick={() => void approve(g)}>
                  {busy === gateKey(g) ? "Approving…" : "Approve"}
                </button>
              )}
            </div>
          ))}
        </div>
      )}
      {!owner ? (
        <div class="browser-card">
          <p>{name} is a studio app of this machine's owner.</p>
          <p class="dim">Only they can open it here; its questions still come to you if you may answer them.</p>
        </div>
      ) : error ? (
        <div class="browser-card error">
          <p>Couldn't get into {name}</p>
          <p class="dim">{error}</p>
          <button onClick={() => setWant((w) => ({ n: w.n + 1 }))}>Try again</button>
        </div>
      ) : (
        <>
          {phase === "opening" && <div class="browser-card dim">Opening {name}…</div>}
          {phase === "stuck" && (
            <div class="browser-card error" data-app-stuck>
              <p>{name} isn't answering</p>
              <p class="dim">
                {hostOf(s.box_url)} hasn't sent a page in {STUCK_AFTER / 1000} seconds
                {s.follower.state === "error" && s.follower.error ? ` (${s.follower.error})` : ""}. It shows here if it answers.
              </p>
              <button onClick={() => setWant((w) => ({ n: w.n + 1 }))}>Try again</button>
            </div>
          )}
          {/* The box's own origin, never the app's. */}
          <iframe
            ref={frame}
            class="browser-frame"
            style={phase === "in" ? undefined : { display: "none" }}
            src={s.box_url + "/"}
            title={name}
            sandbox="allow-scripts allow-forms allow-same-origin allow-popups"
            referrerpolicy="no-referrer"
            onLoad={() => {
              loaded.current = true;
              if (pending.current) go(pending.current);
              else if (sent.current) {
                if (stuck.current !== null) clearTimeout(stuck.current);
                stuck.current = null;
                setPhase("in");
              }
            }}
          />
        </>
      )}
    </div>
  );
}

registerBlock("app", (client, id): BlockView => {
  const host = document.createElement("div");
  host.className = "block block-browser block-app";
  let state: AppState | null = null;
  const draw = () => render(<AppBlock client={client} id={id} s={state} />, host);
  draw();
  return {
    host,
    setVisible: (v) => (host.style.display = v ? "" : "none"),
    update: (s) => {
      state = s as AppState;
      draw();
    },
    title: () => state?.title ?? state?.app ?? "app",
    text: () => (state ? `${state.title ?? state.app}\n${state.box_url}` : ""),
    focus: () => host.querySelector<HTMLElement>("iframe")?.focus(),
    dispose: () => {
      render(null, host);
      host.remove();
    },
  };
});
