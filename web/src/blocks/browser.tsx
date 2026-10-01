// Browser blocks (M6a): a web page in a frame, or, for sites that refuse to
// be framed, a card that opens them in a new tab.

import { render } from "preact";
import type { Client } from "../client";
import type { PaneId } from "../proto";
import { registerBlock, type BlockView } from "./view";

export interface BrowserState {
  url: string;
  title: string | null;
  framable: boolean | null;
  error: string | null;
  loading: boolean;
  reloads: number;
  back: string[];
}

function BrowserBlock({ client, id, s }: { client: Client; id: PaneId; s: BrowserState | null }) {
  if (!s) return <div class="browser-card">…</div>;
  const call = (method: string, args: unknown = {}) => void client.api(`/api/blocks/${id}/call/${method}`, args);
  const host = (() => {
    try {
      return new URL(s.url).host;
    } catch {
      return s.url;
    }
  })();
  return (
    <div class="browser">
      <div class="browser-bar">
        <button title="Back" disabled={s.back.length === 0} onClick={() => call("back")}>
          ←
        </button>
        <button title="Reload" onClick={() => call("reload")}>
          ↻
        </button>
        <form
          class="browser-url"
          onSubmit={(e) => {
            e.preventDefault();
            const v = (e.currentTarget.elements.namedItem("url") as HTMLInputElement).value;
            call("navigate", { url: v });
          }}
        >
          <input name="url" value={s.url} spellcheck={false} autocomplete="off" />
        </form>
        <a class="browser-open" href={s.url} target="_blank" rel="noopener noreferrer" title="Open in a new tab">
          ↗
        </a>
      </div>
      {s.error ? (
        <div class="browser-card error">
          <p>Couldn't load {host}</p>
          <p class="dim">{s.error}</p>
          <button onClick={() => call("reload")}>Try again</button>
        </div>
      ) : s.framable === false ? (
        <div class="browser-card">
          <p>{s.title ?? host} doesn't allow being shown inside another page.</p>
          <a class="button" href={s.url} target="_blank" rel="noopener noreferrer">
            Open in new tab
          </a>
        </div>
      ) : s.framable === null ? (
        <div class="browser-card dim">Loading {host}…</div>
      ) : (
        // A page from elsewhere: its own origin, never the app's.
        <iframe
          key={`${s.url}#${s.reloads}`}
          class="browser-frame"
          src={s.url}
          title={s.title ?? s.url}
          sandbox="allow-scripts allow-forms allow-same-origin allow-popups"
          referrerpolicy="no-referrer"
        />
      )}
    </div>
  );
}

registerBlock("browser", (client, id): BlockView => {
  const host = document.createElement("div");
  host.className = "block block-browser";
  let state: BrowserState | null = null;
  const draw = () => render(<BrowserBlock client={client} id={id} s={state} />, host);
  draw();
  return {
    host,
    setVisible: (v) => (host.style.display = v ? "" : "none"),
    update: (s) => {
      state = s as BrowserState;
      draw();
    },
    title: () => state?.title ?? state?.url ?? "browser",
    text: () => (state ? `${state.title ?? ""}\n${state.url}`.trim() : ""),
    focus: () => host.querySelector<HTMLElement>("iframe, input")?.focus(),
    dispose: () => {
      render(null, host);
      host.remove();
    },
  };
});
