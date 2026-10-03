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

import { render } from "preact";
import { useEffect, useRef, useState } from "preact/hooks";
import type { Client } from "../client";
import type { PaneId } from "../proto";
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
  gates: unknown[];
}

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
  const [error, setError] = useState<string | null>(null);
  const [entered, setEntered] = useState(false);
  // Bumped to enter again (↻, or another client's `reload`).
  const [want, setWant] = useState<{ n: number; to?: string }>({ n: 0 });
  const owner = !client.state?.roles;

  const enter = async (to?: string) => {
    setError(null);
    try {
      const res = await client.request("POST", `/api/blocks/${id}/call/enter`, to ? { to } : {});
      const v = await res.json<{ url?: string; error?: string }>();
      if (!res.ok || !v.url) throw new Error(v.error ?? `couldn't get in (${res.status})`);
      setEntered(true);
      // Into the frame, and nowhere else: not its src, not the URL bar.
      const go = (f: HTMLIFrameElement) => {
        try {
          f.contentWindow!.location.replace(v.url!);
        } catch {
          f.src = v.url!;
        }
      };
      if (frame.current) go(frame.current);
      else requestAnimationFrame(() => frame.current && go(frame.current));
    } catch (e) {
      setError((e as Error).message);
    }
  };

  useEffect(() => {
    if (s && owner) void enter(want.to);
  }, [!!s, owner, want.n, s?.reloads]);

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
          {!entered && <div class="browser-card dim">Opening {name}…</div>}
          {/* The box's own origin, never the app's. */}
          <iframe
            ref={frame}
            class="browser-frame"
            style={entered ? undefined : { display: "none" }}
            src={s.box_url + "/"}
            title={name}
            sandbox="allow-scripts allow-forms allow-same-origin allow-popups"
            referrerpolicy="no-referrer"
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
